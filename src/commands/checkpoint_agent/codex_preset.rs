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
use dirs;
use glob::glob;
use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};



pub struct CodexPreset;

impl AgentCheckpointPreset for CodexPreset {
    fn run(&self, flags: AgentCheckpointFlags) -> Result<AgentRunResult, GitAiError> {
        let stdin_json = flags.hook_input.ok_or_else(|| {
            GitAiError::PresetError("hook_input is required for Codex preset".to_string())
        })?;

        let hook_data: serde_json::Value = serde_json::from_str(&stdin_json)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        let session_id = CodexPreset::session_id_from_hook_data(&hook_data).ok_or_else(|| {
            GitAiError::PresetError("session_id/thread_id not found in hook_input".to_string())
        })?;

        let cwd = hook_data
            .get("cwd")
            .and_then(|v| v.as_str())
            .ok_or_else(|| GitAiError::PresetError("cwd not found in hook_input".to_string()))?;

        let transcript_path = hook_data
            .get("transcript_path")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(
                || match CodexPreset::find_latest_rollout_path_for_session(&session_id) {
                    Ok(Some(path)) => Some(path.to_string_lossy().to_string()),
                    Ok(None) => None,
                    Err(e) => {
                        eprintln!(
                            "[Warning] Failed to locate Codex rollout for session {session_id}: {e}"
                        );
                        log_error(
                            &e,
                            Some(serde_json::json!({
                                "agent_tool": "codex",
                                "operation": "find_latest_rollout_path_for_session"
                            })),
                        );
                        None
                    }
                },
            );

        let (transcript, model) = if let Some(path) = transcript_path.as_deref() {
            match CodexPreset::transcript_and_model_from_codex_rollout_jsonl(path) {
                Ok((transcript, model)) => (transcript, model),
                Err(e) => {
                    eprintln!("[Warning] Failed to parse Codex rollout JSONL: {e}");
                    log_error(
                        &e,
                        Some(serde_json::json!({
                            "agent_tool": "codex",
                            "operation": "transcript_and_model_from_codex_rollout_jsonl"
                        })),
                    );
                    (AiTranscript::new(), Some("unknown".to_string()))
                }
            }
        } else {
            eprintln!(
                "[Warning] No Codex rollout path found for session {session_id}; continuing with empty transcript"
            );
            (AiTranscript::new(), Some("unknown".to_string()))
        };

        let hook_event_name = hook_data
            .get("hook_event_name")
            .or_else(|| hook_data.get("hookEventName"))
            .and_then(|v| v.as_str());
        let tool_name = hook_data
            .get("tool_name")
            .and_then(|v| v.as_str())
            .or_else(|| hook_data.get("toolName").and_then(|v| v.as_str()));
        let is_bash_tool = tool_name
            .map(|name| bash_tool::classify_tool(Agent::Codex, name) == ToolClass::Bash)
            .unwrap_or(false);
        let tool_use_id = hook_data
            .get("tool_use_id")
            .or_else(|| hook_data.get("toolUseId"))
            .and_then(|v| v.as_str())
            .unwrap_or("bash");

        let agent_id = AgentId {
            tool: "codex".to_string(),
            id: session_id.clone(),
            model: model.unwrap_or_else(|| "unknown".to_string()),
        };

        let agent_metadata =
            transcript_path.map(|path| HashMap::from([("transcript_path".to_string(), path)]));

        match hook_event_name {
            Some("PreToolUse") => {
                if !is_bash_tool {
                    return Err(GitAiError::PresetError(format!(
                        "Skipping Codex PreToolUse for unsupported tool {}",
                        tool_name.unwrap_or("unknown")
                    )));
                }

                let pre_hook_captured_id = prepare_agent_bash_pre_hook(
                    true,
                    Some(cwd),
                    &session_id,
                    tool_use_id,
                    &agent_id,
                    agent_metadata.as_ref(),
                    BashPreHookStrategy::SnapshotOnly,
                )?
                .captured_checkpoint_id();

                if pre_hook_captured_id.is_some() {
                    tracing::debug!(
                        "Codex PreToolUse captured a bash pre-snapshot but will skip emitting a checkpoint",
                    );
                }

                return Err(GitAiError::PresetError(
                    "Skipping Codex PreToolUse checkpoint; stored bash pre-snapshot only."
                        .to_string(),
                ));
            }
            Some("PostToolUse") => {
                if !is_bash_tool {
                    return Err(GitAiError::PresetError(format!(
                        "Skipping Codex PostToolUse for unsupported tool {}",
                        tool_name.unwrap_or("unknown")
                    )));
                }

                let repo_root = Path::new(cwd);
                let bash_result = bash_tool::handle_bash_tool(
                    HookEvent::PostToolUse,
                    repo_root,
                    &session_id,
                    tool_use_id,
                );
                let edited_filepaths = match bash_result.as_ref().map(|result| &result.action) {
                    Ok(BashCheckpointAction::Checkpoint(paths)) => Some(paths.clone()),
                    Ok(BashCheckpointAction::NoChanges) => None,
                    Ok(BashCheckpointAction::Fallback) => None,
                    Ok(BashCheckpointAction::TakePreSnapshot) => None,
                    Err(e) => {
                        tracing::debug!("Codex bash post-hook error: {}", e);
                        None
                    }
                };
                let bash_captured_checkpoint_id = bash_result
                    .as_ref()
                    .ok()
                    .and_then(|result| result.captured_checkpoint.as_ref())
                    .map(|info| info.capture_id.clone());

                return Ok(AgentRunResult {
                    agent_id,
                    agent_metadata,
                    checkpoint_kind: CheckpointKind::AiAgent,
                    transcript: Some(transcript),
                    repo_working_dir: Some(cwd.to_string()),
                    edited_filepaths,
                    will_edit_filepaths: None,
                    dirty_files: None,
                    captured_checkpoint_id: bash_captured_checkpoint_id,
                });
            }
            Some("Stop") | None => {}
            Some(other) => {
                return Err(GitAiError::PresetError(format!(
                    "Unsupported Codex hook_event_name: {}",
                    other
                )));
            }
        }

        Ok(AgentRunResult {
            agent_id,
            agent_metadata,
            checkpoint_kind: CheckpointKind::AiAgent,
            transcript: Some(transcript),
            repo_working_dir: Some(cwd.to_string()),
            edited_filepaths: None,
            will_edit_filepaths: None,
            dirty_files: None,
            captured_checkpoint_id: None,
        })
    }
}

impl CodexPreset {
    fn session_id_from_hook_data(hook_data: &serde_json::Value) -> Option<String> {
        hook_data
            .get("session_id")
            .and_then(|v| v.as_str())
            .or_else(|| hook_data.get("thread_id").and_then(|v| v.as_str()))
            .or_else(|| hook_data.get("thread-id").and_then(|v| v.as_str()))
            .or_else(|| {
                hook_data
                    .get("hook_event")
                    .and_then(|ev| ev.get("thread_id"))
                    .and_then(|v| v.as_str())
            })
            .map(|s| s.to_string())
    }

    pub fn codex_home_dir() -> PathBuf {
        if let Ok(codex_home) = env::var("CODEX_HOME")
            && !codex_home.trim().is_empty()
        {
            return PathBuf::from(codex_home);
        }

        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("~"))
            .join(".codex")
    }

    pub fn find_latest_rollout_path_for_session(
        session_id: &str,
    ) -> Result<Option<PathBuf>, GitAiError> {
        Self::find_latest_rollout_path_for_session_in_home(session_id, &Self::codex_home_dir())
    }

    pub fn find_latest_rollout_path_for_session_in_home(
        session_id: &str,
        codex_home: &Path,
    ) -> Result<Option<PathBuf>, GitAiError> {
        let mut candidates = Vec::new();
        for subdir in ["sessions", "archived_sessions"] {
            let base = codex_home.join(subdir);
            if !base.exists() {
                continue;
            }

            let pattern = format!(
                "{}/**/rollout-*{}*.jsonl",
                base.to_string_lossy(),
                session_id
            );
            let entries = glob(&pattern).map_err(|e| {
                GitAiError::Generic(format!("Failed to glob Codex rollout files: {e}"))
            })?;

            for entry in entries.flatten() {
                if entry.is_file() {
                    candidates.push(entry);
                }
            }
        }

        let newest = candidates.into_iter().max_by_key(|path| {
            std::fs::metadata(path)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH)
        });

        Ok(newest)
    }

    pub fn transcript_and_model_from_codex_rollout_jsonl(
        transcript_path: &str,
    ) -> Result<(AiTranscript, Option<String>), GitAiError> {
        let jsonl_content =
            std::fs::read_to_string(transcript_path).map_err(GitAiError::IoError)?;

        let mut parsed_lines: Vec<serde_json::Value> = Vec::new();
        for line in jsonl_content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let value: serde_json::Value = serde_json::from_str(trimmed)?;
            parsed_lines.push(value);
        }

        let mut transcript = AiTranscript::new();
        let mut model = None;

        for entry in &parsed_lines {
            let timestamp = entry
                .get("timestamp")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let item_type = entry
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let payload = entry.get("payload").unwrap_or(entry);

            match item_type {
                "turn_context" => {
                    if let Some(model_name) = payload.get("model").and_then(|v| v.as_str())
                        && !model_name.trim().is_empty()
                    {
                        // Keep the latest model for sessions that switched models mid-thread.
                        model = Some(model_name.to_string());
                    }
                }
                "response_item" => {
                    let response_type = payload
                        .get("type")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    match response_type {
                        "message" => {
                            let role = payload
                                .get("role")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default();

                            let mut text_parts: Vec<String> = Vec::new();
                            if let Some(content_arr) =
                                payload.get("content").and_then(|v| v.as_array())
                            {
                                for item in content_arr {
                                    let content_type = item
                                        .get("type")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or_default();
                                    if (role == "assistant" || role == "user")
                                        && (content_type == "output_text"
                                            || content_type == "input_text")
                                        && let Some(text) =
                                            item.get("text").and_then(|v| v.as_str())
                                    {
                                        let trimmed = text.trim();
                                        if !trimmed.is_empty() {
                                            text_parts.push(trimmed.to_string());
                                        }
                                    }
                                }
                            }

                            if !text_parts.is_empty() {
                                let joined = text_parts.join("\n");
                                if role == "user" {
                                    transcript.add_message(Message::User {
                                        text: joined,
                                        timestamp: timestamp.clone(),
                                        id: None,
                                    });
                                } else if role == "assistant" {
                                    transcript.add_message(Message::Assistant {
                                        text: joined,
                                        timestamp: timestamp.clone(),
                                        id: None,
                                    });
                                }
                            }
                        }
                        "function_call" | "custom_tool_call" | "local_shell_call"
                        | "web_search_call" => {
                            let name = payload
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or(response_type)
                                .to_string();

                            let input = if response_type == "function_call" {
                                if let Some(arguments) =
                                    payload.get("arguments").and_then(|v| v.as_str())
                                {
                                    serde_json::from_str::<serde_json::Value>(arguments)
                                        .unwrap_or_else(|_| {
                                            serde_json::Value::String(arguments.to_string())
                                        })
                                } else {
                                    payload.get("arguments").cloned().unwrap_or_else(|| {
                                        serde_json::Value::Object(serde_json::Map::new())
                                    })
                                }
                            } else if let Some(input) =
                                payload.get("input").and_then(|v| v.as_str())
                            {
                                serde_json::Value::String(input.to_string())
                            } else {
                                payload.clone()
                            };

                            transcript.add_message(Message::ToolUse {
                                name,
                                input,
                                timestamp: timestamp.clone(),
                                id: None,
                            });
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }

        if transcript.messages().is_empty() {
            // Backward-compatible fallback for sessions that only recorded legacy event messages.
            for entry in &parsed_lines {
                let timestamp = entry
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                if entry.get("type").and_then(|v| v.as_str()) != Some("event_msg") {
                    continue;
                }

                let payload = entry.get("payload").unwrap_or(entry);
                let event_type = payload
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();

                if event_type == "user_message" {
                    if let Some(text) = payload.get("message").and_then(|v| v.as_str()) {
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            transcript.add_message(Message::User {
                                text: trimmed.to_string(),
                                timestamp: timestamp.clone(),
                                id: None,
                            });
                        }
                    }
                } else if event_type == "agent_message"
                    && let Some(text) = payload.get("message").and_then(|v| v.as_str())
                {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        transcript.add_message(Message::Assistant {
                            text: trimmed.to_string(),
                            timestamp: timestamp.clone(),
                            id: None,
                        });
                    }
                }
            }
        }

        Ok((transcript, model))
    }
}

