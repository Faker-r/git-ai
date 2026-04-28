
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
            BashPreHookStrategy, 
            extract_plan_from_tool_use, prepare_agent_bash_pre_hook
        },
        github_copilot_preset::GithubCopilotPreset,
    },
    error::GitAiError,
    observability::log_error,
};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path};


const CLAUDE_HOOK_PRE_TOOL_USE: &str = "PreToolUse";
const CLAUDE_HOOK_POST_TOOL_USE: &str = "PostToolUse";

#[derive(Debug, Deserialize)]
struct ClaudeHookInput {
    #[serde(alias = "hookEventName")]
    hook_event_name: Option<String>,
    session_id: Option<String>,
    transcript_path: Option<String>,
    cwd: Option<String>,
    #[serde(alias = "toolName")]
    tool_name: Option<String>,
    tool_input: Option<serde_json::Value>,
}

// Claude Code to checkpoint preset
pub struct ClaudePreset;

impl AgentCheckpointPreset for ClaudePreset {
    fn run(&self, flags: AgentCheckpointFlags) -> Result<AgentRunResult, GitAiError> {
        // Parse claude_hook_stdin as JSON
        let hook_input_json = flags.hook_input.ok_or_else(|| {
            GitAiError::PresetError("hook_input is required for Claude preset".to_string())
        })?;

        let hook_data: serde_json::Value = serde_json::from_str(&hook_input_json)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;
        tracing::debug!("ClaudePreset: hook_data={:?}", hook_data);

        // VS Code Copilot hooks can be imported into Claude settings.
        // An incoming hook input payload might comes from Claude or VS Code/GitHub Copilot. 
        // We need to detect payloads from Copilot and ignore them because
        // dedicated VS Code/GitHub Copilot hooks should handle them directly.
        if ClaudePreset::is_vscode_copilot_hook_payload(&hook_data) {
            return Err(GitAiError::PresetError(
                "Skipping VS Code hook payload in Claude preset; use github-copilot/vscode hooks."
                    .to_string(),
            ));
        }
        if ClaudePreset::is_cursor_hook_payload(&hook_data) {
            return Err(GitAiError::PresetError(
                "Skipping Cursor hook payload in Claude preset; use cursor hooks.".to_string(),
            ));
        }

        let hook_input: ClaudeHookInput = serde_json::from_str(&hook_input_json)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;
        tracing::debug!("ClaudeHookInput: hook_input={:?}", hook_input);


        let ClaudeHookInput {
            hook_event_name,
            session_id,
            transcript_path,
            cwd,
            tool_name,
            tool_input,
        } = hook_input;


        let transcript_path = transcript_path.ok_or_else(|| {
                GitAiError::PresetError("transcript_path not found in hook_input".to_string())
            })?;

        let cwd = cwd.ok_or_else(|| {
            GitAiError::PresetError("cwd not found in hook_input".to_string())
        })?;
        
        // Extract session_id for bash tool snapshot correlation
        // It is reported that sessin_id changes when resuming an existing session
        // thus we fallback to the filename if the session_id is not provided
        // or if two values are different
        let mut session_id = session_id.unwrap_or_default();

        // Extract the ID from the filename
        // Example: /Users/aidancunniffe/.claude/projects/-Users-aidancunniffe-Desktop-ghq/cb947e5b-246e-4253-a953-631f7e464c6b.jsonl
        let path = Path::new(transcript_path.as_str());
        // Example: cb947e5b-246e-4253-a953-631f7e464c6b
        let filename = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| {
                GitAiError::PresetError(
                    "Could not extract filename from transcript_path".to_string(),
                )
        })?;

        // Session ID should be the same as filename
        if filename != session_id {
            tracing::error!(
                "Session ID mismatch: filename '{}' != session_id '{}'. Falling back to filename as session_id.",
                filename, session_id
            );
            session_id = filename.to_string();
       
        }

        // Extract hook_event_name from the JSON, a common hook input field
        let hook_event_name = hook_event_name.ok_or_else(|| {
            GitAiError::PresetError("hook_event_name not found in hook_input".to_string())
        })?;
        
        // Validate hook_event_name
        if hook_event_name != CLAUDE_HOOK_PRE_TOOL_USE 
            && hook_event_name != CLAUDE_HOOK_POST_TOOL_USE 
        {
            return Err(GitAiError::PresetError(format!(
                "Invalid hook_event_name: {}. Expected '{}' or '{}'",
                hook_event_name, CLAUDE_HOOK_PRE_TOOL_USE, CLAUDE_HOOK_POST_TOOL_USE
            )));
        }
        
        let tool_name = tool_name.unwrap_or_default();
        let tool_class = bash_tool::classify_tool(Agent::Claude, tool_name.as_str());
        
        // // Only checkpoint on file-mutating tools.
        // if tool_class == ToolClass::Skip {
        //     tracing::debug!(
        //         "ClaudePreset: tool_name '{}' is not a file-mutating tool, skipping checkpoint",
        //         tool_name
        //     );
        //     std::process::exit(0);
        // }

        let is_bash_tool = tool_class == ToolClass::Bash;

        let agent_id = AgentId {
            tool: "claude".to_string(),
            id: session_id.clone(),
            model: "unknown".to_string(),
        };

        let file_paths = tool_input
            .as_ref()
            .and_then(|ti| ti.get("file_path"))
            .and_then(|v| v.as_str())
            .map(|path| vec![path.to_string()]);

        if hook_event_name == CLAUDE_HOOK_PRE_TOOL_USE {
            let pre_hook_captured_id = prepare_agent_bash_pre_hook(
                is_bash_tool,
                Some(cwd.as_str()),
                &session_id,
                "bash",
                &agent_id,
                None,
                BashPreHookStrategy::EmitHumanCheckpoint,
            )?
            .captured_checkpoint_id();

            // Early return for human checkpoint
            return Ok(AgentRunResult {
                agent_id,
                agent_metadata: None,
                checkpoint_kind: CheckpointKind::Human,
                transcript: None,
                repo_working_dir: Some(cwd.to_string()),
                edited_filepaths: None,
                will_edit_filepaths: file_paths,
                dirty_files: None,
                captured_checkpoint_id: pre_hook_captured_id,
            });
        }

        let bash_result = if is_bash_tool {
            let repo_root = Path::new(cwd.as_str());
            Some(bash_tool::handle_bash_tool(
                HookEvent::PostToolUse,
                repo_root,
                &session_id,
                "bash",
            ))
        } else {
            None
        };

        let edited_filepaths = if is_bash_tool {
            match bash_result.as_ref().unwrap().as_ref().map(|r| &r.action) {
                Ok(BashCheckpointAction::Checkpoint(paths)) => Some(paths.clone()),
                Ok(BashCheckpointAction::NoChanges) => None,
                Ok(BashCheckpointAction::Fallback) => {
                    // snapshot unavailable or repo too large; no paths to report
                    None
                }
                Ok(BashCheckpointAction::TakePreSnapshot) => None, // shouldn't happen on post
                Err(e) => {
                    tracing::debug!("Bash tool post-hook error: {}", e);
                    None
                }
            }
        } else {
            file_paths
        };

        let bash_captured_checkpoint_id = bash_result
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .and_then(|r| r.captured_checkpoint.as_ref())
            .map(|info| info.capture_id.clone());


        // Parse into transcript and extract model
        let (transcript, model) =
            match ClaudePreset::transcript_and_model_from_claude_code_jsonl(transcript_path.clone().as_str()) {
                Ok((transcript, model)) => (transcript, model),
                Err(e) => {
                    eprintln!("[Warning] Failed to parse Claude JSONL: {e}");
                    log_error(
                        &e,
                        Some(serde_json::json!({
                            "agent_tool": "claude",
                            "operation": "transcript_and_model_from_claude_code_jsonl"
                        })),
                    );
                    (
                        crate::authorship::transcript::AiTranscript::new(),
                        Some("unknown".to_string()),
                    )
                }
            };

        // The filename should be a UUID
        let agent_id = AgentId {
            tool: "claude".to_string(),
            id: session_id.to_string(),
            model: model.unwrap_or_else(|| "unknown".to_string()),
        };

        // Store transcript_path in metadata
        let agent_metadata =
            HashMap::from([("transcript_path".to_string(), transcript_path.to_string())]);

        Ok(AgentRunResult {
            agent_id,
            agent_metadata: Some(agent_metadata),
            checkpoint_kind: CheckpointKind::AiAgent,
            transcript: Some(transcript),
            repo_working_dir: Some(cwd.to_string()),
            edited_filepaths,
            will_edit_filepaths: None,
            dirty_files: None,
            captured_checkpoint_id: bash_captured_checkpoint_id,
        })
    }
}

impl ClaudePreset {
    fn is_vscode_copilot_hook_payload(hook_data: &serde_json::Value) -> bool {
        let transcript_path = GithubCopilotPreset::transcript_path_from_hook_data(hook_data);
        match transcript_path {
            Some(path) if GithubCopilotPreset::looks_like_claude_transcript_path(path) => false,
            Some(path) => GithubCopilotPreset::looks_like_copilot_transcript_path(path),
            None => false,
        }
    }

    fn is_cursor_hook_payload(hook_data: &serde_json::Value) -> bool {
        if hook_data.get("cursor_version").is_some() {
            return true;
        }

        let transcript_path = GithubCopilotPreset::transcript_path_from_hook_data(hook_data);
        match transcript_path {
            Some(path) if GithubCopilotPreset::looks_like_claude_transcript_path(path) => false,
            Some(path) => ClaudePreset::looks_like_cursor_transcript_path(path),
            None => false,
        }
    }

    fn looks_like_cursor_transcript_path(path: &str) -> bool {
        let normalized = path.replace('\\', "/").to_ascii_lowercase();
        normalized.contains("/.cursor/projects/") && normalized.contains("/agent-transcripts/")
    }

    /// Parse a Claude Code JSONL file into a transcript and extract model info
    pub fn transcript_and_model_from_claude_code_jsonl(
        transcript_path: &str,
    ) -> Result<(AiTranscript, Option<String>), GitAiError> {
        let jsonl_content =
            std::fs::read_to_string(transcript_path).map_err(GitAiError::IoError)?;
        let mut transcript = AiTranscript::new();
        let mut model = None;
        let mut plan_states = std::collections::HashMap::new();

        for line in jsonl_content.lines() {
            if !line.trim().is_empty() {
                // Parse the raw JSONL entry
                let raw_entry: serde_json::Value = serde_json::from_str(line)?;
                let timestamp = raw_entry["timestamp"].as_str().map(|s| s.to_string());

                // Extract model from assistant messages if we haven't found it yet
                if model.is_none()
                    && raw_entry["type"].as_str() == Some("assistant")
                    && let Some(model_str) = raw_entry["message"]["model"].as_str()
                {
                    model = Some(model_str.to_string());
                }

                // Extract messages based on the type
                match raw_entry["type"].as_str() {
                    Some("user") => {
                        // Handle user messages
                        if let Some(content) = raw_entry["message"]["content"].as_str() {
                            if !content.trim().is_empty() {
                                transcript.add_message(Message::User {
                                    text: content.to_string(),
                                    timestamp: timestamp.clone(),
                                    id: None,
                                });
                            }
                        } else if let Some(content_array) =
                            raw_entry["message"]["content"].as_array()
                        {
                            // Handle user messages with content array
                            for item in content_array {
                                // Skip tool_result items - those are system-generated responses, not human input
                                if item["type"].as_str() == Some("tool_result") {
                                    continue;
                                }
                                // Handle text content blocks from actual user input
                                if item["type"].as_str() == Some("text")
                                    && let Some(text) = item["text"].as_str()
                                    && !text.trim().is_empty()
                                {
                                    transcript.add_message(Message::User {
                                        text: text.to_string(),
                                        timestamp: timestamp.clone(),
                                        id: None,
                                    });
                                }
                            }
                        }
                    }
                    Some("assistant") => {
                        // Handle assistant messages
                        if let Some(content_array) = raw_entry["message"]["content"].as_array() {
                            for item in content_array {
                                match item["type"].as_str() {
                                    Some("text") => {
                                        if let Some(text) = item["text"].as_str()
                                            && !text.trim().is_empty()
                                        {
                                            transcript.add_message(Message::Assistant {
                                                text: text.to_string(),
                                                timestamp: timestamp.clone(),
                                                id: None,
                                            });
                                        }
                                    }
                                    Some("thinking") => {
                                        if let Some(thinking) = item["thinking"].as_str()
                                            && !thinking.trim().is_empty()
                                        {
                                            transcript.add_message(Message::Assistant {
                                                text: thinking.to_string(),
                                                timestamp: timestamp.clone(),
                                                id: None,
                                            });
                                        }
                                    }
                                    Some("tool_use") => {
                                        if let (Some(name), Some(_input)) =
                                            (item["name"].as_str(), item["input"].as_object())
                                        {
                                            // Check if this is a Write/Edit to a plan file
                                            if let Some(plan_text) = extract_plan_from_tool_use(
                                                name,
                                                &item["input"],
                                                &mut plan_states,
                                            ) {
                                                transcript.add_message(Message::Plan {
                                                    text: plan_text,
                                                    timestamp: timestamp.clone(),
                                                    id: None,
                                                });
                                            } else {
                                                transcript.add_message(Message::tool_use_with_timestamp(
                                                    name.to_string(),
                                                    item["input"].clone(),
                                                    timestamp.clone(),
                                                ));
                                            }
                                        }
                                    }
                                    // YW TODO: add thinking messages, if any are present
                                    _ => continue, // Skip unknown content types
                                }
                            }
                        }
                    }
                    _ => continue, // Skip unknown message types
                }
            }
        }

        Ok((transcript, model))
    }
}
