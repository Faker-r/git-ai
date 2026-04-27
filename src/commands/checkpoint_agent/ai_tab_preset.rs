use crate::{
    authorship::{
        working_log::{AgentId, CheckpointKind},
    },
    commands::checkpoint_agent::{
        agent_presets::{
            AgentCheckpointFlags, AgentCheckpointPreset, AgentRunResult, 
        },
    },
    error::GitAiError,
};
use chrono::{Utc};
use serde::{Deserialize};
use std::collections::HashMap;

pub struct AiTabPreset;

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
