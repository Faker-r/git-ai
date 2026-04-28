use crate::{
    authorship::{
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
    utils::normalize_to_posix,
};
use chrono::{Utc};
use serde::{Deserialize};
use std::collections::HashMap;
use std::path::{Path};


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
