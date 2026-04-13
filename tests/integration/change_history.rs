use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::authorship::authorship_log_serialization::ChangeHistoryEntry;
use insta::assert_debug_snapshot;

/// Sanitize non-deterministic fields in change_history entries so snapshots are stable.
/// Keeps: kind, agent_type, model, files (with all line details), line_stats.
/// Normalizes: timestamp -> 0, conversation_id -> Some("CONV_ID") or None,
///             prompt_id -> None, prompt_text -> None.
fn sanitize_change_history(entries: &[ChangeHistoryEntry]) -> Vec<ChangeHistoryEntry> {
    entries
        .iter()
        .map(|e| ChangeHistoryEntry {
            timestamp: 0,
            kind: e.kind.clone(),
            conversation_id: e.conversation_id.as_ref().map(|_| "CONV_ID".to_string()),
            agent_type: e.agent_type.clone(),
            prompt_id: None,
            prompt_text: None,
            model: e.model.clone(),
            files: e.files.clone(),
            line_stats: e.line_stats.clone(),
        })
        .collect()
}

/// Big test: AI creates a file, human edits it, AI creates another file, then commit.
/// Validates the full change_history structure including per-file line ranges/contents,
/// checkpoint kinds (ai_agent vs human), agent metadata, and line_stats.
#[test]
fn mixed_ai_and_human_edits_change_history() {
    let repo = TestRepo::new();

    // Base commit so HEAD exists
    let mut readme = repo.filename("README.md");
    readme.set_contents(crate::lines!["# Project"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI creates feature.txt with 3 lines
    let mut feature = repo.filename("feature.txt");
    feature.set_contents(crate::lines![
        "fn main() {".ai(),
        "    println!(\"hello\");".ai(),
        "}".ai(),
    ]);

    // Human edits feature.txt: replace line 2, add line 4
    // Write directly to disk — pre-commit will capture this as a human change
    std::fs::write(
        feature.file_path.clone(),
        "fn main() {\n    println!(\"goodbye\");\n}\nfn helper() {}",
    )
    .unwrap();

    // AI creates config.txt with 2 lines
    let mut config = repo.filename("config.txt");
    config.set_contents(crate::lines!["key = value".ai(), "debug = true".ai(),]);

    // Commit — pre-commit captures human edits, post-commit builds change_history
    let commit = repo.stage_all_and_commit("Add features").unwrap();

    let change_history = commit
        .authorship_log
        .metadata
        .change_history
        .expect("change_history should be present");

    let sanitized = sanitize_change_history(&change_history);
    assert_debug_snapshot!(sanitized);
}

/// Edge case: only human edits, no AI involvement.
/// The pre-commit early exit was removed for research, so human-only changes
/// should still produce change_history entries with line ranges.
#[test]
fn only_human_edits_change_history() {
    let repo = TestRepo::new();

    let mut file = repo.filename("notes.txt");
    file.set_contents(crate::lines!["Line 1", "Line 2"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Human adds a line — write directly, no checkpoint
    std::fs::write(file.file_path.clone(), "Line 1\nLine 2\nLine 3").unwrap();

    let commit = repo.stage_all_and_commit("Human edit").unwrap();

    let change_history = commit
        .authorship_log
        .metadata
        .change_history
        .expect("change_history should be present for human-only edits");

    let sanitized = sanitize_change_history(&change_history);
    assert_debug_snapshot!(sanitized);
}

/// Edge case: only AI edits, no human modifications after.
/// The pre-commit human checkpoint should still appear but with no new changes
/// to the AI-created file (the AI checkpoint already captured the final state).
#[test]
fn only_ai_edits_change_history() {
    let repo = TestRepo::new();

    let mut readme = repo.filename("README.md");
    readme.set_contents(crate::lines!["# Project"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI creates a file
    let mut generated = repo.filename("generated.txt");
    generated.set_contents(crate::lines![
        "auto line 1".ai(),
        "auto line 2".ai(),
        "auto line 3".ai(),
    ]);

    let commit = repo.stage_all_and_commit("AI only").unwrap();

    let change_history = commit
        .authorship_log
        .metadata
        .change_history
        .expect("change_history should be present for AI-only edits");

    let sanitized = sanitize_change_history(&change_history);
    assert_debug_snapshot!(sanitized);
}

/// Edge case: AI creates a file (checkpointed but never committed), then the file
/// is deleted from disk before commit. Our fix re-includes previously-checkpointed
/// files that were deleted, so the change_history should show the deletion.
#[test]
fn deleted_untracked_file_change_history() {
    let repo = TestRepo::new();

    let mut readme = repo.filename("README.md");
    readme.set_contents(crate::lines!["# Project"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI creates a file and checkpoints it (never committed to git)
    let mut ephemeral = repo.filename("ephemeral.txt");
    ephemeral.set_contents(crate::lines![
        "temporary line 1".ai(),
        "temporary line 2".ai(),
        "temporary line 3".ai(),
    ]);

    // Delete the file before committing
    std::fs::remove_file(ephemeral.file_path.clone()).expect("should delete file");

    let commit = repo.stage_all_and_commit("Delete ephemeral").unwrap();

    let change_history = commit
        .authorship_log
        .metadata
        .change_history
        .expect("change_history should be present");

    // Find the entry that references the deleted file
    let has_deleted_entry = change_history.iter().any(|entry| {
        if let Some(detail) = entry.files.get("ephemeral.txt") {
            !detail.deleted_lines.is_empty() && !detail.deleted_line_contents.is_empty()
        } else {
            false
        }
    });

    assert!(
        has_deleted_entry,
        "change_history should contain a deletion entry for ephemeral.txt with deleted_lines and deleted_line_contents.\nGot: {:#?}",
        change_history
    );

    let sanitized = sanitize_change_history(&change_history);
    assert_debug_snapshot!(sanitized);
}
