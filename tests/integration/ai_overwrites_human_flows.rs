use crate::repos::test_repo::TestRepo;


fn setup_repo_with_committed_a_md() -> TestRepo {
    let repo = TestRepo::new();

    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["initial".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("initial commit should succeed");

    std::fs::write(repo.path().join("a.md"), "initial text\n").expect("write a.md should succeed");
    repo.stage_all_and_commit("add baseline a.md")
        .expect("baseline commit should succeed");

    repo
}

fn assert_contains_added_text(
    change_history: &[git_ai::authorship::authorship_log_serialization::ChangeHistoryEntry],
    entry_idx: usize,
    expected_text: &str,
) {
    let file = change_history[entry_idx]
        .files
        .get("a.md")
        .expect("entry should include a.md");
    assert!(
        file.added_line_contents
            .iter()
            .any(|line| line.contains(expected_text)),
        "entry {entry_idx} should include added text '{expected_text}' in a.md, got: {:#?}",
        file
    );
}

fn assert_contains_deleted_text(
    change_history: &[git_ai::authorship::authorship_log_serialization::ChangeHistoryEntry],
    entry_idx: usize,
    expected_text: &str,
) {
    let file = change_history[entry_idx]
        .files
        .get("a.md")
        .expect("entry should include a.md");
    assert!(
        file.deleted_line_contents
            .iter()
            .any(|line| line.contains(expected_text)),
        "entry {entry_idx} should include deleted text '{expected_text}' in a.md, got: {:#?}",
        file
    );
}


#[test]
fn test_ai_modifies_human_added_line() {
    // Scenario:
    // 1) Test repository has a a.md file with one line "initial text", no uncommitted changes
    // 2) Human writes in a.md line 2 as "human generated text"
    // 3) AI overwrites a.md line 2 as "AI overwritten text"
    // 4) User stages the file and commits
    //
    // Expected: final committed line 2 is "AI overwritten text".
    // git-notes for the commit should contain two change_history entries:
    // 1) human generated text
    // 2) ai generated text
    let repo = setup_repo_with_committed_a_md();

    std::fs::write(repo.path().join("a.md"), "initial text\nhuman generated text\n")
        .expect("write human-updated a.md should succeed");
    repo.git_ai(&["checkpoint", "--", "a.md"])
        .expect("human checkpoint should succeed");

    std::fs::write(repo.path().join("a.md"), "initial text\nAI overwritten text\n")
        .expect("write AI-overwritten a.md should succeed");
    repo.git_ai(&["checkpoint", "mock_ai", "a.md"])
        .expect("AI checkpoint should succeed");

    let commit = repo
        .stage_all_and_commit("ai modifies human added line")
        .expect("commit should succeed");

    let a_contents =
        std::fs::read_to_string(repo.path().join("a.md")).expect("read a.md should succeed");
    assert_eq!(a_contents, "initial text\nAI overwritten text\n");

    let change_history = commit
        .authorship_log
        .metadata
        .change_history
        .expect("change_history should be present");
    assert_eq!(change_history.len(), 2, "expected exactly two entries");
    assert_eq!(change_history[0].kind, "human");
    assert_eq!(change_history[1].kind, "ai_agent");
    assert_contains_added_text(&change_history, 0, "human generated text");
    assert_contains_deleted_text(&change_history, 1, "human generated text");
    assert_contains_added_text(&change_history, 1, "AI overwritten text");
}


#[test]
fn test_ai_deletes_all_human_added_line() {
    // Scenario:
    // 1) Test repository has a a.md file with one line "initial text", no uncommitted changes
    // 2) Human writes in a.md line 2 as "human generated text"
    // 3) AI deletes a.md line 2 (i.e., all human-modified lines)
    // note that, till here, the repo working space is clean as all changes were reverted
    // to make a commit, we have to create some changes in the repo
    // 4) AI creates new file b.md with one line "dummy text"
    // 5) User stages files a.md and b.md and commits
    //
    // Expected: final committed a.md file line 2 is empty.
    // git-notes for the commit should contain change_history entries where:
    // 1) human generated text for a.md
    // 2) ai deleted human-generated text in a.md
    // 3) ai added text in b.md
    let repo = setup_repo_with_committed_a_md();

    std::fs::write(repo.path().join("a.md"), "initial text\nhuman generated text\n")
        .expect("write human-updated a.md should succeed");
    repo.git_ai(&["checkpoint", "--", "a.md"])
        .expect("human checkpoint should succeed");

    std::fs::write(repo.path().join("a.md"), "initial text\n")
        .expect("write AI-deleted a.md should succeed");
    repo.git_ai(&["checkpoint", "mock_ai", "a.md"])
        .expect("AI checkpoint for a.md should succeed");
    std::fs::write(repo.path().join("b.md"), "dummy text\n").expect("write b.md should succeed");
    repo.git_ai(&["checkpoint", "mock_ai", "b.md"])
        .expect("AI checkpoint for b.md should succeed");

    let commit = repo
        .stage_all_and_commit("ai deletes human added line and adds b.md")
        .expect("commit should succeed");

    let a_contents =
        std::fs::read_to_string(repo.path().join("a.md")).expect("read a.md should succeed");
    assert_eq!(a_contents, "initial text\n");
    let b_contents =
        std::fs::read_to_string(repo.path().join("b.md")).expect("read b.md should succeed");
    assert_eq!(b_contents, "dummy text\n");

    let change_history = commit
        .authorship_log
        .metadata
        .change_history
        .expect("change_history should be present");
    assert_eq!(change_history.len(), 3, "expected exactly three entries");
    assert_eq!(change_history[0].kind, "human");
    assert_eq!(change_history[1].kind, "ai_agent");
    assert_eq!(change_history[2].kind, "ai_agent");
    assert_contains_added_text(&change_history, 0, "human generated text");
    let ai_has_a_deletion = change_history[1..].iter().any(|entry| {
        entry
            .files
            .get("a.md")
            .map(|a_file| {
                a_file
                    .deleted_line_contents
                    .iter()
                    .any(|line| line.contains("human generated text"))
            })
            .unwrap_or(false)
    });
    assert!(
        ai_has_a_deletion,
        "one AI entry should include deleted human-generated text for a.md, got: {:#?}",
        &change_history[1..]
    );
    let ai_has_b_addition = change_history[1..].iter().any(|entry| {
        entry
            .files
            .get("b.md")
            .map(|b_file| {
                b_file
                    .added_line_contents
                    .iter()
                    .any(|line| line.contains("dummy text"))
            })
            .unwrap_or(false)
    });
    assert!(
        ai_has_b_addition,
        "one AI entry should include added dummy text for b.md, got: {:#?}",
        &change_history[1..]
    );
}

#[test]
fn test_ai_deletes_human_added_line_2() {
    // Scenario:
    // 1) Test repository has a a.md file with one line "initial text", no uncommitted changes
    // 2) Human writes in a.md line 2 as "human generated line 2" and line 3 as "human generated line 3"
    // 3) AI deletes a.md line 2
    // 4) User stages the file and commits
    //
    // Expected: final committed a.md file has two lines: "initial text" and "human generated line 3".
    // git-notes for the commit should contain two change_history entries:
    // 1) human generated text
    // 2) ai deleted human-generated line 2
    let repo = setup_repo_with_committed_a_md();

    std::fs::write(
        repo.path().join("a.md"),
        "initial text\nhuman generated line 2\nhuman generated line 3\n",
    )
    .expect("write human-updated a.md should succeed");
    repo.git_ai(&["checkpoint", "--", "a.md"])
        .expect("human checkpoint should succeed");

    std::fs::write(repo.path().join("a.md"), "initial text\nhuman generated line 3\n")
        .expect("write AI-deleted line 2 should succeed");
    repo.git_ai(&["checkpoint", "mock_ai", "a.md"])
        .expect("AI checkpoint should succeed");

    let commit = repo
        .stage_all_and_commit("ai deletes human added line 2")
        .expect("commit should succeed");

    let a_contents =
        std::fs::read_to_string(repo.path().join("a.md")).expect("read a.md should succeed");
    assert_eq!(a_contents, "initial text\nhuman generated line 3\n");

    let change_history = commit
        .authorship_log
        .metadata
        .change_history
        .expect("change_history should be present");
    assert_eq!(change_history.len(), 2, "expected exactly two entries");
    assert_eq!(change_history[0].kind, "human");
    assert_eq!(change_history[1].kind, "ai_agent");
    assert_contains_added_text(&change_history, 0, "human generated line 2");
    assert_contains_added_text(&change_history, 0, "human generated line 3");
    assert_contains_deleted_text(&change_history, 1, "human generated line 2");
}


#[test]
fn test_ai_modifies_human_modified_line() {
    // Scenario:
    // 1) Test repository has a a.md file with one line "initial text", no uncommitted changes
    // 2) Human changes a.md line 1 to "human generated text"
    // 3) AI overwrites a.md line 1 as "ai overwritten text"
    // 4) User stages the file and commits
    //
    // Expected: final committed line 1 is "ai overwritten text".
    // git-notes for the commit should contain two change_history entries:
    // 1) human generated text
    // 2) ai deleted human-generated line 2
    let repo = setup_repo_with_committed_a_md();

    std::fs::write(repo.path().join("a.md"), "human generated text\n")
        .expect("write human-updated a.md should succeed");
    repo.git_ai(&["checkpoint", "--", "a.md"])
        .expect("human checkpoint should succeed");

    std::fs::write(repo.path().join("a.md"), "ai overwritten text\n")
        .expect("write AI-overwritten a.md should succeed");
    repo.git_ai(&["checkpoint", "mock_ai", "a.md"])
        .expect("AI checkpoint should succeed");

    let commit = repo
        .stage_all_and_commit("ai modifies human modified line")
        .expect("commit should succeed");

    let a_contents =
        std::fs::read_to_string(repo.path().join("a.md")).expect("read a.md should succeed");
    assert_eq!(a_contents, "ai overwritten text\n");

    let change_history = commit
        .authorship_log
        .metadata
        .change_history
        .expect("change_history should be present");
    assert_eq!(change_history.len(), 2, "expected exactly two entries");
    assert_eq!(change_history[0].kind, "human");
    assert_eq!(change_history[1].kind, "ai_agent");
    assert_contains_added_text(&change_history, 0, "human generated text");
    assert_contains_deleted_text(&change_history, 1, "human generated text");
    assert_contains_added_text(&change_history, 1, "ai overwritten text");
}


#[test]
fn test_ai_deletes_human_modified_line() {
    // Scenario:
    // 1) Test repository has a a.md file with one line "initial text", no uncommitted changes
    // 2) human changes a.md line 1 to "human generated text"
    // 3) AI deletes a.md line 1
    // 4) User stages the file and commits
    //
    // Expected: final committed line 1 is empty.
    // git-notes for the commit should contain one change_history entry:
    // 1) human generated text. 
    // 2) ai deleted human text.
    let repo = setup_repo_with_committed_a_md();

    std::fs::write(repo.path().join("a.md"), "human generated text\n")
        .expect("write human-updated a.md should succeed");
    repo.git_ai(&["checkpoint", "--", "a.md"])
        .expect("human checkpoint should succeed");

    std::fs::write(repo.path().join("a.md"), "").expect("AI deletes line 1 should succeed");
    repo.git_ai(&["checkpoint", "mock_ai", "a.md"])
        .expect("AI checkpoint should succeed");

    let commit = repo
        .stage_all_and_commit("ai deletes human modified line")
        .expect("commit should succeed");

    let a_contents =
        std::fs::read_to_string(repo.path().join("a.md")).expect("read a.md should succeed");
    assert_eq!(a_contents, "");

    let change_history = commit
        .authorship_log
        .metadata
        .change_history
        .expect("change_history should be present");
    assert_eq!(change_history.len(), 2, "expected two entries");
    assert_eq!(change_history[0].kind, "human");
    assert_eq!(change_history[1].kind, "ai_agent");
    assert_contains_added_text(&change_history, 0, "human generated text");
    assert_contains_deleted_text(&change_history, 1, "human generated text");
}



#[test]
fn test_ai_reverts_human_deleted_line() {
    // Scenario:
    // 1) Test repository has a a.md file with one line "initial text", no uncommitted changes
    // 2) human deletes a.md line 1
    // 3) AI adds a.md line 1 back as "initial text"
    // note that, till here, the repo working space is clean as all changes were reverted
    // to make a commit, we have to create some changes in the repo
    // 4) AI creates new file b.md with one line "dummy text"
    // 5) User stages files a.md and b.md and commits
    //
    // Expected: final committed a.md line 1 is "initial text".
    // git-notes for the commit should contain two change_history entries:
    // 1) human deletes text. 
    // 2) ai reverts human-deleted line
    let repo = setup_repo_with_committed_a_md();

    std::fs::write(repo.path().join("a.md"), "").expect("human deletes line 1 should succeed");
    repo.git_ai(&["checkpoint", "--", "a.md"])
        .expect("human checkpoint should succeed");

    std::fs::write(repo.path().join("a.md"), "initial text\n")
        .expect("AI reverts line 1 should succeed");
    std::fs::write(repo.path().join("b.md"), "dummy text\n").expect("write b.md should succeed");
    repo.git_ai(&["checkpoint", "mock_ai", "a.md", "b.md"])
        .expect("AI checkpoint for a.md and b.md should succeed");

    let commit = repo
        .stage_all_and_commit("ai reverts human deleted line and adds b.md")
        .expect("commit should succeed");

    let a_contents =
        std::fs::read_to_string(repo.path().join("a.md")).expect("read a.md should succeed");
    assert_eq!(a_contents, "initial text\n");

    let change_history = commit
        .authorship_log
        .metadata
        .change_history
        .expect("change_history should be present");
    assert_eq!(change_history.len(), 2, "expected two entries");
    assert_eq!(change_history[0].kind, "human");
    assert_eq!(change_history[1].kind, "ai_agent");
    assert_contains_deleted_text(&change_history, 0, "initial text");
}


#[test]
fn test_ai_modifies_human_deleted_line() {
    // Scenario:
    // 1) Test repository has a a.md file with one line "initial text", no uncommitted changes
    // 2) human deletes a.md line 1
    // 3) AI adds a.md line 1 as "ai overwritten text".
    // 4) User stages the file and commits
    //
    // Expected: final committed line 1 is "ai overwritten text".
    // git-notes for the commit should contain two change_history entries:
    // 1) human deleted text
    // 2) ai overwritten human text
    let repo = setup_repo_with_committed_a_md();

    std::fs::write(repo.path().join("a.md"), "").expect("human deletes line 1 should succeed");
    repo.git_ai(&["checkpoint", "--", "a.md"])
        .expect("human checkpoint should succeed");

    std::fs::write(repo.path().join("a.md"), "ai overwritten text\n")
        .expect("AI writes replacement text should succeed");
    repo.git_ai(&["checkpoint", "mock_ai", "a.md"])
        .expect("AI checkpoint should succeed");

    let commit = repo
        .stage_all_and_commit("ai modifies human deleted line")
        .expect("commit should succeed");

    let a_contents =
        std::fs::read_to_string(repo.path().join("a.md")).expect("read a.md should succeed");
    assert_eq!(a_contents, "ai overwritten text\n");

    let change_history = commit
        .authorship_log
        .metadata
        .change_history
        .expect("change_history should be present");
    assert_eq!(change_history.len(), 2, "expected exactly two entries");
    assert_eq!(change_history[0].kind, "human");
    assert_eq!(change_history[1].kind, "ai_agent");
    assert_contains_deleted_text(&change_history, 0, "initial text");
    assert_contains_added_text(&change_history, 1, "ai overwritten text");
}


#[test]
fn test_ai_overwrites_human_without_intermediate_staging() {
    // Scenario:
    // 1) AI creates a.md with 1 line "ai written text"
    // 2) Human modifies a.md's line to "human generated text"
    // 3) AI overwrites a.md's line to "ai overwritten text"
    // 4) User stages the file and commits
    //
    // Expected: final committed content is "ai overwritten text".
    // git-notes for the commit should contain three change_history entries:
    // 1) AI written text
    // 2) Human modify AI written text
    // 3) AI overwritten human text
    let repo = TestRepo::new();

    // Create an initial HEAD so checkpoints/working logs have a stable base commit.
    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["initial".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("initial commit should succeed");

    // Step 1: AI creates a.md.
    std::fs::write(repo.path().join("a.md"), "ai written text\n")
        .expect("write AI-created a.md should succeed");
    repo.git_ai(&["checkpoint", "mock_ai", "a.md"])
        .expect("AI checkpoint should succeed");

    // Step 2: human modifies the line.
    std::fs::write(repo.path().join("a.md"), "human generated text\n")
        .expect("write human-modified a.md should succeed");
    repo.git_ai(&["checkpoint", "--", "a.md"])
        .expect("human checkpoint should succeed");

    // Step 3: AI overwrites the human line.
    std::fs::write(repo.path().join("a.md"), "ai overwritten text\n")
        .expect("write AI-overwritten a.md should succeed");
    repo.git_ai(&["checkpoint", "mock_ai", "a.md"])
        .expect("AI overwrite checkpoint should succeed");

    // Step 4: stage + commit.
    let commit = repo
        .stage_all_and_commit("AI overwrites human without intermediate staging")
        .expect("commit should succeed");

    let a_contents =
        std::fs::read_to_string(repo.path().join("a.md")).expect("read a.md should succeed");
    assert_eq!(a_contents, "ai overwritten text\n");

    let change_history = commit
        .authorship_log
        .metadata
        .change_history
        .expect("change_history should be present");
    assert_eq!(
        change_history.len(),
        3,
        "expected exactly three entries (ai_agent, human, ai_agent)"
    );
    assert_eq!(change_history[0].kind, "ai_agent");
    assert_eq!(change_history[1].kind, "human");
    assert_eq!(change_history[2].kind, "ai_agent");

    assert_contains_added_text(&change_history, 0, "ai written text");
    assert_contains_deleted_text(&change_history, 1, "ai written text");
    assert_contains_added_text(&change_history, 1, "human generated text");
    assert_contains_deleted_text(&change_history, 2, "human generated text");
    assert_contains_added_text(&change_history, 2, "ai overwritten text");
}

crate::reuse_tests_in_worktree!(
    test_ai_modifies_human_added_line,
    test_ai_deletes_all_human_added_line,
    test_ai_deletes_human_added_line_2,
    test_ai_modifies_human_modified_line,
    test_ai_deletes_human_modified_line,
    test_ai_reverts_human_deleted_line,
    test_ai_modifies_human_deleted_line,
    test_ai_overwrites_human_without_intermediate_staging,
);

