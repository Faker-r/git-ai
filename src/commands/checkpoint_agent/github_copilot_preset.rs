use crate::{
    authorship::{
        transcript::{AiTranscript, Message},
        working_log::{AgentId, CheckpointKind},
    },
    commands::checkpoint_agent::bash_tool::{
        self, BashCheckpointAction, HookEvent, ToolClass,
    },
    commands::checkpoint_agent::{
        agent_presets::{
            AgentCheckpointFlags, AgentCheckpointPreset, AgentRunResult, 
             BashPreHookStrategy, 
            prepare_agent_bash_pre_hook
        },
    },
    error::GitAiError,
    observability::log_error,
};
use chrono::{TimeZone, Utc};
use std::env;
use std::collections::HashMap;
use std::path::{Path,PathBuf};

const COPILOT_HOOK_PRE_TOOL_USE: &str = "PreToolUse";
const COPILOT_HOOK_POST_TOOL_USE: &str = "PostToolUse";



pub struct GithubCopilotPreset;

#[derive(Default)]
struct CopilotModelCandidates {
    request_non_auto_model_id: Option<String>,
    request_model_id: Option<String>,
    session_non_auto_model_id: Option<String>,
    session_model_id: Option<String>,
}

impl CopilotModelCandidates {
    fn best(self) -> Option<String> {
        self.request_non_auto_model_id
            .or(self.request_model_id)
            .or(self.session_non_auto_model_id)
            .or(self.session_model_id)
    }
}

impl AgentCheckpointPreset for GithubCopilotPreset {
    fn run(&self, flags: AgentCheckpointFlags) -> Result<AgentRunResult, GitAiError> {
        let hook_input_json = flags.hook_input.ok_or_else(|| {
            GitAiError::PresetError("hook_input is required for GitHub Copilot preset".to_string())
        })?;

        let hook_data: serde_json::Value = serde_json::from_str(&hook_input_json)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        let hook_event_name = hook_data
            .get("hook_event_name")
            .or_else(|| hook_data.get("hookEventName"))
            .and_then(|v| v.as_str())
            .unwrap_or("after_edit");

        if hook_event_name == "before_edit" || hook_event_name == "after_edit" {
            return Self::run_legacy_extension_hooks(&hook_data, hook_event_name);
        }

        if hook_event_name == COPILOT_HOOK_PRE_TOOL_USE || hook_event_name == COPILOT_HOOK_POST_TOOL_USE {
            return Self::run_vscode_native_hooks(&hook_data, hook_event_name);
        }

        Err(GitAiError::PresetError(format!(
            "Invalid hook_event_name: {}. Expected one of 'before_edit', 'after_edit', '{}', or '{}'",
            hook_event_name,
            COPILOT_HOOK_PRE_TOOL_USE,
            COPILOT_HOOK_POST_TOOL_USE
        )))
    }
}

impl GithubCopilotPreset {
    fn run_legacy_extension_hooks(
        hook_data: &serde_json::Value,
        hook_event_name: &str,
    ) -> Result<AgentRunResult, GitAiError> {
        let repo_working_dir: String = hook_data
            .get("workspace_folder")
            .and_then(|v| v.as_str())
            .or_else(|| hook_data.get("workspaceFolder").and_then(|v| v.as_str()))
            .ok_or_else(|| {
                GitAiError::PresetError(
                    "workspace_folder or workspaceFolder not found in hook_input for GitHub Copilot preset".to_string(),
                )
            })?
            .to_string();

        let dirty_files = Self::dirty_files_from_hook_data(hook_data);

        if hook_event_name == "before_edit" {
            let will_edit_filepaths = hook_data
                .get("will_edit_filepaths")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect::<Vec<String>>()
                })
                .ok_or_else(|| {
                    GitAiError::PresetError(
                        "will_edit_filepaths is required for before_edit hook_event_name"
                            .to_string(),
                    )
                })?;

            if will_edit_filepaths.is_empty() {
                return Err(GitAiError::PresetError(
                    "will_edit_filepaths cannot be empty for before_edit hook_event_name"
                        .to_string(),
                ));
            }

            return Ok(AgentRunResult {
                agent_id: AgentId {
                    tool: "human".to_string(),
                    id: "human".to_string(),
                    model: "human".to_string(),
                },
                agent_metadata: None,
                checkpoint_kind: CheckpointKind::Human,
                transcript: None,
                repo_working_dir: Some(repo_working_dir),
                edited_filepaths: None,
                will_edit_filepaths: Some(will_edit_filepaths),
                dirty_files,
                captured_checkpoint_id: None,
            });
        }

        let chat_session_path = hook_data
            .get("chat_session_path")
            .and_then(|v| v.as_str())
            .or_else(|| hook_data.get("chatSessionPath").and_then(|v| v.as_str()))
            .ok_or_else(|| {
                GitAiError::PresetError(
                    "chat_session_path or chatSessionPath not found in hook_input for after_edit"
                        .to_string(),
                )
            })?;

        let agent_metadata = HashMap::from([(
            "chat_session_path".to_string(),
            chat_session_path.to_string(),
        )]);

        let chat_session_id = hook_data
            .get("chat_session_id")
            .and_then(|v| v.as_str())
            .or_else(|| hook_data.get("session_id").and_then(|v| v.as_str()))
            .or_else(|| hook_data.get("chatSessionId").and_then(|v| v.as_str()))
            .or_else(|| hook_data.get("sessionId").and_then(|v| v.as_str()))
            .unwrap_or("unknown")
            .to_string();

        // TODO Make edited_filepaths required in future versions (after old extensions are updated)
        let edited_filepaths = hook_data
            .get("edited_filepaths")
            .and_then(|val| val.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect::<Vec<String>>()
            });

        let (transcript, detected_model, detected_edited_filepaths) =
            GithubCopilotPreset::transcript_and_model_from_copilot_session_json(chat_session_path)
                .map(|(t, m, f)| (Some(t), m, f))
                .unwrap_or_else(|e| {
                    eprintln!(
                        "[Warning] Failed to parse GitHub Copilot chat session JSON from {} (will update transcript at commit): {}",
                        chat_session_path, e
                    );
                    log_error(
                        &e,
                        Some(serde_json::json!({
                            "agent_tool": "github-copilot",
                            "operation": "transcript_and_model_from_copilot_session_json",
                            "note": "JSON exists but invalid"
                        })),
                    );
                    (None, None, None)
                });

        let agent_id = AgentId {
            tool: "github-copilot".to_string(),
            id: chat_session_id,
            model: detected_model.unwrap_or_else(|| "unknown".to_string()),
        };

        Ok(AgentRunResult {
            agent_id,
            agent_metadata: Some(agent_metadata),
            checkpoint_kind: CheckpointKind::AiAgent,
            transcript,
            repo_working_dir: Some(repo_working_dir),
            // TODO Remove detected_edited_filepaths once edited_filepaths is required in future versions (after old extensions are updated)
            edited_filepaths: edited_filepaths.or(detected_edited_filepaths),
            will_edit_filepaths: None,
            dirty_files,
            captured_checkpoint_id: None,
        })
    }

    fn run_vscode_native_hooks(
        hook_data: &serde_json::Value,
        hook_event_name: &str,
    ) -> Result<AgentRunResult, GitAiError> {
        let cwd = hook_data
            .get("cwd")
            .and_then(|v| v.as_str())
            .or_else(|| hook_data.get("workspace_folder").and_then(|v| v.as_str()))
            .or_else(|| hook_data.get("workspaceFolder").and_then(|v| v.as_str()))
            .ok_or_else(|| GitAiError::PresetError("cwd not found in hook_input".to_string()))?
            .to_string();

        let dirty_files = Self::dirty_files_from_hook_data(hook_data);
        let chat_session_id = hook_data
            .get("chat_session_id")
            .and_then(|v| v.as_str())
            .or_else(|| hook_data.get("session_id").and_then(|v| v.as_str()))
            .or_else(|| hook_data.get("chatSessionId").and_then(|v| v.as_str()))
            .or_else(|| hook_data.get("sessionId").and_then(|v| v.as_str()))
            .unwrap_or("unknown")
            .to_string();

        let tool_name = hook_data
            .get("tool_name")
            .and_then(|v| v.as_str())
            .or_else(|| hook_data.get("toolName").and_then(|v| v.as_str()))
            .unwrap_or("unknown");

        // VS Code currently executes imported hooks even when matcher/tool filters are ignored.
        // Enforce tool filtering in git-ai to avoid creating checkpoints for read/search tools.
        if !Self::is_supported_vscode_edit_tool_name(tool_name) {
            return Err(GitAiError::PresetError(format!(
                "Skipping VS Code hook for unsupported tool_name '{}' (non-edit tool).",
                tool_name
            )));
        }

        let tool_input = hook_data
            .get("tool_input")
            .or_else(|| hook_data.get("toolInput"));
        let tool_response = hook_data
            .get("tool_response")
            .or_else(|| hook_data.get("toolResponse"));

        // Extract file paths ONLY from tool_input and tool_response. This ensures strict tool-call
        // scoping: we capture exactly which file(s) THIS tool invocation operated on, not session-
        // level history. Do NOT merge hook_data.edited_filepaths/will_edit_filepaths as those may
        // contain stale session-level data from previous tool calls, causing cross-contamination
        // in rapid multi-file operations.
        let extracted_paths =
            Self::extract_filepaths_from_vscode_hook_payload(tool_input, tool_response, &cwd);

        let transcript_path = Self::transcript_path_from_hook_data(hook_data).map(str::to_string);

        if let Some(path) = transcript_path.as_deref()
            && Self::looks_like_claude_transcript_path(path)
        {
            return Err(GitAiError::PresetError(
                "Skipping VS Code hook because transcript_path looks like a Claude transcript path."
                    .to_string(),
            ));
        }

        // Load transcript and model from session JSON. Transcript parsing is ONLY used for:
        // 1. Transcript content (conversation messages for display)
        // 2. Model detection (fallback if not in chat_sessions)
        // File paths are NEVER sourced from transcript - only from hook payload (tool_input)
        // to ensure we capture exactly what THIS tool call edited, not session-level history.
        let (transcript, mut detected_model) = if let Some(path) = transcript_path.as_deref() {
            // Parse transcript but discard the detected_edited_filepaths (3rd return value)
            GithubCopilotPreset::transcript_and_model_from_copilot_session_json(path)
                .map(|(t, m, _)| (Some(t), m))
                .unwrap_or_else(|e| {
                    eprintln!(
                        "[Warning] Failed to parse GitHub Copilot chat session JSON from {} (will update transcript at commit): {}",
                        path, e
                    );
                    log_error(
                        &e,
                        Some(serde_json::json!({
                            "agent_tool": "github-copilot",
                            "operation": "transcript_and_model_from_copilot_session_json",
                            "note": "JSON exists but invalid"
                        })),
                    );
                    (None, None)
                })
        } else {
            (None, None)
        };

        if let Some(path) = transcript_path.as_deref()
            && chat_session_id != "unknown"
            && Self::should_resolve_model_from_chat_sessions(detected_model.as_deref())
            && let Some(chat_sessions_model) =
                Self::model_from_copilot_chat_sessions(path, &chat_session_id)
        {
            detected_model = Some(chat_sessions_model);
        }

        if !Self::is_likely_copilot_native_hook(transcript_path.as_deref()) {
            return Err(GitAiError::PresetError(format!(
                "Skipping VS Code hook for non-Copilot session (tool_name: {}, model: {}).",
                tool_name,
                detected_model.as_deref().unwrap_or("unknown")
            )));
        }

        // extracted_paths now contains ONLY files from this tool call's hook payload (tool_input/tool_response).
        // No merging of session-level detected_edited_filepaths - this prevents cross-contamination
        // when multiple tool calls fire in rapid succession.

        // Classify tool for bash vs file edit handling
        let tool_class = Self::classify_copilot_tool(tool_name);
        let is_bash_tool = tool_class == ToolClass::Bash;

        let tool_use_id = hook_data
            .get("tool_use_id")
            .or_else(|| hook_data.get("toolUseId"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        let agent_id = AgentId {
            tool: "github-copilot".to_string(),
            id: chat_session_id.clone(),
            model: detected_model
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
        };

        let agent_metadata = if let Some(path) = transcript_path.as_ref() {
            HashMap::from([
                ("transcript_path".to_string(), path.clone()),
                ("chat_session_path".to_string(), path.clone()),
            ])
        } else {
            HashMap::new()
        };

        if hook_event_name == "PreToolUse" {
            // Handle bash tool PreToolUse (take snapshot)
            let pre_hook_captured_id = prepare_agent_bash_pre_hook(
                is_bash_tool,
                Some(&cwd),
                &chat_session_id,
                tool_use_id,
                &agent_id,
                Some(&agent_metadata),
                BashPreHookStrategy::SnapshotOnly,
            )?
            .captured_checkpoint_id();

            if is_bash_tool {
                // For bash tools, PreToolUse creates a snapshot but no Human checkpoint
                return Ok(AgentRunResult {
                    agent_id: AgentId {
                        tool: "human".to_string(),
                        id: "human".to_string(),
                        model: "human".to_string(),
                    },
                    agent_metadata: None,
                    checkpoint_kind: CheckpointKind::Human,
                    transcript: None,
                    repo_working_dir: Some(cwd),
                    edited_filepaths: None,
                    will_edit_filepaths: None,
                    dirty_files: None,
                    captured_checkpoint_id: pre_hook_captured_id,
                });
            }
            // For create_file PreToolUse, synthesize dirty_files with empty content to explicitly
            // mark the file as not existing yet (rather than letting it fall back to disk read,
            // which could capture content from a concurrent tool call).
            if tool_name.eq_ignore_ascii_case("create_file") {
                let mut empty_dirty_files = HashMap::new();
                for path in &extracted_paths {
                    empty_dirty_files.insert(path.clone(), String::new());
                }
                // Override dirty_files with our synthesized empty content
                let dirty_files = Some(empty_dirty_files);

                if extracted_paths.is_empty() {
                    return Err(GitAiError::PresetError(
                        "No file path found in create_file PreToolUse tool_input".to_string(),
                    ));
                }

                return Ok(AgentRunResult {
                    agent_id: AgentId {
                        tool: "human".to_string(),
                        id: "human".to_string(),
                        model: "human".to_string(),
                    },
                    agent_metadata: None,
                    checkpoint_kind: CheckpointKind::Human,
                    transcript: None,
                    repo_working_dir: Some(cwd),
                    edited_filepaths: None,
                    will_edit_filepaths: Some(extracted_paths),
                    dirty_files,
                    captured_checkpoint_id: None,
                });
            }

            if extracted_paths.is_empty() {
                return Err(GitAiError::PresetError(format!(
                    "No editable file paths found in VS Code hook input (tool_name: {}). Skipping checkpoint.",
                    tool_name
                )));
            }

            return Ok(AgentRunResult {
                agent_id: AgentId {
                    tool: "human".to_string(),
                    id: "human".to_string(),
                    model: "human".to_string(),
                },
                agent_metadata: None,
                checkpoint_kind: CheckpointKind::Human,
                transcript: None,
                repo_working_dir: Some(cwd),
                edited_filepaths: None,
                will_edit_filepaths: Some(extracted_paths),
                dirty_files,
                captured_checkpoint_id: None,
            });
        }

        // PostToolUse: Handle bash tools via snapshot diff
        let bash_result = if is_bash_tool {
            let repo_root = Path::new(&cwd);
            Some(bash_tool::handle_bash_tool(
                HookEvent::PostToolUse,
                repo_root,
                &chat_session_id,
                tool_use_id,
            ))
        } else {
            None
        };

        let final_edited_filepaths = if is_bash_tool {
            match bash_result.as_ref().unwrap().as_ref().map(|r| &r.action) {
                Ok(BashCheckpointAction::Checkpoint(paths)) => Some(paths.clone()),
                Ok(BashCheckpointAction::NoChanges) => None,
                Ok(BashCheckpointAction::Fallback) => None,
                Ok(BashCheckpointAction::TakePreSnapshot) => {
                    // This shouldn't happen in PostToolUse, but handle it gracefully
                    None
                }
                Err(_) => {
                    eprintln!("[Warning] Bash tool snapshot diff failed, skipping checkpoint");
                    None
                }
            }
        } else {
            Some(extracted_paths)
        };

        let bash_captured_checkpoint_id = bash_result
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .and_then(|r| r.captured_checkpoint.as_ref())
            .map(|info| info.capture_id.clone());

        let transcript_path = transcript_path.ok_or_else(|| {
            GitAiError::PresetError(
                "transcript_path not found in hook_input for PostToolUse".to_string(),
            )
        })?;

        let final_agent_metadata = HashMap::from([
            ("transcript_path".to_string(), transcript_path.clone()),
            ("chat_session_path".to_string(), transcript_path),
        ]);

        if final_edited_filepaths.is_none() || final_edited_filepaths.as_ref().unwrap().is_empty() {
            return Err(GitAiError::PresetError(format!(
                "No editable file paths found in VS Code PostToolUse hook input (tool_name: {}). Skipping checkpoint.",
                tool_name
            )));
        }

        Ok(AgentRunResult {
            agent_id,
            agent_metadata: Some(final_agent_metadata),
            checkpoint_kind: CheckpointKind::AiAgent,
            transcript,
            repo_working_dir: Some(cwd),
            edited_filepaths: final_edited_filepaths,
            will_edit_filepaths: None,
            dirty_files,
            captured_checkpoint_id: bash_captured_checkpoint_id,
        })
    }

    fn dirty_files_from_hook_data(
        hook_data: &serde_json::Value,
    ) -> Option<HashMap<String, String>> {
        hook_data
            .get("dirty_files")
            .and_then(|v| v.as_object())
            .or_else(|| hook_data.get("dirtyFiles").and_then(|v| v.as_object()))
            .map(|obj| {
                obj.iter()
                    .filter_map(|(key, value)| {
                        value
                            .as_str()
                            .map(|content| (key.clone(), content.to_string()))
                    })
                    .collect::<HashMap<String, String>>()
            })
    }

    fn is_likely_copilot_native_hook(transcript_path: Option<&str>) -> bool {
        let Some(path) = transcript_path else {
            return false;
        };

        if Self::looks_like_claude_transcript_path(path) {
            return false;
        }

        Self::looks_like_copilot_transcript_path(path)
    }

    fn should_resolve_model_from_chat_sessions(detected_model: Option<&str>) -> bool {
        match detected_model {
            None => true,
            Some(model) => {
                let normalized = model.trim().to_ascii_lowercase();
                normalized.is_empty() || normalized == "unknown" || normalized == "copilot/auto"
            }
        }
    }

    fn model_from_copilot_chat_sessions(
        transcript_path: &str,
        transcript_session_id: &str,
    ) -> Option<String> {
        let chat_sessions_dir = Self::chat_sessions_dir_from_transcript_path(transcript_path)?;
        let entries = std::fs::read_dir(chat_sessions_dir).ok()?;
        let mut candidates = CopilotModelCandidates::default();

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            let ext = path
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if ext != "json" && ext != "jsonl" {
                continue;
            }

            let content = match std::fs::read_to_string(&path) {
                Ok(content) => content,
                Err(_) => continue,
            };

            if !content.contains(transcript_session_id) {
                continue;
            }

            Self::collect_model_candidates_from_chat_session_content(
                &content,
                transcript_session_id,
                &mut candidates,
            );

            if candidates.request_non_auto_model_id.is_some() {
                break;
            }
        }

        candidates.best()
    }

    fn chat_sessions_dir_from_transcript_path(transcript_path: &str) -> Option<PathBuf> {
        let transcript = Path::new(transcript_path);
        let transcripts_dir = transcript.parent()?;
        let is_transcripts_dir = transcripts_dir
            .file_name()
            .and_then(|v| v.to_str())
            .map(|name| name.eq_ignore_ascii_case("transcripts"))
            .unwrap_or(false);
        if !is_transcripts_dir {
            return None;
        }

        let copilot_dir = transcripts_dir.parent()?;
        let is_copilot_dir = copilot_dir
            .file_name()
            .and_then(|v| v.to_str())
            .map(|name| name.eq_ignore_ascii_case("github.copilot-chat"))
            .unwrap_or(false);
        if !is_copilot_dir {
            return None;
        }

        let workspace_storage_dir = copilot_dir.parent()?;
        let chat_sessions_dir = workspace_storage_dir.join("chatSessions");
        if chat_sessions_dir.is_dir() {
            Some(chat_sessions_dir)
        } else {
            None
        }
    }

    fn collect_model_candidates_from_chat_session_content(
        content: &str,
        transcript_session_id: &str,
        candidates: &mut CopilotModelCandidates,
    ) {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(content) {
            Self::collect_model_candidates_from_session_object(
                &parsed,
                transcript_session_id,
                candidates,
            );
            return;
        }

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let parsed_line: serde_json::Value = match serde_json::from_str(trimmed) {
                Ok(value) => value,
                Err(_) => continue,
            };

            match parsed_line.get("kind").and_then(|v| v.as_u64()) {
                Some(0) => {
                    if let Some(session_obj) = parsed_line.get("v") {
                        Self::collect_model_candidates_from_session_object(
                            session_obj,
                            transcript_session_id,
                            candidates,
                        );
                    }
                }
                Some(2) => {
                    if let Some(requests) = parsed_line.get("v").and_then(|v| v.as_array()) {
                        for request in requests {
                            Self::collect_model_candidates_from_request(
                                request,
                                transcript_session_id,
                                candidates,
                            );
                        }
                    }
                }
                _ => {
                    Self::collect_model_candidates_from_session_object(
                        &parsed_line,
                        transcript_session_id,
                        candidates,
                    );
                }
            }
        }
    }

    fn collect_model_candidates_from_session_object(
        session_obj: &serde_json::Value,
        transcript_session_id: &str,
        candidates: &mut CopilotModelCandidates,
    ) {
        if let Some(selected_model) = session_obj
            .get("inputState")
            .and_then(|v| v.get("selectedModel"))
            .and_then(|v| v.get("identifier"))
            .and_then(|v| v.as_str())
        {
            Self::record_selected_model_candidate(candidates, selected_model);
        }

        if let Some(requests) = session_obj.get("requests").and_then(|v| v.as_array()) {
            for request in requests {
                Self::collect_model_candidates_from_request(
                    request,
                    transcript_session_id,
                    candidates,
                );
            }
        }
    }

    fn collect_model_candidates_from_request(
        request: &serde_json::Value,
        transcript_session_id: &str,
        candidates: &mut CopilotModelCandidates,
    ) {
        if !Self::request_matches_transcript_session(request, transcript_session_id) {
            return;
        }

        if let Some(model_id) = request.get("modelId").and_then(|v| v.as_str()) {
            Self::record_model_id_candidate(candidates, model_id);
        }
    }

    fn request_matches_transcript_session(
        request: &serde_json::Value,
        transcript_session_id: &str,
    ) -> bool {
        request
            .get("result")
            .and_then(|v| v.get("metadata"))
            .and_then(|v| v.get("sessionId"))
            .and_then(|v| v.as_str())
            .map(|session_id| session_id == transcript_session_id)
            .unwrap_or(false)
            || request
                .get("result")
                .and_then(|v| v.get("sessionId"))
                .and_then(|v| v.as_str())
                .map(|session_id| session_id == transcript_session_id)
                .unwrap_or(false)
            || request
                .get("sessionId")
                .and_then(|v| v.as_str())
                .map(|session_id| session_id == transcript_session_id)
                .unwrap_or(false)
    }

    fn record_model_id_candidate(candidates: &mut CopilotModelCandidates, model_id: &str) {
        let model = model_id.trim();
        if model.is_empty() {
            return;
        }

        if candidates.request_model_id.is_none() {
            candidates.request_model_id = Some(model.to_string());
        }

        if !model.eq_ignore_ascii_case("copilot/auto")
            && candidates.request_non_auto_model_id.is_none()
        {
            candidates.request_non_auto_model_id = Some(model.to_string());
        }
    }

    fn record_selected_model_candidate(candidates: &mut CopilotModelCandidates, model_id: &str) {
        let model = model_id.trim();
        if model.is_empty() {
            return;
        }

        if candidates.session_model_id.is_none() {
            candidates.session_model_id = Some(model.to_string());
        }

        if !model.eq_ignore_ascii_case("copilot/auto")
            && candidates.session_non_auto_model_id.is_none()
        {
            candidates.session_non_auto_model_id = Some(model.to_string());
        }
    }

    pub fn transcript_path_from_hook_data(hook_data: &serde_json::Value) -> Option<&str> {
        hook_data
            .get("transcript_path")
            .and_then(|v| v.as_str())
            .or_else(|| hook_data.get("transcriptPath").and_then(|v| v.as_str()))
            .or_else(|| hook_data.get("chat_session_path").and_then(|v| v.as_str()))
            .or_else(|| hook_data.get("chatSessionPath").and_then(|v| v.as_str()))
    }

    pub fn looks_like_claude_transcript_path(path: &str) -> bool {
        let normalized = path.replace('\\', "/").to_ascii_lowercase();
        normalized.contains("/.claude/") || normalized.contains("/claude/projects/")
    }

    pub fn looks_like_copilot_transcript_path(path: &str) -> bool {
        let normalized = path.replace('\\', "/").to_ascii_lowercase();
        normalized.contains("/github.copilot-chat/transcripts/")
            || normalized.contains("vscode-chat-session")
            || normalized.contains("copilot_session")
            || (normalized.contains("/workspacestorage/") && normalized.contains("/chatsessions/"))
    }

    fn is_supported_vscode_edit_tool_name(tool_name: &str) -> bool {
        let lower = tool_name.to_ascii_lowercase();

        // Explicit bash/terminal tools that should be tracked (handled via bash_tool flow)
        let bash_tools = ["run_in_terminal"];
        if bash_tools.iter().any(|name| lower == *name) {
            return true;
        }

        let non_edit_keywords = [
            "find", "search", "read", "grep", "glob", "list", "ls", "fetch", "web", "open", "todo",
        ];
        if non_edit_keywords.iter().any(|kw| lower.contains(kw)) {
            return false;
        }

        let exact_edit_tools = [
            "write",
            "edit",
            "multiedit",
            "applypatch",
            "apply_patch",
            "copilot_insertedit",
            "copilot_replacestring",
            "vscode_editfile_internal",
            "create_file",
            "delete_file",
            "rename_file",
            "move_file",
            "replace_string_in_file",
            "insert_edit_into_file",
        ];
        if exact_edit_tools.iter().any(|name| lower == *name) {
            return true;
        }

        lower.contains("edit") || lower.contains("write") || lower.contains("replace")
    }

    /// Classify GitHub Copilot tool for bash vs file edit handling
    fn classify_copilot_tool(tool_name: &str) -> ToolClass {
        let lower = tool_name.to_ascii_lowercase();
        match lower.as_str() {
            "run_in_terminal" => ToolClass::Bash,
            "create_file"
            | "replace_string_in_file"
            | "apply_patch"
            | "delete_file"
            | "rename_file"
            | "move_file" => ToolClass::FileEdit,
            _ if lower.contains("edit") || lower.contains("write") || lower.contains("replace") => {
                ToolClass::FileEdit
            }
            _ => ToolClass::Skip,
        }
    }

    fn collect_apply_patch_paths_from_text(raw: &str, out: &mut Vec<String>) {
        for line in raw.lines() {
            let trimmed = line.trim();
            let maybe_path = trimmed
                .strip_prefix("*** Update File: ")
                .or_else(|| trimmed.strip_prefix("*** Add File: "))
                .or_else(|| trimmed.strip_prefix("*** Delete File: "))
                .or_else(|| trimmed.strip_prefix("*** Move to: "));

            if let Some(path) = maybe_path {
                let path = path.trim();
                if !path.is_empty() && !out.iter().any(|existing| existing == path) {
                    out.push(path.to_string());
                }
            }
        }
    }

    fn extract_filepaths_from_vscode_hook_payload(
        tool_input: Option<&serde_json::Value>,
        tool_response: Option<&serde_json::Value>,
        cwd: &str,
    ) -> Vec<String> {
        let mut raw_paths = Vec::new();
        if let Some(value) = tool_input {
            Self::collect_tool_paths(value, &mut raw_paths);
        }
        if let Some(value) = tool_response {
            Self::collect_tool_paths(value, &mut raw_paths);
        }

        let mut normalized_paths = Vec::new();
        for raw in raw_paths {
            if let Some(path) = Self::normalize_hook_path(&raw, cwd)
                && !normalized_paths.contains(&path)
            {
                normalized_paths.push(path);
            }
        }
        normalized_paths
    }

    fn collect_tool_paths(value: &serde_json::Value, out: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, val) in map {
                    let key_lower = key.to_ascii_lowercase();
                    let is_single_path_key = key_lower == "file_path"
                        || key_lower == "filepath"
                        || key_lower == "path"
                        || key_lower == "fspath";

                    let is_multi_path_key = key_lower == "files"
                        || key_lower == "filepaths"
                        || key_lower == "file_paths";

                    if is_single_path_key {
                        if let Some(path) = val.as_str() {
                            out.push(path.to_string());
                        }
                    } else if is_multi_path_key {
                        match val {
                            serde_json::Value::String(path) => out.push(path.to_string()),
                            serde_json::Value::Array(paths) => {
                                for path_value in paths {
                                    if let Some(path) = path_value.as_str() {
                                        out.push(path.to_string());
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    Self::collect_tool_paths(val, out);
                }
            }
            serde_json::Value::Array(arr) => {
                for item in arr {
                    Self::collect_tool_paths(item, out);
                }
            }
            serde_json::Value::String(s) => {
                if s.starts_with("file://") {
                    out.push(s.to_string());
                }
                Self::collect_apply_patch_paths_from_text(s, out);
            }
            _ => {}
        }
    }

    fn normalize_hook_path(raw_path: &str, cwd: &str) -> Option<String> {
        let trimmed = raw_path.trim();
        if trimmed.is_empty() {
            return None;
        }

        let path_without_scheme = trimmed
            .strip_prefix("file://localhost")
            .or_else(|| trimmed.strip_prefix("file://"))
            .unwrap_or(trimmed);

        let path = Path::new(path_without_scheme);
        let joined = if path.is_absolute()
            || path_without_scheme.starts_with("\\\\")
            || path_without_scheme
                .as_bytes()
                .get(1)
                .map(|b| *b == b':')
                .unwrap_or(false)
        {
            PathBuf::from(path_without_scheme)
        } else {
            Path::new(cwd).join(path_without_scheme)
        };

        Some(joined.to_string_lossy().replace('\\', "/"))
    }
}

impl GithubCopilotPreset {
    /// Translate a GitHub Copilot chat session JSON file into an AiTranscript, optional model, and edited filepaths.
    /// Returns an empty transcript if running in Codespaces or Remote Containers.
    #[allow(clippy::type_complexity)]
    pub fn transcript_and_model_from_copilot_session_json(
        session_json_path: &str,
    ) -> Result<(AiTranscript, Option<String>, Option<Vec<String>>), GitAiError> {
        // Check if running in Codespaces or Remote Containers - if so, return empty transcript
        let is_codespaces = env::var("CODESPACES").ok().as_deref() == Some("true");
        let is_remote_containers = env::var("REMOTE_CONTAINERS").ok().as_deref() == Some("true");

        if is_codespaces || is_remote_containers {
            return Ok((AiTranscript::new(), None, Some(Vec::new())));
        }

        // Read the session JSON file.
        // Supports both plain .json (pretty-printed or single-line) and .jsonl files
        // where the session is wrapped in a JSONL envelope on the first line:
        //   {"kind":0,"v":{...session data...}}
        let session_json_str =
            std::fs::read_to_string(session_json_path).map_err(GitAiError::IoError)?;

        // Try parsing the first line as JSON first (handles JSONL and single-line JSON).
        // Fall back to parsing the entire content (handles pretty-printed JSON).
        let first_line = session_json_str.lines().next().unwrap_or("");
        let parsed: serde_json::Value = serde_json::from_str(first_line)
            .or_else(|_| serde_json::from_str(&session_json_str))
            .map_err(GitAiError::JsonError)?;

        // New VS Code Copilot transcript format (1.109.3+):
        // JSONL event stream with lines like {"type":"session.start","data":{...}}
        if Self::looks_like_copilot_event_stream_root(&parsed) {
            return Self::transcript_and_model_from_copilot_event_stream_jsonl(&session_json_str);
        }

        // Auto-detect JSONL wrapper: if the parsed value has "kind" and "v" fields,
        // unwrap to use the inner "v" object as the session data
        let is_jsonl = parsed.get("kind").is_some() && parsed.get("v").is_some();
        let mut session_json = if is_jsonl {
            parsed.get("v").unwrap().clone()
        } else {
            parsed
        };

        // Apply incremental patches from subsequent JSONL lines (kind:1 = scalar, kind:2 = array/object)
        if is_jsonl {
            for line in session_json_str.lines().skip(1) {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let patch: serde_json::Value = match serde_json::from_str(line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let kind = match patch.get("kind").and_then(|v| v.as_u64()) {
                    Some(k) => k,
                    None => continue,
                };
                if (kind == 1 || kind == 2)
                    && let (Some(key_path), Some(value)) =
                        (patch.get("k").and_then(|v| v.as_array()), patch.get("v"))
                {
                    // Walk the key path on session_json, setting the value at the leaf
                    let keys: Vec<String> = key_path
                        .iter()
                        .filter_map(|k| {
                            k.as_str()
                                .map(|s| s.to_string())
                                .or_else(|| k.as_u64().map(|n| n.to_string()))
                                .or_else(|| k.as_i64().map(|n| n.to_string()))
                        })
                        .collect();
                    if !keys.is_empty() {
                        // Use pointer-based indexing to find the parent, then insert at leaf
                        let json_pointer = if keys.len() == 1 {
                            String::new()
                        } else {
                            format!("/{}", keys[..keys.len() - 1].join("/"))
                        };
                        let leaf_key = &keys[keys.len() - 1];
                        let parent = if json_pointer.is_empty() {
                            Some(&mut session_json)
                        } else {
                            session_json.pointer_mut(&json_pointer)
                        };
                        if let Some(obj) = parent.and_then(|p| p.as_object_mut()) {
                            obj.insert(leaf_key.clone(), value.clone());
                        }
                    }
                }
            }
        }

        // Extract the requests array which represents the conversation from start to finish
        let requests = session_json
            .get("requests")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                GitAiError::PresetError(
                    "requests array not found in Copilot chat session".to_string(),
                )
            })?;

        // Extract session-level model from inputState as fallback
        let session_level_model: Option<String> = session_json
            .get("inputState")
            .and_then(|is| is.get("selectedModel"))
            .and_then(|sm| sm.get("identifier"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let mut transcript = AiTranscript::new();
        let mut detected_model: Option<String> = None;
        let mut edited_filepaths: Vec<String> = Vec::new();

        for request in requests {
            // Parse the human timestamp once per request (unix ms and RFC3339)
            let user_ts_ms = request.get("timestamp").and_then(|v| v.as_i64());
            let user_ts_rfc3339 = user_ts_ms.and_then(|ms| {
                Utc.timestamp_millis_opt(ms)
                    .single()
                    .map(|dt| dt.to_rfc3339())
            });

            // Add the human's message
            if let Some(user_text) = request
                .get("message")
                .and_then(|m| m.get("text"))
                .and_then(|v| v.as_str())
            {
                let trimmed = user_text.trim();
                if !trimmed.is_empty() {
                    transcript.add_message(Message::User {
                        text: trimmed.to_string(),
                        timestamp: user_ts_rfc3339.clone(),
                        id: None,
                    });
                }
            }

            // Process the agent's response items: tool invocations, edits, and text
            if let Some(response_items) = request.get("response").and_then(|v| v.as_array()) {
                let mut assistant_text_accumulator = String::new();

                for item in response_items {
                    // Capture tool invocations and other structured actions as tool_use
                    if let Some(kind) = item.get("kind").and_then(|v| v.as_str()) {
                        match kind {
                            // Primary tool invocation entries
                            "toolInvocationSerialized" => {
                                let tool_name = item
                                    .get("toolId")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("tool");

                                // Normalize invocationMessage to a string
                                let inv_msg = item.get("invocationMessage").and_then(|im| {
                                    if let Some(s) = im.as_str() {
                                        Some(s.to_string())
                                    } else if im.is_object() {
                                        im.get("value")
                                            .and_then(|v| v.as_str())
                                            .map(|s| s.to_string())
                                    } else {
                                        None
                                    }
                                });

                                if let Some(msg) = inv_msg {
                                    transcript.add_message(Message::tool_use(
                                        tool_name.to_string(),
                                        serde_json::Value::String(msg),
                                    ));
                                }
                            }
                            // Other structured response elements worth capturing
                            "textEditGroup" => {
                                // Extract file path from textEditGroup
                                if let Some(uri_obj) = item.get("uri") {
                                    let path_opt = uri_obj
                                        .get("fsPath")
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string())
                                        .or_else(|| {
                                            uri_obj
                                                .get("path")
                                                .and_then(|v| v.as_str())
                                                .map(|s| s.to_string())
                                        });
                                    if let Some(p) = path_opt
                                        && !edited_filepaths.contains(&p)
                                    {
                                        edited_filepaths.push(p);
                                    }
                                }
                                transcript
                                    .add_message(Message::tool_use(kind.to_string(), item.clone()));
                            }
                            "prepareToolInvocation" => {
                                transcript
                                    .add_message(Message::tool_use(kind.to_string(), item.clone()));
                            }
                            // codeblockUri should contribute a visible mention like @path, not a tool_use
                            "codeblockUri" => {
                                let path_opt = item
                                    .get("uri")
                                    .and_then(|u| {
                                        u.get("fsPath")
                                            .and_then(|v| v.as_str())
                                            .map(|s| s.to_string())
                                            .or_else(|| {
                                                u.get("path")
                                                    .and_then(|v| v.as_str())
                                                    .map(|s| s.to_string())
                                            })
                                    })
                                    .or_else(|| {
                                        item.get("fsPath")
                                            .and_then(|v| v.as_str())
                                            .map(|s| s.to_string())
                                    })
                                    .or_else(|| {
                                        item.get("path")
                                            .and_then(|v| v.as_str())
                                            .map(|s| s.to_string())
                                    });
                                if let Some(p) = path_opt {
                                    let mention = format!("@{}", p);
                                    if !assistant_text_accumulator.is_empty() {
                                        assistant_text_accumulator.push(' ');
                                    }
                                    assistant_text_accumulator.push_str(&mention);
                                }
                            }
                            // inlineReference should contribute a visible mention like @path, not a tool_use
                            "inlineReference" => {
                                let path_opt = item.get("inlineReference").and_then(|ir| {
                                    // Try nested uri.fsPath or uri.path
                                    ir.get("uri")
                                        .and_then(|u| u.get("fsPath"))
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string())
                                        .or_else(|| {
                                            ir.get("uri")
                                                .and_then(|u| u.get("path"))
                                                .and_then(|v| v.as_str())
                                                .map(|s| s.to_string())
                                        })
                                        // Or top-level fsPath / path on inlineReference
                                        .or_else(|| {
                                            ir.get("fsPath")
                                                .and_then(|v| v.as_str())
                                                .map(|s| s.to_string())
                                        })
                                        .or_else(|| {
                                            ir.get("path")
                                                .and_then(|v| v.as_str())
                                                .map(|s| s.to_string())
                                        })
                                });
                                if let Some(p) = path_opt {
                                    let mention = format!("@{}", p);
                                    if !assistant_text_accumulator.is_empty() {
                                        assistant_text_accumulator.push(' ');
                                    }
                                    assistant_text_accumulator.push_str(&mention);
                                }
                            }
                            _ => {}
                        }
                    }

                    // Accumulate visible assistant text snippets
                    if let Some(val) = item.get("value").and_then(|v| v.as_str()) {
                        let t = val.trim();
                        if !t.is_empty() {
                            if !assistant_text_accumulator.is_empty() {
                                assistant_text_accumulator.push(' ');
                            }
                            assistant_text_accumulator.push_str(t);
                        }
                    }
                }

                if !assistant_text_accumulator.trim().is_empty() {
                    // Set assistant timestamp to user_ts + totalElapsed if available
                    let assistant_ts = request
                        .get("result")
                        .and_then(|r| r.get("timings"))
                        .and_then(|t| t.get("totalElapsed"))
                        .and_then(|v| v.as_i64())
                        .and_then(|elapsed| user_ts_ms.map(|ums| ums + elapsed))
                        .and_then(|ms| {
                            Utc.timestamp_millis_opt(ms)
                                .single()
                                .map(|dt| dt.to_rfc3339())
                        });

                    transcript.add_message(Message::Assistant {
                        text: assistant_text_accumulator.trim().to_string(),
                        timestamp: assistant_ts,
                        id: None,
                    });
                }
            }

            // Detect model from request metadata if not yet set (uses first modelId seen)
            if detected_model.is_none()
                && let Some(model_id) = request.get("modelId").and_then(|v| v.as_str())
            {
                detected_model = Some(model_id.to_string());
            }
        }

        // Fall back to session-level model if no per-request modelId was found
        if detected_model.is_none() {
            detected_model = session_level_model;
        }

        Ok((transcript, detected_model, Some(edited_filepaths)))
    }

    fn looks_like_copilot_event_stream_root(parsed: &serde_json::Value) -> bool {
        parsed
            .get("type")
            .and_then(|v| v.as_str())
            .map(|event_type| {
                parsed.get("data").map(|v| v.is_object()).unwrap_or(false)
                    && parsed.get("kind").is_none()
                    && (event_type.starts_with("session.")
                        || event_type.starts_with("assistant.")
                        || event_type.starts_with("user.")
                        || event_type.starts_with("tool."))
            })
            .unwrap_or(false)
    }

    #[allow(clippy::type_complexity)]
    fn transcript_and_model_from_copilot_event_stream_jsonl(
        session_jsonl: &str,
    ) -> Result<(AiTranscript, Option<String>, Option<Vec<String>>), GitAiError> {
        let mut transcript = AiTranscript::new();
        let mut edited_filepaths: Vec<String> = Vec::new();
        let mut detected_model: Option<String> = None;

        for line in session_jsonl.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let event: serde_json::Value = match serde_json::from_str(line) {
                Ok(value) => value,
                Err(_) => continue,
            };

            let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let data = event.get("data");
            let timestamp = event
                .get("timestamp")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            if detected_model.is_none()
                && let Some(d) = data
            {
                detected_model = Self::extract_copilot_model_hint(d);
            }

            match event_type {
                "user.message" => {
                    if let Some(text) = data
                        .and_then(|d| d.get("content"))
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                    {
                        transcript.add_message(Message::User {
                            text: text.to_string(),
                            timestamp: timestamp.clone(),
                            id: None,
                        });
                    }
                }
                "assistant.message" => {
                    // Prefer visible assistant content; if empty, use reasoningText as a fallback.
                    let assistant_text = data
                        .and_then(|d| d.get("content"))
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .or_else(|| {
                            data.and_then(|d| d.get("reasoningText"))
                                .and_then(|v| v.as_str())
                                .map(str::trim)
                                .filter(|s| !s.is_empty())
                                .map(str::to_string)
                        });

                    if let Some(text) = assistant_text {
                        transcript.add_message(Message::Assistant {
                            text,
                            timestamp: timestamp.clone(),
                            id: None,
                        });
                    }

                    if let Some(tool_requests) = data
                        .and_then(|d| d.get("toolRequests"))
                        .and_then(|v| v.as_array())
                    {
                        for request in tool_requests {
                            let name = request
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("tool")
                                .to_string();

                            let input = request
                                .get("arguments")
                                .map(Self::normalize_copilot_tool_arguments)
                                .unwrap_or(serde_json::Value::Null);

                            Self::collect_copilot_filepaths(&input, &mut edited_filepaths);
                            transcript.add_message(Message::tool_use(name, input));
                        }
                    }
                }
                "tool.execution_start" => {
                    let name = data
                        .and_then(|d| d.get("toolName"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("tool")
                        .to_string();

                    let input = data
                        .and_then(|d| d.get("arguments"))
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);

                    Self::collect_copilot_filepaths(&input, &mut edited_filepaths);
                    transcript.add_message(Message::tool_use(name, input));
                }
                _ => {}
            }
        }

        Ok((transcript, detected_model, Some(edited_filepaths)))
    }

    fn normalize_copilot_tool_arguments(value: &serde_json::Value) -> serde_json::Value {
        if let Some(as_str) = value.as_str() {
            serde_json::from_str::<serde_json::Value>(as_str)
                .unwrap_or_else(|_| serde_json::Value::String(as_str.to_string()))
        } else {
            value.clone()
        }
    }

    fn collect_copilot_filepaths(value: &serde_json::Value, out: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, val) in map {
                    let key_lower = key.to_ascii_lowercase();
                    if (key_lower == "filepath"
                        || key_lower == "file_path"
                        || key_lower == "fspath"
                        || key_lower == "path")
                        && let Some(path) = val.as_str()
                    {
                        let normalized = path.replace('\\', "/");
                        if !out.contains(&normalized) {
                            out.push(normalized);
                        }
                    }
                    Self::collect_copilot_filepaths(val, out);
                }
            }
            serde_json::Value::Array(arr) => {
                for item in arr {
                    Self::collect_copilot_filepaths(item, out);
                }
            }
            serde_json::Value::String(s) => {
                Self::collect_apply_patch_paths_from_text(s, out);
            }
            _ => {}
        }
    }

    fn extract_copilot_model_hint(value: &serde_json::Value) -> Option<String> {
        match value {
            serde_json::Value::Object(map) => {
                if let Some(model_id) = map.get("modelId").and_then(|v| v.as_str())
                    && model_id.starts_with("copilot/")
                {
                    return Some(model_id.to_string());
                }
                if let Some(model) = map.get("model").and_then(|v| v.as_str())
                    && model.starts_with("copilot/")
                {
                    return Some(model.to_string());
                }
                if let Some(identifier) = map
                    .get("selectedModel")
                    .and_then(|v| v.get("identifier"))
                    .and_then(|v| v.as_str())
                    && identifier.starts_with("copilot/")
                {
                    return Some(identifier.to_string());
                }
                for val in map.values() {
                    if let Some(found) = Self::extract_copilot_model_hint(val) {
                        return Some(found);
                    }
                }
                None
            }
            serde_json::Value::Array(arr) => arr.iter().find_map(Self::extract_copilot_model_hint),
            serde_json::Value::String(s) => {
                if s.starts_with("copilot/") {
                    Some(s.to_string())
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}
