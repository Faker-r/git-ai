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
            prepare_agent_bash_pre_hook
        },
    },
    error::GitAiError,
    observability::log_error,
};
use std::collections::HashMap;
use std::path::Path;


pub struct GeminiPreset;

impl AgentCheckpointPreset for GeminiPreset {
    fn run(&self, flags: AgentCheckpointFlags) -> Result<AgentRunResult, GitAiError> {
        // Parse claude_hook_stdin as JSON
        let stdin_json = flags.hook_input.ok_or_else(|| {
            GitAiError::PresetError("hook_input is required for Gemini preset".to_string())
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

        // Parse into transcript and extract model
        let (transcript, model) =
            match GeminiPreset::transcript_and_model_from_gemini_json(transcript_path) {
                Ok((transcript, model)) => (transcript, model),
                Err(e) => {
                    eprintln!("[Warning] Failed to parse Gemini JSON: {e}");
                    log_error(
                        &e,
                        Some(serde_json::json!({
                            "agent_tool": "gemini",
                            "operation": "transcript_and_model_from_gemini_json"
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
            tool: "gemini".to_string(),
            id: session_id.to_string(),
            model: model.unwrap_or_else(|| "unknown".to_string()),
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
        let hook_event_name = hook_data
            .get("hook_event_name")
            .or_else(|| hook_data.get("hookEventName"))
            .and_then(|v| v.as_str());

        // Determine if this is a bash tool invocation
        let is_bash_tool = tool_name
            .map(|name| bash_tool::classify_tool(Agent::Gemini, name) == ToolClass::Bash)
            .unwrap_or(false);

        let tool_use_id = hook_data
            .get("tool_use_id")
            .or_else(|| hook_data.get("toolUseId"))
            .and_then(|v| v.as_str())
            .unwrap_or("bash");

        if hook_event_name == Some("BeforeTool") {
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
impl GeminiPreset {
    /// Parse a Gemini JSON file into a transcript and extract model info
    pub fn transcript_and_model_from_gemini_json(
        transcript_path: &str,
    ) -> Result<(AiTranscript, Option<String>), GitAiError> {
        let json_content = std::fs::read_to_string(transcript_path).map_err(GitAiError::IoError)?;
        let conversation: serde_json::Value =
            serde_json::from_str(&json_content).map_err(GitAiError::JsonError)?;

        let messages = conversation
            .get("messages")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                GitAiError::PresetError("messages array not found in Gemini JSON".to_string())
            })?;

        let mut transcript = AiTranscript::new();
        let mut model = None;

        for message in messages {
            let message_type = match message.get("type").and_then(|v| v.as_str()) {
                Some(t) => t,
                None => {
                    // Skip messages without a type field
                    continue;
                }
            };

            let timestamp = message
                .get("timestamp")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            match message_type {
                "user" => {
                    // Handle user messages - content can be a string
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
                "gemini" => {
                    // Extract model from gemini messages if we haven't found it yet
                    if model.is_none()
                        && let Some(model_str) = message.get("model").and_then(|v| v.as_str())
                    {
                        model = Some(model_str.to_string());
                    }

                    // Handle assistant text content - content can be a string
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

                    // Handle tool calls
                    if let Some(tool_calls) = message.get("toolCalls").and_then(|v| v.as_array()) {
                        for tool_call in tool_calls {
                            if let Some(name) = tool_call.get("name").and_then(|v| v.as_str()) {
                                // Extract args, defaulting to empty object if not present
                                let args = tool_call.get("args").cloned().unwrap_or_else(|| {
                                    serde_json::Value::Object(serde_json::Map::new())
                                });

                                let tool_timestamp = tool_call
                                    .get("timestamp")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string());

                                transcript.add_message(Message::ToolUse {
                                    name: name.to_string(),
                                    input: args,
                                    timestamp: tool_timestamp,
                                    id: None,
                                });
                            }
                        }
                    }
                }
                _ => {
                    // Skip unknown message types (info, error, warning, etc.)
                    continue;
                }
            }
        }

        Ok((transcript, model))
    }
}
