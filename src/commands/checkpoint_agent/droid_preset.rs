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
    },
    error::GitAiError,
    observability::log_error,
};
use dirs;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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

// Droid (Factory) to checkpoint preset
pub struct DroidPreset;
