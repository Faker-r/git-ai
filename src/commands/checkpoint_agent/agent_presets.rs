use crate::{
    authorship::{
        transcript::{AiTranscript, Message},
        working_log::{AgentId, CheckpointKind},
    },
    commands::checkpoint_agent::bash_tool::{
        self, Agent, BashCheckpointAction, HookEvent, ToolClass,
    },
    error::GitAiError,
    observability::log_error,
    utils::normalize_to_posix,
};
use chrono::{Utc};
use dirs;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

pub struct AgentCheckpointFlags {
    pub hook_input: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentRunResult {
    // The raw hook input payload passed from the agent (JSON string or literal).
    pub agent_id: AgentId,
    pub agent_metadata: Option<HashMap<String, String>>,
    pub checkpoint_kind: CheckpointKind,
    pub transcript: Option<AiTranscript>, // the transcript of the conversation at the time of the hook event
    pub repo_working_dir: Option<String>,
    pub edited_filepaths: Option<Vec<String>>,
    pub will_edit_filepaths: Option<Vec<String>>,
    pub dirty_files: Option<HashMap<String, String>>,
    /// Pre-prepared captured checkpoint ID from bash tool (bypasses normal capture flow).
    pub captured_checkpoint_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BashPreHookStrategy {
    EmitHumanCheckpoint,
    SnapshotOnly,
}

pub(crate) enum BashPreHookResult {
    EmitHumanCheckpoint {
        captured_checkpoint_id: Option<String>,
    },
    SkipCheckpoint {
        captured_checkpoint_id: Option<String>,
    },
}

impl BashPreHookResult {
    pub(crate) fn captured_checkpoint_id(self) -> Option<String> {
        match self {
            Self::EmitHumanCheckpoint {
                captured_checkpoint_id,
            }
            | Self::SkipCheckpoint {
                captured_checkpoint_id,
            } => captured_checkpoint_id,
        }
    }
}

pub(crate) fn prepare_agent_bash_pre_hook(
    is_bash_tool: bool,
    repo_working_dir: Option<&str>,
    session_id: &str,
    tool_use_id: &str,
    agent_id: &AgentId,
    agent_metadata: Option<&HashMap<String, String>>,
    strategy: BashPreHookStrategy,
) -> Result<BashPreHookResult, GitAiError> {
    let captured_checkpoint_id = if is_bash_tool {
        if let Some(cwd) = repo_working_dir {
            match bash_tool::handle_bash_pre_tool_use_with_context(
                Path::new(cwd),
                session_id,
                tool_use_id,
                agent_id,
                agent_metadata,
            ) {
                Ok(result) => result.captured_checkpoint.map(|info| info.capture_id),
                Err(error) => {
                    tracing::debug!(
                        "Bash pre-hook snapshot failed for {} session {}: {}",
                        agent_id.tool,
                        session_id,
                        error
                    );
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    Ok(match strategy {
        BashPreHookStrategy::EmitHumanCheckpoint => BashPreHookResult::EmitHumanCheckpoint {
            captured_checkpoint_id,
        },
        BashPreHookStrategy::SnapshotOnly => BashPreHookResult::SkipCheckpoint {
            captured_checkpoint_id,
        },
    })
}

pub trait AgentCheckpointPreset {
    fn run(&self, flags: AgentCheckpointFlags) -> Result<AgentRunResult, GitAiError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prepare_agent_bash_pre_hook_swallows_snapshot_errors() {
        let temp = tempfile::tempdir().unwrap();
        let missing_repo = temp.path().join("missing-repo");
        let agent_id = AgentId {
            tool: "codex".to_string(),
            id: "session-1".to_string(),
            model: "gpt-5.4".to_string(),
        };

        let result = prepare_agent_bash_pre_hook(
            true,
            Some(missing_repo.to_string_lossy().as_ref()),
            "session-1",
            "tool-1",
            &agent_id,
            None,
            BashPreHookStrategy::EmitHumanCheckpoint,
        )
        .expect("pre-hook helper should treat snapshot failures as best-effort");

        match result {
            BashPreHookResult::EmitHumanCheckpoint {
                captured_checkpoint_id,
            } => {
                assert!(
                    captured_checkpoint_id.is_none(),
                    "failed pre-hook snapshot should not produce a captured checkpoint"
                );
            }
            BashPreHookResult::SkipCheckpoint { .. } => {
                panic!("expected EmitHumanCheckpoint result");
            }
        }
    }
}

/// Check if a file path refers to a Claude plan file.
///
/// Claude plans are written under `~/.claude/plans/`. We treat a path as a plan
/// file only when it:
/// - ends with `.md` (case-insensitive), and
/// - contains the path segment pair `.claude/plans` (platform-aware separators).
pub fn is_plan_file_path(file_path: &str) -> bool {
    let path = Path::new(file_path);
    let is_markdown = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"));
    if !is_markdown {
        false
    } else {
        let components: Vec<String> = path
            .components()
            .filter_map(|component| match component {
                Component::Normal(segment) => Some(segment.to_string_lossy().to_ascii_lowercase()),
                _ => None,
            })
            .collect();

        components
            .windows(2)
            .any(|window| window[0] == ".claude" && window[1] == "plans")
    }
}

/// Extract plan content from a Write or Edit tool_use input if it targets a plan file.
///
/// Maintains a running `plan_states` map keyed by file path so that Edit operations
/// can reconstruct the full plan text (not just the replaced fragment). On Write the
/// full content is stored; on Edit the old_string→new_string replacement is applied
/// to the tracked state and the complete result is returned.
///
/// Returns None if this is not a plan file edit.
pub fn extract_plan_from_tool_use(
    tool_name: &str,
    input: &serde_json::Value,
    plan_states: &mut std::collections::HashMap<String, String>,
) -> Option<String> {
    match tool_name {
        "Write" => {
            let file_path = input.get("file_path")?.as_str()?;
            if !is_plan_file_path(file_path) {
                return None;
            }
            let content = input.get("content")?.as_str()?;
            if content.trim().is_empty() {
                return None;
            }
            plan_states.insert(file_path.to_string(), content.to_string());
            Some(content.to_string())
        }
        "Edit" => {
            let file_path = input.get("file_path")?.as_str()?;
            if !is_plan_file_path(file_path) {
                return None;
            }
            let old_string = input.get("old_string").and_then(|v| v.as_str());
            let new_string = input.get("new_string").and_then(|v| v.as_str());

            match (old_string, new_string) {
                (Some(old), Some(new)) if !old.is_empty() || !new.is_empty() => {
                    // Apply the replacement to the tracked plan state if available
                    if let Some(current) = plan_states.get(file_path) {
                        let updated = current.replacen(old, new, 1);
                        plan_states.insert(file_path.to_string(), updated.clone());
                        Some(updated)
                    } else {
                        // No prior state tracked — store what we can and return the fragment
                        plan_states.insert(file_path.to_string(), new.to_string());
                        Some(new.to_string())
                    }
                }
                (None, Some(new)) if !new.is_empty() => {
                    plan_states.insert(file_path.to_string(), new.to_string());
                    Some(new.to_string())
                }
                _ => None,
            }
        }
        _ => None,
    }
}
pub struct WindsurfPreset;
impl AgentCheckpointPreset for WindsurfPreset {
    fn run(&self, flags: AgentCheckpointFlags) -> Result<AgentRunResult, GitAiError> {
        let stdin_json = flags.hook_input.ok_or_else(|| {
            GitAiError::PresetError("hook_input is required for Windsurf preset".to_string())
        })?;

        let hook_data: serde_json::Value = serde_json::from_str(&stdin_json)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        let trajectory_id = hook_data
            .get("trajectory_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                GitAiError::PresetError("trajectory_id not found in hook_input".to_string())
            })?;

        let agent_action_name = hook_data
            .get("agent_action_name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        // Extract cwd if present (Windsurf may or may not provide it)
        let cwd = hook_data
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Determine transcript path: either directly from tool_info or derived from trajectory_id
        let transcript_path = hook_data
            .get("tool_info")
            .and_then(|ti| ti.get("transcript_path"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                let home = dirs::home_dir().unwrap_or_default();
                home.join(".windsurf")
                    .join("transcripts")
                    .join(format!("{}.jsonl", trajectory_id))
                    .to_string_lossy()
                    .to_string()
            });

        // Extract model_name from hook payload (Windsurf provides this on every hook event)
        let hook_model = hook_data
            .get("model_name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty() && *s != "Unknown")
            .map(|s| s.to_string());

        // Parse transcript (best-effort)
        let (transcript, transcript_model) =
            match WindsurfPreset::transcript_and_model_from_windsurf_jsonl(&transcript_path) {
                Ok((transcript, model)) => (transcript, model),
                Err(e) => {
                    eprintln!("[Warning] Failed to parse Windsurf JSONL: {e}");
                    log_error(
                        &e,
                        Some(serde_json::json!({
                            "agent_tool": "windsurf",
                            "operation": "transcript_and_model_from_windsurf_jsonl"
                        })),
                    );
                    (crate::authorship::transcript::AiTranscript::new(), None)
                }
            };

        // Prefer hook-level model_name, fall back to transcript, then "unknown"
        let model = hook_model
            .or(transcript_model)
            .unwrap_or_else(|| "unknown".to_string());

        let agent_id = AgentId {
            tool: "windsurf".to_string(),
            id: trajectory_id.to_string(),
            model,
        };

        // Extract file_path from tool_info if present
        let file_path_as_vec = hook_data
            .get("tool_info")
            .and_then(|ti| ti.get("file_path"))
            .and_then(|v| v.as_str())
            .map(|path| vec![path.to_string()]);

        // Store transcript_path in metadata
        let agent_metadata =
            HashMap::from([("transcript_path".to_string(), transcript_path.to_string())]);

        // pre_write_code is the human checkpoint (before AI edit)
        if agent_action_name == "pre_write_code" {
            return Ok(AgentRunResult {
                agent_id,
                agent_metadata: None,
                checkpoint_kind: CheckpointKind::Human,
                transcript: None,
                repo_working_dir: cwd.clone(),
                edited_filepaths: None,
                will_edit_filepaths: file_path_as_vec,
                dirty_files: None,
                captured_checkpoint_id: None,
            });
        }

        // post_write_code and post_cascade_response_with_transcript are AI checkpoints
        Ok(AgentRunResult {
            agent_id,
            agent_metadata: Some(agent_metadata),
            checkpoint_kind: CheckpointKind::AiAgent,
            transcript: Some(transcript),
            repo_working_dir: cwd,
            edited_filepaths: file_path_as_vec,
            will_edit_filepaths: None,
            dirty_files: None,
            captured_checkpoint_id: None,
        })
    }
}
impl WindsurfPreset {
    /// Parse a Windsurf JSONL transcript file into a transcript.
    /// Each line is a JSON object with a "type" field.
    /// Model info is not present in the JSONL format — always returns None.
    /// (Model is instead provided via `model_name` in the hook payload.)
    pub fn transcript_and_model_from_windsurf_jsonl(
        transcript_path: &str,
    ) -> Result<(AiTranscript, Option<String>), GitAiError> {
        let content = std::fs::read_to_string(transcript_path).map_err(GitAiError::IoError)?;

        let mut transcript = AiTranscript::new();

        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let entry: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue, // skip malformed lines
            };

            let entry_type = match entry.get("type").and_then(|v| v.as_str()) {
                Some(t) => t,
                None => continue,
            };

            let timestamp = entry
                .get("timestamp")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            // Windsurf nests data under a key matching the type name,
            // e.g. {"type": "user_input", "user_input": {"user_response": "..."}}
            let inner = entry.get(entry_type);

            match entry_type {
                "user_input" => {
                    if let Some(text) = inner
                        .and_then(|v| v.get("user_response"))
                        .and_then(|v| v.as_str())
                    {
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            transcript.add_message(Message::User {
                                text: trimmed.to_string(),
                                timestamp,
                                id: None,
                            });
                        }
                    }
                }
                "planner_response" => {
                    if let Some(text) = inner
                        .and_then(|v| v.get("response"))
                        .and_then(|v| v.as_str())
                    {
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            transcript.add_message(Message::Assistant {
                                text: trimmed.to_string(),
                                timestamp,
                                id: None,
                            });
                        }
                    }
                }
                "code_action" => {
                    if let Some(action) = inner {
                        let path = action
                            .get("path")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        let new_content = action
                            .get("new_content")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);

                        transcript.add_message(Message::ToolUse {
                            name: "code_action".to_string(),
                            input: serde_json::json!({
                                "path": path,
                                "new_content": new_content,
                            }),
                            timestamp,
                            id: None,
                        });
                    }
                }
                "view_file" | "run_command" | "find" | "grep_search" | "list_directory"
                | "list_resources" => {
                    // Map all tool-like actions to ToolUse
                    let input = inner.cloned().unwrap_or(serde_json::json!({}));
                    transcript.add_message(Message::ToolUse {
                        name: entry_type.to_string(),
                        input,
                        timestamp,
                        id: None,
                    });
                }
                _ => {
                    // Skip truly unknown types silently
                    continue;
                }
            }
        }

        // Model info is not present in Windsurf JSONL format
        Ok((transcript, None))
    }
}
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


impl AgentCheckpointPreset for DroidPreset {
    fn run(&self, flags: AgentCheckpointFlags) -> Result<AgentRunResult, GitAiError> {
        // Parse hook_input JSON from Droid
        let hook_input_json = flags.hook_input.ok_or_else(|| {
            GitAiError::PresetError("hook_input is required for Droid preset".to_string())
        })?;

        let hook_data: serde_json::Value = serde_json::from_str(&hook_input_json)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        // Extract common fields from Droid hook input
        // Note: Droid may use either snake_case or camelCase field names
        // session_id is optional - generate a fallback if not present
        let session_id = hook_data
            .get("session_id")
            .and_then(|v| v.as_str())
            .or_else(|| hook_data.get("sessionId").and_then(|v| v.as_str()))
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                use std::time::{SystemTime, UNIX_EPOCH};
                format!(
                    "droid-{}",
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_millis()
                )
            });

        // transcript_path is optional - Droid may not always provide it
        let transcript_path = hook_data
            .get("transcript_path")
            .and_then(|v| v.as_str())
            .or_else(|| hook_data.get("transcriptPath").and_then(|v| v.as_str()));

        let cwd = hook_data
            .get("cwd")
            .and_then(|v| v.as_str())
            .ok_or_else(|| GitAiError::PresetError("cwd not found in hook_input".to_string()))?;

        let hook_event_name = hook_data
            .get("hookEventName")
            .and_then(|v| v.as_str())
            .or_else(|| hook_data.get("hook_event_name").and_then(|v| v.as_str()))
            .ok_or_else(|| {
                GitAiError::PresetError("hookEventName not found in hook_input".to_string())
            })?;

        // Extract tool_name and tool_input for tool-related events
        let tool_name = hook_data
            .get("tool_name")
            .and_then(|v| v.as_str())
            .or_else(|| hook_data.get("toolName").and_then(|v| v.as_str()));

        // Extract file_path from tool_input if present
        let tool_input = hook_data
            .get("tool_input")
            .or_else(|| hook_data.get("toolInput"));

        let mut file_path_as_vec = tool_input.and_then(|ti| {
            ti.get("file_path")
                .or_else(|| ti.get("filePath"))
                .and_then(|v| v.as_str())
                .map(|path| vec![path.to_string()])
        });

        // For ApplyPatch, extract file paths from the patch text
        // Patch format contains lines like: *** Update File: <path>
        if file_path_as_vec.is_none() && tool_name == Some("ApplyPatch") {
            let mut paths = Vec::new();

            // Try extracting from tool_input patch text
            if let Some(ti) = tool_input
                && let Some(patch_text) = ti
                    .as_str()
                    .or_else(|| ti.get("patch").and_then(|v| v.as_str()))
            {
                for line in patch_text.lines() {
                    let trimmed = line.trim();
                    if let Some(path) = trimmed
                        .strip_prefix("*** Update File: ")
                        .or_else(|| trimmed.strip_prefix("*** Add File: "))
                    {
                        paths.push(path.trim().to_string());
                    }
                }
            }

            // For PostToolUse, also try parsing tool_response for file_path
            if paths.is_empty()
                && hook_event_name == "PostToolUse"
                && let Some(tool_response) = hook_data
                    .get("tool_response")
                    .or_else(|| hook_data.get("toolResponse"))
            {
                // tool_response might be a JSON string or an object
                let response_obj = if let Some(s) = tool_response.as_str() {
                    serde_json::from_str::<serde_json::Value>(s).ok()
                } else {
                    Some(tool_response.clone())
                };
                if let Some(obj) = response_obj
                    && let Some(path) = obj
                        .get("file_path")
                        .or_else(|| obj.get("filePath"))
                        .and_then(|v| v.as_str())
                {
                    paths.push(path.to_string());
                }
            }

            if !paths.is_empty() {
                file_path_as_vec = Some(paths);
            }
        }

        // Resolve transcript and settings paths:
        // 1. Use transcript_path from hook input if provided
        // 2. Otherwise derive from session_id + cwd
        let (resolved_transcript_path, resolved_settings_path) = if let Some(tp) = transcript_path {
            // Derive settings path as sibling of transcript_path
            let settings = tp.replace(".jsonl", ".settings.json");
            (tp.to_string(), settings)
        } else {
            let (jsonl_p, settings_p) = DroidPreset::droid_session_paths(&session_id, cwd);
            (
                jsonl_p.to_string_lossy().to_string(),
                settings_p.to_string_lossy().to_string(),
            )
        };

        // Parse the Droid transcript JSONL file
        let transcript =
            match DroidPreset::transcript_and_model_from_droid_jsonl(&resolved_transcript_path) {
                Ok((transcript, _model)) => transcript,
                Err(e) => {
                    eprintln!("[Warning] Failed to parse Droid JSONL: {e}");
                    log_error(
                        &e,
                        Some(serde_json::json!({
                            "agent_tool": "droid",
                            "operation": "transcript_and_model_from_droid_jsonl"
                        })),
                    );
                    crate::authorship::transcript::AiTranscript::new()
                }
            };

        // Extract model from settings.json
        let model = match DroidPreset::model_from_droid_settings_json(&resolved_settings_path) {
            Ok(m) => m.unwrap_or_else(|| "unknown".to_string()),
            Err(_) => "unknown".to_string(),
        };

        let agent_id = AgentId {
            tool: "droid".to_string(),
            id: session_id,
            model,
        };

        // Store both paths in metadata
        let mut agent_metadata = HashMap::new();
        agent_metadata.insert(
            "transcript_path".to_string(),
            resolved_transcript_path.clone(),
        );
        agent_metadata.insert("settings_path".to_string(), resolved_settings_path.clone());
        if let Some(name) = tool_name {
            agent_metadata.insert("tool_name".to_string(), name.to_string());
        }

        // Determine if this is a bash tool invocation
        let is_bash_tool = tool_name
            .map(|name| bash_tool::classify_tool(Agent::Droid, name) == ToolClass::Bash)
            .unwrap_or(false);

        let tool_use_id = hook_data
            .get("tool_use_id")
            .or_else(|| hook_data.get("toolUseId"))
            .and_then(|v| v.as_str())
            .unwrap_or("bash");

        // Check if this is a PreToolUse event (human checkpoint)
        if hook_event_name == "PreToolUse" {
            let pre_hook_captured_id = prepare_agent_bash_pre_hook(
                is_bash_tool,
                Some(cwd),
                &agent_id.id,
                tool_use_id,
                &agent_id,
                Some(&agent_metadata),
                BashPreHookStrategy::EmitHumanCheckpoint,
            )?
            .captured_checkpoint_id();
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
                &agent_id.id,
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

        // PostToolUse event - AI checkpoint
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

impl DroidPreset {
    /// Parse a Droid JSONL transcript file into a transcript.
    /// Droid JSONL uses the same nested format as Claude Code:
    /// `{"type":"message","timestamp":"...","message":{"role":"user|assistant","content":[...]}}`
    /// Model is NOT stored in the JSONL — it comes from the companion .settings.json file.
    pub fn transcript_and_model_from_droid_jsonl(
        transcript_path: &str,
    ) -> Result<(AiTranscript, Option<String>), GitAiError> {
        let jsonl_content =
            std::fs::read_to_string(transcript_path).map_err(GitAiError::IoError)?;
        let mut transcript = AiTranscript::new();
        let mut plan_states = std::collections::HashMap::new();

        for line in jsonl_content.lines() {
            if line.trim().is_empty() {
                continue;
            }

            let raw_entry: serde_json::Value = serde_json::from_str(line)?;

            // Only process "message" entries; skip session_start, todo_state, etc.
            if raw_entry["type"].as_str() != Some("message") {
                continue;
            }

            let timestamp = raw_entry["timestamp"].as_str().map(|s| s.to_string());

            let message = &raw_entry["message"];
            let role = match message["role"].as_str() {
                Some(r) => r,
                None => continue,
            };

            match role {
                "user" => {
                    if let Some(content_array) = message["content"].as_array() {
                        for item in content_array {
                            // Skip tool_result items — those are system-generated responses
                            if item["type"].as_str() == Some("tool_result") {
                                continue;
                            }
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
                    } else if let Some(content) = message["content"].as_str()
                        && !content.trim().is_empty()
                    {
                        transcript.add_message(Message::User {
                            text: content.to_string(),
                            timestamp: timestamp.clone(),
                            id: None,
                        });
                    }
                }
                "assistant" => {
                    if let Some(content_array) = message["content"].as_array() {
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
                                            transcript.add_message(Message::ToolUse {
                                                name: name.to_string(),
                                                input: item["input"].clone(),
                                                timestamp: timestamp.clone(),
                                                id: None,
                                            });
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

        // Model is not in the JSONL — return None
        Ok((transcript, None))
    }

    /// Read the model from a Droid .settings.json file
    pub fn model_from_droid_settings_json(
        settings_path: &str,
    ) -> Result<Option<String>, GitAiError> {
        let content = std::fs::read_to_string(settings_path).map_err(GitAiError::IoError)?;
        let settings: serde_json::Value =
            serde_json::from_str(&content).map_err(GitAiError::JsonError)?;
        Ok(settings["model"].as_str().map(|s| s.to_string()))
    }

    /// Derive JSONL and settings.json paths from a session_id and cwd.
    /// Droid stores sessions at ~/.factory/sessions/{encoded_cwd}/{session_id}.jsonl
    /// where encoded_cwd replaces '/' with '-'.
    pub fn droid_session_paths(session_id: &str, cwd: &str) -> (PathBuf, PathBuf) {
        let encoded_cwd = cwd.replace('/', "-");
        let base = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("~"))
            .join(".factory")
            .join("sessions")
            .join(&encoded_cwd);
        let jsonl_path = base.join(format!("{}.jsonl", session_id));
        let settings_path = base.join(format!("{}.settings.json", session_id));
        (jsonl_path, settings_path)
    }
}

pub struct AiTabPreset;

// Droid (Factory) to checkpoint preset
pub struct DroidPreset;

#[derive(Debug, Deserialize)]
struct AiTabHookInput {
    hook_event_name: String,
    tool: String,
    model: String,
    repo_working_dir: Option<String>,
    will_edit_filepaths: Option<Vec<String>>,
    edited_filepaths: Option<Vec<String>>,
    completion_id: Option<String>,
    dirty_files: Option<HashMap<String, String>>,
}

impl AgentCheckpointPreset for AiTabPreset {
    fn run(&self, flags: AgentCheckpointFlags) -> Result<AgentRunResult, GitAiError> {
        let hook_input_json = flags.hook_input.ok_or_else(|| {
            GitAiError::PresetError("hook_input is required for ai_tab preset".to_string())
        })?;

        let hook_input: AiTabHookInput = serde_json::from_str(&hook_input_json)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        let AiTabHookInput {
            hook_event_name,
            tool,
            model,
            repo_working_dir,
            will_edit_filepaths,
            edited_filepaths,
            completion_id,
            dirty_files,
        } = hook_input;

        if hook_event_name != "before_edit" && hook_event_name != "after_edit" {
            return Err(GitAiError::PresetError(format!(
                "Unsupported hook_event_name '{}' for ai_tab preset (expected 'before_edit' or 'after_edit')",
                hook_event_name
            )));
        }

        let tool = tool.trim().to_string();
        if tool.is_empty() {
            return Err(GitAiError::PresetError(
                "tool must be a non-empty string for ai_tab preset".to_string(),
            ));
        }

        let model = model.trim().to_string();
        if model.is_empty() {
            return Err(GitAiError::PresetError(
                "model must be a non-empty string for ai_tab preset".to_string(),
            ));
        }

        let repo_working_dir = repo_working_dir
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let agent_id = AgentId {
            tool,
            id: format!(
                "ai_tab-{}",
                completion_id.unwrap_or_else(|| Utc::now().timestamp_millis().to_string())
            ),
            model,
        };

        if hook_event_name == "before_edit" {
            return Ok(AgentRunResult {
                agent_id,
                agent_metadata: None,
                checkpoint_kind: CheckpointKind::Human,
                transcript: None,
                repo_working_dir,
                edited_filepaths: None,
                will_edit_filepaths,
                dirty_files,
                captured_checkpoint_id: None,
            });
        }

        Ok(AgentRunResult {
            agent_id,
            agent_metadata: None,
            checkpoint_kind: CheckpointKind::AiTab,
            transcript: None,
            repo_working_dir,
            edited_filepaths,
            will_edit_filepaths: None,
            dirty_files,
            captured_checkpoint_id: None,
        })
    }
}

// Firebender to checkpoint preset
pub struct FirebenderPreset;

#[derive(Debug, Deserialize)]
struct FirebenderHookInput {
    hook_event_name: String,
    model: String,
    repo_working_dir: Option<String>,
    workspace_roots: Option<Vec<String>>,
    tool_name: Option<String>,
    tool_input: Option<serde_json::Value>,
    completion_id: Option<String>,
    dirty_files: Option<HashMap<String, String>>,
}

impl FirebenderPreset {
    fn push_unique_path(paths: &mut Vec<String>, candidate: &str) {
        let trimmed = candidate.trim();
        if !trimmed.is_empty() && !paths.iter().any(|path| path == trimmed) {
            paths.push(trimmed.to_string());
        }
    }

    fn normalize_hook_path(raw_path: &str, cwd: &str) -> Option<String> {
        let trimmed = raw_path.trim();
        if trimmed.is_empty() {
            return None;
        }

        let normalized_path = normalize_to_posix(trimmed);
        let normalized_cwd = normalize_to_posix(cwd.trim())
            .trim_end_matches('/')
            .to_string();

        if normalized_cwd.is_empty() {
            return Some(normalized_path);
        }

        let relative = if normalized_path == normalized_cwd {
            String::new()
        } else if let Some(stripped) = normalized_path.strip_prefix(&(normalized_cwd.clone() + "/"))
        {
            stripped.to_string()
        } else {
            normalized_path
        };

        Some(relative)
    }

    fn extract_patch_paths(patch: &str) -> Vec<String> {
        let mut paths = Vec::new();

        for line in patch.lines() {
            for prefix in [
                "*** Add File: ",
                "*** Update File: ",
                "*** Delete File: ",
                "*** Move to: ",
            ] {
                if let Some(path) = line.strip_prefix(prefix) {
                    Self::push_unique_path(&mut paths, path);
                }
            }
        }

        paths
    }

    // Firebender emits multiple real tool_input shapes across editing flows.
    // Normalize direct file fields, structured patch payloads, and raw apply-patch
    // text into a single edited-file list for checkpointing.
    fn extract_file_paths(tool_input: &serde_json::Value) -> Option<Vec<String>> {
        let mut paths = Vec::new();

        match tool_input {
            serde_json::Value::Object(_) => {
                for key in [
                    "file_path",
                    "target_file",
                    "relative_workspace_path",
                    "path",
                ] {
                    if let Some(path) = tool_input.get(key).and_then(|v| v.as_str()) {
                        Self::push_unique_path(&mut paths, path);
                    }
                }

                if let Some(patch) = tool_input.get("patch").and_then(|v| v.as_str()) {
                    for path in Self::extract_patch_paths(patch) {
                        Self::push_unique_path(&mut paths, &path);
                    }
                }
            }
            serde_json::Value::String(raw_patch) => {
                for path in Self::extract_patch_paths(raw_patch) {
                    Self::push_unique_path(&mut paths, &path);
                }
            }
            _ => {}
        }

        if paths.is_empty() { None } else { Some(paths) }
    }
}

impl AgentCheckpointPreset for FirebenderPreset {
    fn run(&self, flags: AgentCheckpointFlags) -> Result<AgentRunResult, GitAiError> {
        let hook_input_json = flags.hook_input.ok_or_else(|| {
            GitAiError::PresetError("hook_input is required for firebender preset".to_string())
        })?;

        let hook_input: FirebenderHookInput = serde_json::from_str(&hook_input_json)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        let FirebenderHookInput {
            hook_event_name,
            model,
            repo_working_dir,
            workspace_roots,
            tool_name,
            tool_input,
            completion_id,
            dirty_files,
        } = hook_input;

        if hook_event_name == "beforeSubmitPrompt" || hook_event_name == "afterFileEdit" {
            std::process::exit(0);
        }

        if hook_event_name != "preToolUse" && hook_event_name != "postToolUse" {
            return Err(GitAiError::PresetError(format!(
                "Invalid hook_event_name: {}. Expected 'preToolUse' or 'postToolUse'",
                hook_event_name
            )));
        }

        let tool_name = tool_name.unwrap_or_default();
        // Firebender hooks fire for all tool calls (no matcher in hooks.json). Silently
        // skip tools that don't edit files or run shell commands.
        // Firebender hooks emit canonical hook tool names rather than raw function names.
        // For example, `apply_patch` and `local_search_replace` both come through as `Edit`.
        let tool_class = bash_tool::classify_tool(Agent::Firebender, tool_name.as_str());
        if tool_class == ToolClass::Skip {
            std::process::exit(0);
        }
        let is_bash_tool = tool_class == ToolClass::Bash;

        let repo_working_dir = repo_working_dir
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| workspace_roots.and_then(|roots| roots.into_iter().next()));

        let tool_input = tool_input.unwrap_or(serde_json::Value::Null);
        let file_paths = Self::extract_file_paths(&tool_input).map(|paths| {
            if let Some(cwd) = repo_working_dir.as_deref() {
                paths
                    .into_iter()
                    .filter_map(|path| Self::normalize_hook_path(&path, cwd))
                    .collect::<Vec<String>>()
            } else {
                paths
            }
        });

        let model = {
            let m = model.trim().to_string();
            if m.is_empty() {
                "unknown".to_string()
            } else {
                m
            }
        };

        let session_id = completion_id
            .clone()
            .unwrap_or_else(|| Utc::now().timestamp_millis().to_string());

        let agent_id = AgentId {
            tool: "firebender".to_string(),
            id: format!("firebender-{}", session_id),
            model,
        };

        if hook_event_name == "preToolUse" {
            let pre_hook_captured_id = prepare_agent_bash_pre_hook(
                is_bash_tool,
                repo_working_dir.as_deref(),
                &session_id,
                "bash",
                &agent_id,
                None,
                BashPreHookStrategy::EmitHumanCheckpoint,
            )?
            .captured_checkpoint_id();
            return Ok(AgentRunResult {
                agent_id,
                agent_metadata: None,
                checkpoint_kind: CheckpointKind::Human,
                transcript: None,
                repo_working_dir,
                edited_filepaths: None,
                will_edit_filepaths: file_paths.clone(),
                dirty_files,
                captured_checkpoint_id: pre_hook_captured_id,
            });
        }

        let bash_result = if is_bash_tool {
            repo_working_dir.as_deref().map(|cwd| {
                bash_tool::handle_bash_tool(
                    HookEvent::PostToolUse,
                    Path::new(cwd),
                    &session_id,
                    "bash",
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

        Ok(AgentRunResult {
            agent_id,
            agent_metadata: None,
            checkpoint_kind: CheckpointKind::AiAgent,
            transcript: None,
            repo_working_dir,
            edited_filepaths,
            will_edit_filepaths: None,
            dirty_files,
            captured_checkpoint_id: bash_captured_checkpoint_id,
        })
    }
}
