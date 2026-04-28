
use crate::{
    authorship::{
        transcript::{AiTranscript, Message},
        working_log::{AgentId, CheckpointKind},
    },
    commands::checkpoint_agent::bash_tool::{
        self, Agent, BashCheckpointAction, HookEvent, ToolClass,
    },
    commands::checkpoint_agent::{
        agent_presets::{
            AgentCheckpointFlags, AgentCheckpointPreset, AgentRunResult, 
            BashPreHookStrategy, extract_plan_from_tool_use, prepare_agent_bash_pre_hook
        },
    },
    error::GitAiError,
    git::repository::find_repository_for_file,
};
use dirs;
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};


const CURSOR_HOOK_PRE_TOOL_USE: &str = "preToolUse";
const CURSOR_HOOK_POST_TOOL_USE: &str = "postToolUse";

#[derive(Debug, Deserialize)]
struct CursorHookInput {
    #[serde(alias = "conversationId")]
    conversation_id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(alias = "hookEventName")]
    #[serde(default)]
    hook_event_name: Option<String>,
    #[serde(default)]
    workspace_roots: Option<Vec<String>>,
    
    // preToolUse and postToolUse common input fields
    tool_name: Option<String>,
    tool_input: Option<serde_json::Value>,
    
    // additional fields
    #[serde(flatten)]
    _extra: HashMap<String, serde_json::Value>,
}

/// Result of reading transcript + model from Cursor's global SQLite composer state.
#[derive(Debug)]
pub enum CursorSqliteTranscriptOutcome {
    /// Parsed bubbles yielded transcript text and a model name (when present in DB).
    Ready(AiTranscript, String),
    /// Composer row not persisted yet (hook race with Cursor).
    NoConversationRow,
    /// Composer JSON present but bubble extraction produced no messages.
    ExtractionEmpty,
}

pub struct CursorPreset;

impl AgentCheckpointPreset for CursorPreset {
    fn run(&self, flags: AgentCheckpointFlags) -> Result<AgentRunResult, GitAiError> {
        // Parse hook_input JSON
        let hook_input_json = flags.hook_input.ok_or_else(|| {
            GitAiError::PresetError("hook_input is required for Cursor preset".to_string())
        })?;

        let hook_input: CursorHookInput = serde_json::from_str(&hook_input_json)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;
        tracing::debug!("CursorPreset: hook_input={:?}", hook_input);

        let CursorHookInput {
            conversation_id,
            hook_event_name,
            model,
            workspace_roots,
            tool_name,
            tool_input,
            ..
        } = hook_input;

        let conversation_id = conversation_id.ok_or_else(|| {
            GitAiError::PresetError("conversation_id not found in hook_input".to_string())
        })?;

        let workspace_roots = workspace_roots.ok_or_else(|| {
            GitAiError::PresetError("workspace_roots not found in hook_input".to_string())
        })?;
        let workspace_roots = workspace_roots
            .iter()
            .filter_map(|root| {
                let trimmed = root.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(Self::normalize_cursor_path(trimmed))
                }
            })
            .collect::<Vec<String>>();

        let hook_event_name = hook_event_name
            .unwrap_or_default()
            .trim()
            .to_string();

        // Legacy hooks no longer installed; exit silently for existing users who haven't reinstalled.
        if hook_event_name == "beforeSubmitPrompt" || hook_event_name == "afterFileEdit" {
            tracing::debug!(
                "CursorPreset: legacy hook event '{}', exiting (reinstall hooks to fix)",
                hook_event_name
            );
            std::process::exit(0);
        }

        // Validate hook_event_name
        if hook_event_name != CURSOR_HOOK_PRE_TOOL_USE
            && hook_event_name != CURSOR_HOOK_POST_TOOL_USE
        {
            return Err(GitAiError::PresetError(format!(
                "Invalid hook_event_name: {}. Expected '{}' or '{}'",
                hook_event_name, CURSOR_HOOK_PRE_TOOL_USE, CURSOR_HOOK_POST_TOOL_USE,
            )));
        }

        let model = model
            .unwrap_or_default()
            .trim()
            .to_string();
        let model = if model.is_empty() {
            "unknown".to_string()
        } else {
            model
        };

        let tool_name = tool_name.unwrap_or_default();
        // Only checkpoint on file-mutating tools
        let tool_class = bash_tool::classify_tool(Agent::Cursor, tool_name.as_str());
        if tool_class == ToolClass::Skip {
            tracing::debug!(
                "CursorPreset: tool_name '{}' is not a file-mutating tool, skipping checkpoint",
                tool_name
            );
            std::process::exit(0);
        }
        let is_bash_tool = tool_class == ToolClass::Bash;

        // Absolute path of the file-to-edit, can be anything (e.g., None, file in the repo, file outside the repo)
        let file_path = Self::extract_file_path_from_tool_input(tool_input.as_ref())
            .map(|path| Self::normalize_cursor_path(&path))
            .unwrap_or_default();

        let repo_working_dir = Self::resolve_repo_working_dir(&file_path, &workspace_roots)
            .ok_or_else(|| {
                GitAiError::PresetError("No workspace root found in hook_input".to_string())
            })?;
        tracing::debug!("CursorPreset: resolved repo_working_dir={}", repo_working_dir);


        let session_id = conversation_id.clone();
        let agent_id = AgentId {
            tool: "cursor".to_string(),
            id: session_id.clone(),
            model: model.clone(),
        };

        let file_paths = if !file_path.is_empty() {
            Some(vec![file_path.clone()])
        } else {
            None
        };

        if hook_event_name == CURSOR_HOOK_PRE_TOOL_USE {
            let pre_hook_captured_id = prepare_agent_bash_pre_hook(
                is_bash_tool,
                Some(repo_working_dir.as_str()),
                &session_id,
                "bash",
                &agent_id,
                None,
                BashPreHookStrategy::EmitHumanCheckpoint,
            )?
            .captured_checkpoint_id();

            // early return, we're just adding a human checkpoint.
            return Ok(AgentRunResult {
                agent_id,
                agent_metadata: None,
                checkpoint_kind: CheckpointKind::Human,
                transcript: None,
                repo_working_dir: Some(repo_working_dir),
                edited_filepaths: None,
                will_edit_filepaths: file_paths,
                dirty_files: None,
                captured_checkpoint_id: pre_hook_captured_id,
            });
        }

        let bash_result = if is_bash_tool {
            Some(repo_working_dir.clone()).as_ref().map(|cwd| {
                bash_tool::handle_bash_tool(
                    HookEvent::PostToolUse,
                    Path::new(cwd),
                    &conversation_id,
                    "bash"
                )
            })
        } else {
            None
        };

        let edited_filepaths = if is_bash_tool {
            match bash_result
                .as_ref()
                .and_then(|r| r.as_ref().ok())
                .map(|r| &r.action)
            {
                Some(BashCheckpointAction::Checkpoint(paths)) => Some(paths.clone()),
                Some(BashCheckpointAction::NoChanges)
                | Some(BashCheckpointAction::TakePreSnapshot)
                | Some(BashCheckpointAction::Fallback)
                | None => None,
            }
        } else {
            file_paths
        };

        let bash_captured_checkpoint_id = bash_result
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .and_then(|r| r.captured_checkpoint.as_ref())
            .map(|info| info.capture_id.clone());

        // // Option 1: Read transcript from JSONL file if available
        // let transcript_path = hook_data
        //     .get("transcript_path")
        //     .and_then(|v| v.as_str())
        //     .map(|s| s.to_string());
    
        // let transcript = if let Some(ref tp) = transcript_path {
        //     match Self::transcript_and_model_from_cursor_jsonl(tp) {
        //         Ok((transcript, _)) => transcript,
        //         Err(e) => {
        //             eprintln!(
        //                 "[Warning] Failed to parse Cursor JSONL at {}: {}. Will retry at commit.",
        //                 tp, e
        //             );
        //             AiTranscript::new()
        //         }
        //     }
        // } else {
        //     eprintln!("[Warning] No transcript_path in Cursor hook input. Will retry at commit.");
        //     AiTranscript::new()
        // };

        // Option 2: Fetch the composer data and extract transcript from global db
        let global_db = Self::cursor_global_database_path()?;
        if !global_db.exists() {
            return Err(GitAiError::PresetError(format!(
                "Cursor global state database not found at {:?}. \
                Make sure Cursor is installed and has been used at least once. \
                Expected location: {:?}",
                global_db, global_db,
            )));
        }
        let transcript: AiTranscript = match Self::transcript_and_model_from_cursor_sqlite_db(
            &global_db,
            &conversation_id,
        )? {
            CursorSqliteTranscriptOutcome::Ready(transcript, _db_model) => transcript,
            CursorSqliteTranscriptOutcome::ExtractionEmpty => {
                // Gracefully continue when the messages haven't been written for the conversation yet due to Cursor race conditions
                // We refresh and grab all the messages in post-commit
                eprintln!(
                    "[Warning] Failed to extract transcript from composer data in Cursor DB. Will retry at commit."
                );
                tracing::warn!("CursorPreset: [Warning] Failed to extract transcript from composer data in Cursor DB. Will retry at commit.");
                AiTranscript::new()
            }
            CursorSqliteTranscriptOutcome::NoConversationRow => {
                // Gracefully continue when the conversation hasn't been written yet due to Cursor race conditions
                eprintln!(
                    "[Warning] No composer data for the conversation found in Cursor DB. Will retry at commit."
                );
                tracing::warn!("CursorPreset: [Warning] No composer data for the conversation found in Cursor DB. Will retry at commit.");
                AiTranscript::new()
            }
        };

        tracing::debug!("CursorPreset: transcript:\n{}", transcript);

        // // Store transcript_path in metadata for re-reading at commit time
        // let agent_metadata =
        //     transcript_path.map(|tp| HashMap::from([("transcript_path".to_string(), tp)]));

        Ok(AgentRunResult {
            agent_id,
            agent_metadata: None,
            checkpoint_kind: CheckpointKind::AiAgent,
            transcript: Some(transcript),
            repo_working_dir: Some(repo_working_dir.clone()),
            edited_filepaths,
            will_edit_filepaths: None,
            dirty_files: None,
            captured_checkpoint_id: bash_captured_checkpoint_id,
        })
    }

 
}

impl CursorPreset {
    fn extract_file_path_from_tool_input(tool_input: Option<&serde_json::Value>) -> Option<String> {
        // Looks at the optional tool_input object and returns the first non-empty string it finds 
        // among these keys, in order: file_path, then path, then target_file. If tool_input is 
        // missing or none of those keys have a usable string, returns None.
        let tool_input = tool_input?;
        for key in ["file_path", "path", "target_file"] {
            if let Some(path) = tool_input.get(key).and_then(|v| v.as_str()) {
                let trimmed = path.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
        None
    }

    fn matching_workspace_root(file_path: &str, workspace_roots: &[String]) -> Option<String> {
        workspace_roots
            .iter()
            .find(|root| {
                let root_str = root.as_str();
                file_path.starts_with(root_str)
                    && (file_path.len() == root_str.len()
                        || file_path[root_str.len()..].starts_with('/')
                        || file_path[root_str.len()..].starts_with('\\')
                        || root_str.ends_with('/')
                        || root_str.ends_with('\\'))
            })
            .cloned()
    }

    fn resolve_repo_working_dir(file_path: &str, workspace_roots: &[String]) -> Option<String> {
        if file_path.is_empty() {
            return workspace_roots.first().cloned();
        }

        let matched_workspace = Self::matching_workspace_root(file_path, workspace_roots)
            .or_else(|| workspace_roots.first().cloned())?;

        find_repository_for_file(file_path, Some(&matched_workspace))
            .ok()
            .and_then(|repo| repo.workdir().ok())
            .map(|path| path.to_string_lossy().to_string())
            .or(Some(matched_workspace))
    }

    /// Normalize Windows paths that Cursor sends in Unix-style format.
    ///
    /// On Windows, Cursor sometimes sends paths like `/c:/Users/...` instead of `C:\Users\...`.
    /// This function converts those paths to proper Windows format.
    #[cfg(windows)]
    fn normalize_cursor_path(path: &str) -> String {
        // Check for pattern like /c:/ or /C:/ at the start
        // e.g. "/c:/Users/foo" -> "C:\Users\foo"
        let mut chars = path.chars();
        if chars.next() == Some('/')
            && let (Some(drive), Some(':')) = (chars.next(), chars.next())
            && drive.is_ascii_alphabetic()
        {
            let rest: String = chars.collect();
            // Convert forward slashes to backslashes for Windows
            let normalized_rest = rest.replace('/', "\\");
            return format!("{}:{}", drive.to_ascii_uppercase(), normalized_rest);
        }
        // No conversion needed
        path.to_string()
    }

    #[cfg(not(windows))]
    fn normalize_cursor_path(path: &str) -> String {
        // On non-Windows platforms, no conversion needed
        path.to_string()
    }

    /// Fetch the latest version of a Cursor conversation from the database
    pub fn fetch_latest_cursor_conversation(
        conversation_id: &str,
    ) -> Result<Option<(AiTranscript, String)>, GitAiError> {
        let global_db = Self::cursor_global_database_path()?;
        Self::fetch_cursor_conversation_from_db(&global_db, conversation_id)
    }

      /// Fetch a Cursor conversation from a specific database path
      pub fn fetch_cursor_conversation_from_db(
        db_path: &std::path::Path,
        conversation_id: &str,
    ) -> Result<Option<(AiTranscript, String)>, GitAiError> {
        if !db_path.exists() {
            return Ok(None);
        }

        // Fetch composer payload
        let composer_payload = Self::fetch_composer_payload(db_path, conversation_id)?;

        // Extract transcript and model
        let transcript_data = Self::transcript_data_from_composer_payload(
            &composer_payload,
            db_path,
            conversation_id,
        )?;

        Ok(transcript_data)
    }

    /// Load composer payload from Cursor's global DB and parse transcript + model from bubble data.
    pub fn transcript_and_model_from_cursor_sqlite_db(
        global_db: &Path,
        conversation_id: &str,
    ) -> Result<CursorSqliteTranscriptOutcome, GitAiError> {
        let payload = match Self::fetch_composer_payload(global_db, conversation_id) {
            Ok(p) => p,
            Err(GitAiError::PresetError(msg))
                if msg == "No conversation data found in database" =>
            {
                return Ok(CursorSqliteTranscriptOutcome::NoConversationRow);
            }
            Err(e) => return Err(e),
        };

        match Self::transcript_data_from_composer_payload(
            &payload,
            global_db,
            conversation_id,
        )? {
            Some((transcript, model)) => Ok(CursorSqliteTranscriptOutcome::Ready(transcript, model)),
            None => Ok(CursorSqliteTranscriptOutcome::ExtractionEmpty),
        }
    }

    /// Parse a Cursor JSONL transcript file into a transcript.
    ///
    /// Cursor JSONL uses `role` (not `type`) at the top level, has no timestamps
    /// or model fields in entries, and wraps user text in `<user_query>` tags.
    /// Tool inputs use `path`/`contents` instead of `file_path`/`content`.
    pub fn transcript_and_model_from_cursor_jsonl(
        transcript_path: &str,
    ) -> Result<(AiTranscript, Option<String>), GitAiError> {
        let jsonl_content =
            std::fs::read_to_string(transcript_path).map_err(GitAiError::IoError)?;
        let mut transcript = AiTranscript::new();
        let mut plan_states = std::collections::HashMap::new();
        let mut message_seq: u64 = 0;
        let mut next_message_id = || {
            message_seq += 1;
            Some(format!("msg_{}", message_seq))
        };

        for line in jsonl_content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            // Skip malformed lines (file may be partially written)
            let raw_entry: serde_json::Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(_) => continue,
            };

            match raw_entry["role"].as_str() {
                Some("user") => {
                    if let Some(content_array) = raw_entry["message"]["content"].as_array() {
                        for item in content_array {
                            if item["type"].as_str() == Some("tool_result") {
                                tracing::debug!("CursorPreset: user msg, tool_result: {}", item["text"]);
                                continue;
                            }
                            if item["type"].as_str() == Some("text")
                                && let Some(text) = item["text"].as_str()
                            {
                                let cleaned = Self::strip_user_query_tags(text);
                                if !cleaned.is_empty() {
                                    transcript.add_message(Message::user_with_id(
                                        cleaned,
                                        None,
                                        next_message_id(),
                                    ));
                                }
                            }
                        }
                    }
                }
                Some("assistant") => {
                    if let Some(content_array) = raw_entry["message"]["content"].as_array() {
                        for item in content_array {
                            match item["type"].as_str() {
                                Some("text") => {
                                    if let Some(text) = item["text"].as_str()
                                        && !text.trim().is_empty()
                                    {
                                        transcript.add_message(Message::assistant_with_id(
                                            text.to_string(),
                                            None,
                                            next_message_id(),
                                        ));
                                    }
                                }
                                Some("thinking") => {
                                    if let Some(thinking) = item["thinking"].as_str()
                                        && !thinking.trim().is_empty()
                                    {
                                        transcript.add_message(Message::assistant_with_id(
                                            thinking.to_string(),
                                            None,
                                            next_message_id(),
                                        ));
                                    }
                                }
                                Some("tool_use") => {
                                    if let Some(name) = item["name"].as_str() {
                                        let input = &item["input"];
                                        // Normalize tool input: Cursor uses `path` where git-ai uses `file_path`
                                        let normalized_input =
                                            Self::normalize_cursor_tool_input(name, input);

                                        // Check for plan file writes
                                        if let Some(plan_text) = extract_plan_from_tool_use(
                                            name,
                                            &normalized_input,
                                            &mut plan_states,
                                        ) {
                                            transcript.add_message(Message::Plan {
                                                text: plan_text,
                                                timestamp: None,
                                                id: next_message_id(),
                                            });
                                        } else {
                                            // Apply same tool filtering as SQLite path
                                            Self::add_cursor_tool_message(
                                                &mut transcript,
                                                name,
                                                &normalized_input,
                                                next_message_id(),
                                            );
                                        }
                                    }
                                }
                                _ => continue,
                            }
                        }
                    }
                }
                _ => continue,
            }
        }

        // Model is not in Cursor JSONL — it comes from hook input
        Ok((transcript, None))
    }

    /// Strip `<user_query>...</user_query>` wrapper tags from Cursor user messages.
    fn strip_user_query_tags(text: &str) -> String {
        let trimmed = text.trim();
        if let Some(inner) = trimmed
            .strip_prefix("<user_query>")
            .and_then(|s| s.strip_suffix("</user_query>"))
        {
            inner.trim().to_string()
        } else {
            trimmed.to_string()
        }
    }

    /// Normalize Cursor tool input field names to git-ai conventions.
    /// Cursor uses `path`/`contents` where git-ai uses `file_path`/`content`.
    fn normalize_cursor_tool_input(
        tool_name: &str,
        input: &serde_json::Value,
    ) -> serde_json::Value {
        let mut normalized = input.clone();
        if let Some(obj) = normalized.as_object_mut() {
            // Rename `path` → `file_path`
            if let Some(path_val) = obj.remove("path")
                && !obj.contains_key("file_path")
            {
                obj.insert("file_path".to_string(), path_val);
            }
            // For Write tool: rename `contents` → `content`
            if tool_name == "Write"
                && let Some(contents_val) = obj.remove("contents")
                && !obj.contains_key("content")
            {
                obj.insert("content".to_string(), contents_val);
            }
        }
        normalized
    }

    /// Add a tool_use message to the transcript. Edit tools store only
    /// file_path (content is too large); everything else keeps full args.
    fn add_cursor_tool_message(
        transcript: &mut AiTranscript,
        tool_name: &str,
        normalized_input: &serde_json::Value,
        message_id: Option<String>,
    ) {
        match tool_name {
            // Edit tools: store only file_path (content is too large)
            "Write"
            | "Edit"
            | "StrReplace"
            | "Delete"
            | "MultiEdit"
            | "edit_file"
            | "apply_patch"
            | "edit_file_v2_apply_patch"
            | "search_replace"
            | "edit_file_v2_search_replace" => {
                let file_path = normalized_input
                    .get("file_path")
                    .and_then(|v| v.as_str())
                    .or_else(|| normalized_input.get("target_file").and_then(|v| v.as_str()));
                transcript.add_message(Message::tool_use_with_id(
                    tool_name.to_string(),
                    serde_json::json!({ "file_path": file_path.unwrap_or("") }),
                    message_id,
                ));
            }
            // Everything else: store full args
            _ => {
                transcript.add_message(Message::tool_use_with_id(
                    tool_name.to_string(),
                    normalized_input.clone(),
                    message_id,
                ));
            }
        }
    }

    // Get the Cursor database path
    pub fn cursor_global_database_path() -> Result<PathBuf, GitAiError> {
        if let Ok(global_db_path) = std::env::var("GIT_AI_CURSOR_GLOBAL_DB_PATH") {
            return Ok(PathBuf::from(global_db_path));
        }
        let user_dir = Self::cursor_user_dir()?;
        let global_db = user_dir.join("globalStorage").join("state.vscdb");
        Ok(global_db)
    }

    fn cursor_user_dir() -> Result<PathBuf, GitAiError> {
        #[cfg(target_os = "windows")]
        {
            // Windows: %APPDATA%\Cursor\User
            let appdata = env::var("APPDATA")
                .map_err(|e| GitAiError::Generic(format!("APPDATA not set: {}", e)))?;
            Ok(Path::new(&appdata).join("Cursor").join("User"))
        }
        #[cfg(target_os = "macos")]
        {
            // macOS: ~/Library/Application Support/Cursor/User
            let home = dirs::home_dir().ok_or_else(|| {
                GitAiError::Generic("Could not determine home directory".to_string())
            })?;
            Ok(home
                .join("Library")
                .join("Application Support")
                .join("Cursor")
                .join("User"))
        }
        #[cfg(target_os = "linux")]
        {
            // Linux: ~/.config/Cursor/User
            let config_dir = dirs::config_dir().ok_or_else(|| {
                GitAiError::Generic("Could not determine user config directory".to_string())
            })?;
            Ok(config_dir.join("Cursor").join("User"))
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
        {
            Err(GitAiError::PresetError(
                "Cursor is only supported on Windows and macOS platforms".to_string(),
            ))
        }
    }

    /// Fetch the composer payload from Cursor's global DB.
    ///
    /// The composer payload is a JSON object that contains the conversation data.
    /// It is stored in the cursorDiskKV table with the key `composerData:${composer_id}`.
    pub fn fetch_composer_payload(
        global_db_path: &Path,
        composer_id: &str,
    ) -> Result<serde_json::Value, GitAiError> {
        let conn = Self::open_sqlite_readonly(global_db_path)?;
        // Look for the composer data in cursorDiskKV
        let key_pattern = format!("composerData:{}", composer_id);
        let mut stmt = conn
            .prepare("SELECT value FROM cursorDiskKV WHERE key = ?")
            .map_err(|e| GitAiError::Generic(format!("Query failed: {}", e)))?;
        let mut rows = stmt
            .query([&key_pattern])
            .map_err(|e| GitAiError::Generic(format!("Query failed: {}", e)))?;
        if let Ok(Some(row)) = rows.next() {
            let value_text: String = row
                .get(0)
                .map_err(|e| GitAiError::Generic(format!("Failed to read value: {}", e)))?;
            let data = serde_json::from_str::<serde_json::Value>(&value_text)
                .map_err(|e| GitAiError::Generic(format!("Failed to parse JSON: {}", e)))?;
            return Ok(data);
        }
        Err(GitAiError::PresetError(
            "No conversation data found in database".to_string(),
        ))
    }

    fn open_sqlite_readonly(path: &Path) -> Result<Connection, GitAiError> {
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| GitAiError::Generic(format!("Failed to open {:?}: {}", path, e)))
    }

    /// Extract the transcript and model from the composer payload.
    pub fn transcript_data_from_composer_payload(
        data: &serde_json::Value,
        global_db_path: &Path,
        composer_id: &str,
    ) -> Result<Option<(AiTranscript, String)>, GitAiError> {
        // Only support fullConversationHeadersOnly (bubbles format) - the current Cursor format
        // All conversations since April 2025 use this format exclusively
        let conv = data
            .get("fullConversationHeadersOnly")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                GitAiError::PresetError(
                    "Conversation uses unsupported legacy format. Only conversations created after April 2025 are supported.".to_string()
                )
            })?;
        let mut transcript = AiTranscript::new();
        let mut model = None;
        for header in conv.iter() {
            if let Some(bubble_id) = header.get("bubbleId").and_then(|v| v.as_str())
                && let Ok(Some(bubble_content)) =
                    Self::fetch_bubble_content_from_db(global_db_path, composer_id, bubble_id)
            {
                // Get bubble created at (ISO 8601 UTC string)
                let bubble_created_at = bubble_content
                    .get("createdAt")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                // Extract model from bubble (first value wins)
                if model.is_none()
                    && let Some(model_info) = bubble_content.get("modelInfo")
                    && let Some(model_name) = model_info.get("modelName").and_then(|v| v.as_str())
                {
                    model = Some(model_name.to_string());
                }

                let bid = Some(bubble_id.to_string());

                
                // Extract text from bubble
                // non-empty text field && type == 1 -> User message 
                // non-empty text field && type == 2 -> Assistant message 
                // empty text field && has "thinking" block -> Thinking message 
                // empty text field && has "toolFormerData" block && toolFormerData.name = "create_plan" -> Plan message
                // empty text field && has "toolFormerData" block -> ToolUse message
                if let Some(text) = bubble_content.get("text").and_then(|v| v.as_str()) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        let role = header.get("type").and_then(|v| v.as_i64()).unwrap_or(0);
                        if role == 1 {
                            transcript.add_message(Message::user_with_id(
                                trimmed.to_string(),
                                bubble_created_at.clone(),
                                bid.clone(),
                            ));
                        } else if role == 2 {
                            transcript.add_message(Message::assistant_with_id(
                                trimmed.to_string(),
                                bubble_created_at.clone(),
                                bid.clone(),
                            ));
                        } else {
                            tracing::warn!("CursorPreset: [Warning] Unknown type/role {} in bubble: {}", role, bubble_id.to_string());
                        }
                    }
                }

                // Handle thinking blocks 
                if let Some(thinking_data) = bubble_content.get("thinking") {
                    // if there is thinking block, read thinking.text
                    let thinking_text = thinking_data
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("{}");
                    let thinking_text_trimmed = thinking_text.trim();
                    transcript.add_message(Message::thinking_with_id(
                        thinking_text_trimmed.to_string(),
                        bubble_created_at.clone(),
                        bid.clone(),
                    ));
                }

                // Handle (1) plans, and (2) tool calls and edits
                if let Some(tool_former_data) = bubble_content.get("toolFormerData") {
                    let tool_name = tool_former_data
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    let params_str = tool_former_data
                        .get("params")
                        .and_then(|v| v.as_str())
                        .unwrap_or("{}");
                    let params_json = serde_json::from_str::<serde_json::Value>(params_str)
                        .unwrap_or(serde_json::Value::Null);
                    let raw_args_str = tool_former_data
                        .get("rawArgs")
                        .and_then(|v| v.as_str())
                        .unwrap_or("{}");
                    let raw_args_json = serde_json::from_str::<serde_json::Value>(raw_args_str)
                        .unwrap_or(serde_json::Value::Null);
                    // let result = tool_former_data
                    //     .get("result")
                    //     .and_then(|v| v.as_str())
                    //     .unwrap_or("{}");
                    
                    match tool_name {
                        
                        // planning
                        // read additionalData.planUri
                        "create_plan" => {
                            let additional_data_str = bubble_content.get("additionalData")
                                .and_then(|v| v.as_str())
                                .unwrap_or("{}");
                            let additional_data_json = serde_json::from_str::<serde_json::Value>(additional_data_str)
                                .unwrap_or(serde_json::Value::Null);
                            let plan_doc = additional_data_json.get("planUri").and_then(|v| v.as_str())
                                .unwrap_or("");
                            // let plan text 
                            transcript.add_message(Message::plan_with_id(
                                plan_doc.to_string(), 
                                bubble_created_at.clone(), 
                                bid.clone()));
                            // TODO: create_plan tools also has a status field indicating whether user accepts the plan.
                        }

                        // other tool uses
                        // read toolFormerData.rawArgs.target_file field. base git-ai legacy.
                        "edit_file" 
                        | "read_file" => {
                            let target_file =
                                raw_args_json.get("target_file").and_then(|v| v.as_str());
                            transcript.add_message(Message::tool_use_with_id(
                                tool_name.to_string(),
                                serde_json::json!({ "file_path": target_file.unwrap_or("") }),
                                bid.clone(),
                            ));
                        }
                        // read toolFormerData.rawArgs.file_path field
                        "apply_patch"
                        | "edit_file_v2_apply_patch"
                        | "search_replace"
                        | "edit_file_v2_search_replace"
                        | "write"
                        | "MultiEdit" => {
                            let file_path = raw_args_json.get("file_path").and_then(|v| v.as_str());
                            transcript.add_message(Message::tool_use_with_id(
                                tool_name.to_string(),
                                serde_json::json!({ "file_path": file_path.unwrap_or("") }),
                                bid.clone(),
                            ));
                        }
                        // read toolFormerData.rawArgs field
                        "codebase_search" | "grep" | "web_search" | "web_fetch"
                        | "run_terminal_cmd" | "glob_file_search" 
                        | "file_search" | "grep_search" | "list_dir" | "ripgrep" | "ripgrep_raw_search"
                        | "semantic_search_full" 
                        | "list_mcp_resources"
                        | "read_lints" 
                        | "switch_mode"
                        | "delete_path"
                        | "await" => {
                            transcript.add_message(Message::tool_use_with_id(
                                tool_name.to_string(),
                                raw_args_json,
                                bid.clone(),
                            ));
                        }

                        // read toolFormerData.params field
                        "edit_file_v2"
                        | "run_terminal_command_v2" 
                        | "task_v2" => {
                            // let target_file =
                            //     params_json.get("relativeWorkspacePath").and_then(|v| v.as_str());
                            transcript.add_message(Message::tool_use_with_id(
                                tool_name.to_string(),
                                params_json,
                                bid.clone(),
                            ));
                        }

                        // read toolFormerData.rawArgs.todos field
                        "todo_write" => {
                            let todos = bubble_content.get("todos").and_then(|v| v.as_array()); // list of json object                            
                            transcript.add_message(Message::tool_use_with_id(
                                tool_name.to_string(),
                                serde_json::json!({ "todos": todos }),
                                bid.clone(),
                            ));
                        }

                        _ => {
                            tracing::error!("CursorPreset: [Error] Unhandled tool name {} in bubble: {}", tool_name, bubble_id.to_string());
                        }

                    }
                }
            }
        }
        if !transcript.messages.is_empty() {
            Ok(Some((transcript, model.unwrap_or("unknown".to_string()))))
        } else {
            Ok(None)
        }
    }

    pub fn fetch_bubble_content_from_db(
        global_db_path: &Path,
        composer_id: &str,
        bubble_id: &str,
    ) -> Result<Option<serde_json::Value>, GitAiError> {
        let conn = Self::open_sqlite_readonly(global_db_path)?;
        // Look for bubble data in cursorDiskKV with pattern bubbleId:composerId:bubbleId
        let bubble_pattern = format!("bubbleId:{}:{}", composer_id, bubble_id);
        let mut stmt = conn
            .prepare("SELECT value FROM cursorDiskKV WHERE key = ?")
            .map_err(|e| GitAiError::Generic(format!("Query failed: {}", e)))?;
        let mut rows = stmt
            .query([&bubble_pattern])
            .map_err(|e| GitAiError::Generic(format!("Query failed: {}", e)))?;
        if let Ok(Some(row)) = rows.next() {
            let value_text: String = row
                .get(0)
                .map_err(|e| GitAiError::Generic(format!("Failed to read value: {}", e)))?;
            let data = serde_json::from_str::<serde_json::Value>(&value_text)
                .map_err(|e| GitAiError::Generic(format!("Failed to parse JSON: {}", e)))?;
            return Ok(Some(data));
        }

        Ok(None)
    }


    /// List all Cursor conversation IDs that belong to the given workspace, along with
    /// their `lastUpdatedAt` timestamps (seconds since epoch).
    ///
    /// Reads `composer.composerHeaders` from the global `ItemTable`. This key contains
    /// an `allComposers` array where each entry has a `workspaceIdentifier.uri.fsPath`
    /// that identifies the workspace the conversation belongs to. Cursor stamps this
    /// field on every conversation header when a workspace is opened, so it covers all
    /// recent conversations including subagents.
    ///
    /// See documentation/cursor_conversation_db_system.md for details.
    pub fn list_workspace_conversation_ids(
        global_db_path: &Path,
        workspace_path: &Path,
    ) -> Result<Vec<(String, u64)>, GitAiError> {
        let conn = Self::open_sqlite_readonly(global_db_path)?;

        let mut stmt = conn
            .prepare("SELECT value FROM ItemTable WHERE key = 'composer.composerHeaders'")
            .map_err(|e| GitAiError::Generic(format!("Query failed: {}", e)))?;

        let mut rows = stmt
            .query([])
            .map_err(|e| GitAiError::Generic(format!("Query failed: {}", e)))?;

        let row = match rows.next() {
            Ok(Some(r)) => r,
            _ => return Ok(Vec::new()),
        };

        let value_text: String = row
            .get(0)
            .map_err(|e| GitAiError::Generic(format!("Failed to read value: {}", e)))?;

        let data: serde_json::Value = serde_json::from_str(&value_text)
            .map_err(|e| GitAiError::Generic(format!("Failed to parse JSON: {}", e)))?;

        let all_composers = match data.get("allComposers").and_then(|v| v.as_array()) {
            Some(arr) => arr,
            None => return Ok(Vec::new()),
        };

        let canonical_workspace = workspace_path
            .canonicalize()
            .unwrap_or_else(|_| workspace_path.to_path_buf());

        let mut results = Vec::new();
        for entry in all_composers {
            let composer_id = match entry.get("composerId").and_then(|v| v.as_str()) {
                Some(id) => id,
                None => continue,
            };

            // Extract fsPath from workspaceIdentifier.uri.fsPath
            let fs_path = entry
                .get("workspaceIdentifier")
                .and_then(|ws| ws.get("uri"))
                .and_then(|uri| uri.get("fsPath"))
                .and_then(|p| p.as_str());

            let matches = match fs_path {
                Some(p) => {
                    let entry_path = PathBuf::from(p);
                    let canonical_entry = entry_path
                        .canonicalize()
                        .unwrap_or_else(|_| entry_path);
                    canonical_entry == canonical_workspace
                }
                None => false,
            };

            if matches {
                // lastUpdatedAt is epoch milliseconds; convert to seconds
                let last_updated_at = entry
                    .get("lastUpdatedAt")
                    .and_then(|v| v.as_u64())
                    .map(|ms| ms / 1000)
                    .unwrap_or(0);

                results.push((composer_id.to_string(), last_updated_at));
            }
        }

        Ok(results)
    }

    /// Return the path to Cursor's per-project agent-transcripts directory.
    ///
    /// Respects `GIT_AI_CURSOR_AGENT_TRANSCRIPTS_PATH` for testability,
    /// otherwise scans `~/.cursor/projects/*/agent-transcripts/`.
    fn cursor_agent_transcripts_dirs() -> Vec<PathBuf> {
        if let Ok(p) = std::env::var("GIT_AI_CURSOR_AGENT_TRANSCRIPTS_PATH") {
            return vec![PathBuf::from(p)];
        }
        let home = match dirs::home_dir() {
            Some(h) => h,
            None => return vec![],
        };
        let projects_dir = home.join(".cursor").join("projects");
        if !projects_dir.is_dir() {
            return vec![];
        }
        let mut dirs = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&projects_dir) {
            for entry in entries.flatten() {
                let at_dir = entry.path().join("agent-transcripts");
                if at_dir.is_dir() {
                    dirs.push(at_dir);
                }
            }
        }
        dirs
    }

    /// Find subagent conversation IDs for a given Cursor conversation.
    ///
    /// Scans `~/.cursor/projects/*/agent-transcripts/<conversation_id>/subagents/`
    /// for `.jsonl` files and returns the UUIDs extracted from filenames.
    /// Returns `None` if no subagents exist.
    pub fn find_subagent_ids(conversation_id: &str) -> Option<Vec<String>> {
        for transcripts_dir in Self::cursor_agent_transcripts_dirs() {
            let subagents_dir = transcripts_dir
                .join(conversation_id)
                .join("subagents");

            if !subagents_dir.is_dir() {
                continue;
            }

            let entries = match std::fs::read_dir(&subagents_dir) {
                Ok(e) => e,
                Err(_) => continue,
            };

            let mut ids = Vec::new();
            for entry in entries.flatten() {
                let filename = entry.file_name().to_string_lossy().to_string();
                if let Some(id) = filename.strip_suffix(".jsonl") {
                    ids.push(id.to_string());
                }
            }

            if !ids.is_empty() {
                ids.sort();
                return Some(ids);
            }
        }

        None
    }
}
