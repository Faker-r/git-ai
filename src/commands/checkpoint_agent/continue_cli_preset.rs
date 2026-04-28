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
            BashPreHookStrategy, prepare_agent_bash_pre_hook
        }
    },
    error::GitAiError,
    observability::log_error,
};
use std::collections::HashMap;
use std::path::Path;

pub struct ContinueCliPreset;

impl AgentCheckpointPreset for ContinueCliPreset {
    fn run(&self, flags: AgentCheckpointFlags) -> Result<AgentRunResult, GitAiError> {
        // Parse hook_input as JSON
        let stdin_json = flags.hook_input.ok_or_else(|| {
            GitAiError::PresetError("hook_input is required for Continue CLI preset".to_string())
        })?;

        let hook_data: serde_json::Value = serde_json::from_str(&stdin_json)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        let session_id = hook_data
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                GitAiError::PresetError("session_id not found in hook_input".to_string())
            })?;

        let transcript_path = hook_data
            .get("transcript_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                GitAiError::PresetError("transcript_path not found in hook_input".to_string())
            })?;

        let cwd = hook_data
            .get("cwd")
            .and_then(|v| v.as_str())
            .ok_or_else(|| GitAiError::PresetError("cwd not found in hook_input".to_string()))?;

        // Extract tool_name for bash tool classification
        let tool_name = hook_data
            .get("tool_name")
            .and_then(|v| v.as_str())
            .or_else(|| hook_data.get("toolName").and_then(|v| v.as_str()));

        // Extract model from hook_input (required)
        let model = hook_data
            .get("model")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                eprintln!("[Warning] Continue CLI: 'model' field not found in hook_input, defaulting to 'unknown'");
                eprintln!("[Debug] hook_data keys: {:?}", hook_data.as_object().map(|obj| obj.keys().collect::<Vec<_>>()));
                "unknown".to_string()
            });

        eprintln!("[Debug] Continue CLI using model: {}", model);

        // Parse transcript from JSON file
        let transcript = match ContinueCliPreset::transcript_from_continue_json(transcript_path) {
            Ok(transcript) => transcript,
            Err(e) => {
                eprintln!("[Warning] Failed to parse Continue CLI JSON: {e}");
                log_error(
                    &e,
                    Some(serde_json::json!({
                        "agent_tool": "continue-cli",
                        "operation": "transcript_from_continue_json"
                    })),
                );
                crate::authorship::transcript::AiTranscript::new()
            }
        };

        // The session_id is the unique identifier for this conversation
        let agent_id = AgentId {
            tool: "continue-cli".to_string(),
            id: session_id.to_string(),
            model,
        };

        // Extract file_path from tool_input if present
        let file_path_as_vec = hook_data
            .get("tool_input")
            .and_then(|ti| ti.get("file_path"))
            .and_then(|v| v.as_str())
            .map(|path| vec![path.to_string()]);

        // Store transcript_path in metadata
        let agent_metadata =
            HashMap::from([("transcript_path".to_string(), transcript_path.to_string())]);

        // Check if this is a PreToolUse event (human checkpoint)
        let hook_event_name = hook_data.get("hook_event_name").and_then(|v| v.as_str());

        // Determine if this is a bash tool invocation
        let is_bash_tool = tool_name
            .map(|name| bash_tool::classify_tool(Agent::ContinueCli, name) == ToolClass::Bash)
            .unwrap_or(false);

        let tool_use_id = hook_data
            .get("tool_use_id")
            .or_else(|| hook_data.get("toolUseId"))
            .and_then(|v| v.as_str())
            .unwrap_or("bash");

        if hook_event_name == Some("PreToolUse") {
            let pre_hook_captured_id = prepare_agent_bash_pre_hook(
                is_bash_tool,
                Some(cwd),
                session_id,
                tool_use_id,
                &agent_id,
                Some(&agent_metadata),
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
                will_edit_filepaths: file_path_as_vec,
                dirty_files: None,
                captured_checkpoint_id: pre_hook_captured_id,
            });
        }

        // PostToolUse: for bash tools, diff snapshots to detect changed files
        let bash_result = if is_bash_tool {
            let repo_root = Path::new(cwd);
            Some(bash_tool::handle_bash_tool(
                HookEvent::PostToolUse,
                repo_root,
                session_id,
                tool_use_id,
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
                Ok(BashCheckpointAction::TakePreSnapshot) => None,
                Err(e) => {
                    tracing::debug!("Bash tool post-hook error: {}", e);
                    None
                }
            }
        } else {
            file_path_as_vec
        };

        let bash_captured_checkpoint_id = bash_result
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .and_then(|r| r.captured_checkpoint.as_ref())
            .map(|info| info.capture_id.clone());

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

impl ContinueCliPreset {
    /// Parse a Continue CLI JSON file into a transcript
    pub fn transcript_from_continue_json(
        transcript_path: &str,
    ) -> Result<AiTranscript, GitAiError> {
        let json_content = std::fs::read_to_string(transcript_path).map_err(GitAiError::IoError)?;
        let conversation: serde_json::Value =
            serde_json::from_str(&json_content).map_err(GitAiError::JsonError)?;

        let history = conversation
            .get("history")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                GitAiError::PresetError("history array not found in Continue CLI JSON".to_string())
            })?;

        let mut transcript = AiTranscript::new();

        for history_item in history {
            // Extract the message from the history item
            let message = match history_item.get("message") {
                Some(m) => m,
                None => continue, // Skip items without a message
            };

            let role = match message.get("role").and_then(|v| v.as_str()) {
                Some(r) => r,
                None => continue, // Skip messages without a role
            };

            // Extract timestamp from message if available
            let timestamp = message
                .get("timestamp")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            match role {
                "user" => {
                    // Handle user messages - content is a string
                    if let Some(content) = message.get("content").and_then(|v| v.as_str()) {
                        let trimmed = content.trim();
                        if !trimmed.is_empty() {
                            transcript.add_message(Message::User {
                                text: trimmed.to_string(),
                                timestamp: timestamp.clone(),
                                id: None,
                            });
                        }
                    }
                }
                "assistant" => {
                    // Handle assistant text content
                    if let Some(content) = message.get("content").and_then(|v| v.as_str()) {
                        let trimmed = content.trim();
                        if !trimmed.is_empty() {
                            transcript.add_message(Message::Assistant {
                                text: trimmed.to_string(),
                                timestamp: timestamp.clone(),
                                id: None,
                            });
                        }
                    }

                    // Handle tool calls from the message
                    if let Some(tool_calls) = message.get("toolCalls").and_then(|v| v.as_array()) {
                        for tool_call in tool_calls {
                            if let Some(function) = tool_call.get("function") {
                                let tool_name = function
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("unknown");

                                // Parse the arguments JSON string
                                let args = if let Some(args_str) =
                                    function.get("arguments").and_then(|v| v.as_str())
                                {
                                    serde_json::from_str::<serde_json::Value>(args_str)
                                        .unwrap_or_else(|_| {
                                            serde_json::Value::Object(serde_json::Map::new())
                                        })
                                } else {
                                    serde_json::Value::Object(serde_json::Map::new())
                                };

                                let tool_timestamp = tool_call
                                    .get("timestamp")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string());

                                transcript.add_message(Message::ToolUse {
                                    name: tool_name.to_string(),
                                    input: args,
                                    timestamp: tool_timestamp,
                                    id: None,
                                });
                            }
                        }
                    }
                }
                _ => {
                    // Skip unknown roles
                    continue;
                }
            }
        }

        Ok(transcript)
    }
}

