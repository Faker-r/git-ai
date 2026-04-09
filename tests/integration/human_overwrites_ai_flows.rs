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
fn test_human_modifies_ai_added_line() {
    // Scenario:
    // 1) Test repository has a a.md file with one line "initial text", no uncommitted changes
    // 2) AI writes in a.md line 2 as "ai generated text"
    // 3) Human overwrites a.md line 2 as "human overwritten text"
    // 4) User stages the file and commits
    //
    // Expected: final committed line 2 is "human overwritten text".
    // git-notes for the commit should contain two change_history entries:
    // 1) ai generated text
    // 2) human overwritten ai-generated text
    let repo = setup_repo_with_committed_a_md();

    std::fs::write(repo.path().join("a.md"), "initial text\nai generated text\n")
        .expect("write AI-updated a.md should succeed");
    repo.git_ai(&["checkpoint", "mock_ai", "a.md"])
        .expect("AI checkpoint should succeed");

    std::fs::write(
        repo.path().join("a.md"),
        "initial text\nhuman overwritten text\n",
    )
    .expect("write human-overwritten a.md should succeed");
    repo.git_ai(&["checkpoint", "--", "a.md"])
        .expect("human checkpoint should succeed");

    let commit = repo
        .stage_all_and_commit("human modifies AI added line")
        .expect("commit should succeed");

    let a_contents =
        std::fs::read_to_string(repo.path().join("a.md")).expect("read a.md should succeed");
    assert_eq!(a_contents, "initial text\nhuman overwritten text\n");

    let change_history = commit
        .authorship_log
        .metadata
        .change_history
        .expect("change_history should be present");
    assert_eq!(change_history.len(), 2, "expected exactly two entries");
    assert_eq!(change_history[0].kind, "ai_agent");
    assert_eq!(change_history[1].kind, "human");
    assert_contains_added_text(&change_history, 0, "ai generated text");
    assert_contains_deleted_text(&change_history, 1, "ai generated text");
    assert_contains_added_text(&change_history, 1, "human overwritten text");
}


#[test]
fn test_human_deletes_all_ai_added_line() {
    // Scenario:
    // 1) Test repository has a a.md file with one line "initial text", no uncommitted changes
    // 2) AI writes in a.md line 2 as "ai generated text"
    // 3) Human deletes a.md line 2 (i.e., all ai-modified lines)
    // note that, till here, the repo working space is clean as all changes were reverted
    // to make a commit, we have to create some changes in the repo
    // 4) Human creates new file b.md with one line "dummy text"
    // 5) User stages files a.md and b.md and commits
    //
    // Expected: final committed a.md file line 2 is empty.
    // git-notes for the commit should contain change_history entries where:
    // 1) ai generated text for a.md
    // 2) human deleted ai-generated text in a.md
    // 3) human added text in b.md
    let repo = setup_repo_with_committed_a_md();

    std::fs::write(repo.path().join("a.md"), "initial text\nai generated text\n")
        .expect("write AI-updated a.md should succeed");
    repo.git_ai(&["checkpoint", "mock_ai", "a.md"])
        .expect("AI checkpoint should succeed");

    std::fs::write(repo.path().join("a.md"), "initial text\n")
        .expect("write human-deleted a.md should succeed");
    std::fs::write(repo.path().join("b.md"), "dummy text\n")
        .expect("write human-created b.md should succeed");
    repo.git_ai(&["checkpoint", "--", "a.md", "b.md"])
        .expect("human checkpoint for a.md and b.md should succeed");

    let commit = repo
        .stage_all_and_commit("human deletes AI added line and adds b.md")
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
    assert_eq!(change_history[0].kind, "ai_agent");
    assert_eq!(change_history[1].kind, "human");
    assert_eq!(change_history[2].kind, "human");
    assert_contains_added_text(&change_history, 0, "ai generated text");
    let human_has_a_deletion = change_history[1..].iter().any(|entry| {
        entry
            .files
            .get("a.md")
            .map(|a_file| {
                a_file
                    .deleted_line_contents
                    .iter()
                    .any(|line| line.contains("ai generated text"))
            })
            .unwrap_or(false)
    });
    assert!(
        human_has_a_deletion,
        "one human entry should include deleted ai-generated text for a.md, got: {:#?}",
        &change_history[1..]
    );
    let human_has_b_addition = change_history[1..].iter().any(|entry| {
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
        human_has_b_addition,
        "one human entry should include added dummy text for b.md, got: {:#?}",
        &change_history[1..]
    );
}


#[test]
fn test_human_deletes_ai_added_line_2() {
    // Scenario:
    // 1) Test repository has a a.md file with one line "initial text", no uncommitted changes
    // 2) AI writes in a.md line 2 as "ai generated line 2" and line 3 as "ai generated line 3"
    // 3) Human deletes a.md line 2
    // 4) User stages the file and commits
    //
    // Expected: final committed a.md file has two lines: "initial text" and "ai generated line 3".
    // git-notes for the commit should contain two change_history entries:
    // 1) ai generated text
    // 2) human deleted ai-generated line 2
    let repo = setup_repo_with_committed_a_md();

    std::fs::write(
        repo.path().join("a.md"),
        "initial text\nai generated line 2\nai generated line 3\n",
    )
    .expect("write AI-updated a.md should succeed");
    repo.git_ai(&["checkpoint", "mock_ai", "a.md"])
        .expect("AI checkpoint should succeed");

    std::fs::write(repo.path().join("a.md"), "initial text\nai generated line 3\n")
        .expect("write human-deleted line 2 in a.md should succeed");
    repo.git_ai(&["checkpoint", "--", "a.md"])
        .expect("human checkpoint should succeed");

    let commit = repo
        .stage_all_and_commit("human deletes AI added line 2")
        .expect("commit should succeed");

    let a_contents =
        std::fs::read_to_string(repo.path().join("a.md")).expect("read a.md should succeed");
    assert_eq!(a_contents, "initial text\nai generated line 3\n");

    let change_history = commit
        .authorship_log
        .metadata
        .change_history
        .expect("change_history should be present");
    assert_eq!(change_history.len(), 2, "expected exactly two entries");
    assert_eq!(change_history[0].kind, "ai_agent");
    assert_eq!(change_history[1].kind, "human");
    assert_contains_added_text(&change_history, 0, "ai generated line 2");
    assert_contains_added_text(&change_history, 0, "ai generated line 3");
    assert_contains_deleted_text(&change_history, 1, "ai generated line 2");
}

#[test]
fn test_human_modifies_ai_modified_line() {
    // Scenario:
    // 1) Test repository has a a.md file with one line "initial text", no uncommitted changes
    // 2) AI changes a.md line 1 to "ai generated text"
    // 3) Human overwrites a.md line 1 as "human overwritten text"
    // 4) User stages the file and commits
    //
    // Expected: final committed line 1 is "human overwritten text".
    // git-notes for the commit should contain two change_history entries:
    // 1) ai generated text
    // 2) human deleted ai-generated line 2
    let repo = setup_repo_with_committed_a_md();

    std::fs::write(repo.path().join("a.md"), "ai generated text\n")
        .expect("write AI-updated a.md should succeed");
    repo.git_ai(&["checkpoint", "mock_ai", "a.md"])
        .expect("AI checkpoint should succeed");

    std::fs::write(repo.path().join("a.md"), "human overwritten text\n")
        .expect("write human-overwritten a.md should succeed");
    repo.git_ai(&["checkpoint", "--", "a.md"])
        .expect("human checkpoint should succeed");

    let commit = repo
        .stage_all_and_commit("human modifies AI modified line")
        .expect("commit should succeed");

    let a_contents =
        std::fs::read_to_string(repo.path().join("a.md")).expect("read a.md should succeed");
    assert_eq!(a_contents, "human overwritten text\n");

    let change_history = commit
        .authorship_log
        .metadata
        .change_history
        .expect("change_history should be present");
    assert_eq!(change_history.len(), 2, "expected exactly two entries");
    assert_eq!(change_history[0].kind, "ai_agent");
    assert_eq!(change_history[1].kind, "human");
    assert_contains_added_text(&change_history, 0, "ai generated text");
    assert_contains_deleted_text(&change_history, 1, "ai generated text");
    assert_contains_added_text(&change_history, 1, "human overwritten text");
}


#[test]
fn test_human_deletes_ai_modified_line() {
    // Scenario:
    // 1) Test repository has a a.md file with one line "initial text", no uncommitted changes
    // 2) AI changes a.md line 1 to "ai generated text"
    // 3) Human deletes a.md line 1
    // 4) User stages the file and commits
    //
    // Expected: final committed line 1 is empty.
    // git-notes for the commit should contain one change_history entry:
    // 1) ai generated text. human deletion is reflected in the line-declined statistics
    let repo = setup_repo_with_committed_a_md();

    std::fs::write(repo.path().join("a.md"), "ai generated text\n")
        .expect("write AI-updated a.md should succeed");
    repo.git_ai(&["checkpoint", "mock_ai", "a.md"])
        .expect("AI checkpoint should succeed");

    std::fs::write(repo.path().join("a.md"), "").expect("delete line 1 should succeed");
    repo.git_ai(&["checkpoint", "--", "a.md"])
        .expect("human checkpoint should succeed");

    let commit = repo
        .stage_all_and_commit("human deletes AI modified line")
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
    assert_eq!(change_history[0].kind, "ai_agent");
    assert_eq!(change_history[1].kind, "human");
    assert_contains_added_text(&change_history, 0, "ai generated text");
}



#[test]
fn test_ai_deletes_line() {
    // Scenario:
    // 1) Test repository has a a.md file with one line "initial text", no uncommitted changes
    // 2) AI deletes a.md line 1
    // 3) User stages the file and commits
    //
    // Expected: final committed line 1 is empty.
    // git-notes for the commit should contain one change_history entry:
    // 1) ai deletes text. 
    let repo = setup_repo_with_committed_a_md();

    std::fs::write(repo.path().join("a.md"), "").expect("AI deletes line 1 should succeed");
    repo.git_ai(&["checkpoint", "mock_ai", "a.md"])
        .expect("AI checkpoint should succeed");

    let commit = repo
        .stage_all_and_commit("human reverts AI deleted line")
        .expect("commit should succeed");

    let a_contents =
        std::fs::read_to_string(repo.path().join("a.md")).expect("read a.md should succeed");
    assert_eq!(a_contents, "");

    let change_history = commit
        .authorship_log
        .metadata
        .change_history
        .expect("change_history should be present");
    assert_eq!(change_history.len(), 1, "expected one AI entry");
    assert_eq!(change_history[0].kind, "ai_agent");
    assert_contains_deleted_text(&change_history, 0, "initial text");
}


#[test]
fn test_human_reverts_ai_deleted_line() {
    // Scenario:
    // 1) Test repository has a a.md file with one line "initial text", no uncommitted changes
    // 2) AI deletes a.md line 1
    // 3) Human adds a.md line 1 back as "initial text"
    // note that, till here, the repo working space is clean as all changes were reverted
    // to make a commit, we have to create some changes in the repo
    // 4) Human creates new file b.md with one line "dummy text"
    // 5) User stages files a.md and b.md and commits
    //
    // Expected: final committed a.md line 1 is "initial text".
    // git-notes for the commit should contain two change_history entries:
    // 1) ai deletes text. 
    // 2) human reverts ai-deleted line
    let repo = setup_repo_with_committed_a_md();

    std::fs::write(repo.path().join("a.md"), "").expect("AI deletes line 1 should succeed");
    repo.git_ai(&["checkpoint", "mock_ai", "a.md"])
        .expect("AI checkpoint should succeed");

    std::fs::write(repo.path().join("a.md"), "initial text\n")
        .expect("human reverts line 1 should succeed");
    std::fs::write(repo.path().join("b.md"), "dummy text\n")
        .expect("write human-created b.md should succeed");
    repo.git_ai(&["checkpoint", "--", "a.md", "b.md"])
        .expect("human checkpoint for a.md and b.md should succeed");

    let commit = repo
        .stage_all_and_commit("human reverts AI deleted line and adds b.md")
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
    assert_eq!(change_history[0].kind, "ai_agent");
    assert_eq!(change_history[1].kind, "human");
    assert_contains_deleted_text(&change_history, 0, "initial text");
}


#[test]
fn test_human_modifies_ai_deleted_line() {
    // Scenario:
    // 1) Test repository has a a.md file with one line "initial text", no uncommitted changes
    // 2) AI deletes a.md line 1
    // 3) Human adds a.md line 1 as "human overwritten text".
    // 4) User stages the file and commits
    //
    // Expected: final committed line 1 is "human overwritten text".
    // git-notes for the commit should contain two change_history entries:
    // 1) ai deleted text
    // 2) human overwritten ai-generated text
    let repo = setup_repo_with_committed_a_md();

    std::fs::write(repo.path().join("a.md"), "").expect("AI deletes line 1 should succeed");
    repo.git_ai(&["checkpoint", "mock_ai", "a.md"])
        .expect("AI checkpoint should succeed");

    std::fs::write(repo.path().join("a.md"), "human overwritten text\n")
        .expect("human writes replacement text should succeed");
    repo.git_ai(&["checkpoint", "--", "a.md"])
        .expect("human checkpoint should succeed");

    let commit = repo
        .stage_all_and_commit("human modifies AI deleted line")
        .expect("commit should succeed");

    let a_contents =
        std::fs::read_to_string(repo.path().join("a.md")).expect("read a.md should succeed");
    assert_eq!(a_contents, "human overwritten text\n");

    let change_history = commit
        .authorship_log
        .metadata
        .change_history
        .expect("change_history should be present");
    assert_eq!(change_history.len(), 2, "expected exactly two entries");
    assert_eq!(change_history[0].kind, "ai_agent");
    assert_eq!(change_history[1].kind, "human");
    assert_contains_deleted_text(&change_history, 0, "initial text");
    assert_contains_added_text(&change_history, 1, "human overwritten text");
}


#[test]
fn test_human_overwrites_ai_without_intermediate_staging() {
    // Scenario:
    // 1) Human creates a.md with 1 line "human written text"
    // 2) AI modifies a.md's line to "ai generated text"
    // 3) Human overwrites a.md's line to "human overwritten text"
    // 4) User stages the file and commits
    //
    // Expected: final committed content is "human overwritten text".
    // git-notes for the commit should contain three change_history entries:
    // 1) human written text
    // 2) ai modify human written text
    // 3) human overwritten ai-generated text

    let repo = TestRepo::new();

    // Create an initial HEAD so checkpoints/working logs have a stable base commit.
    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["initial".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("initial commit should succeed");

    // Step 1: human creates a.md with 1 line
    std::fs::write(repo.path().join("a.md"), "human written text\n")
        .expect("write a.md should succeed");
    repo.git_ai(&["checkpoint", "--", "a.md"])
        .expect("human checkpoint should succeed");

    // Step 2: AI modifies a.md's line
    std::fs::write(repo.path().join("a.md"), "ai generated text\n")
        .expect("write a.md AI content should succeed");
    repo.git_ai(&["checkpoint", "mock_ai", "a.md"])
        .expect("AI checkpoint should succeed");

    // Step 3: human overwrites the AI-generated line
    std::fs::write(repo.path().join("a.md"), "human overwritten text\n")
        .expect("write a.md human overwrite should succeed");
    repo.git_ai(&["checkpoint", "--", "a.md"])
        .expect("human overwrite checkpoint should succeed");

    // Step 4: stage + commit
    let commit = repo
        .stage_all_and_commit("add a.md")
        .expect("commit should succeed");

    // Verify final content is correct.
    let a_contents = std::fs::read_to_string(repo.path().join("a.md"))
        .expect("read a.md should succeed");
    assert_eq!(a_contents, "human overwritten text\n");

    // Verify change_history has exactly the three checkpoints we created, in order.
    let change_history = commit
        .authorship_log
        .metadata
        .change_history
        .expect("change_history should be present");
    assert_eq!(
        change_history.len(),
        3,
        "Expected exactly 3 change_history entries (human, ai_agent, human), got: {:#?}",
        change_history
    );
    assert_eq!(change_history[0].kind, "human");
    assert_eq!(change_history[1].kind, "ai_agent");
    assert_eq!(change_history[2].kind, "human");

    let file0 = change_history[0]
        .files
        .get("a.md")
        .expect("entry 0 should include a.md");
    assert!(
        file0
            .added_line_contents
            .iter()
            .any(|l| l.contains("human written text")),
        "entry 0 should record adding 'human written text' for a.md, got: {:#?}",
        file0
    );

    let file1 = change_history[1]
        .files
        .get("a.md")
        .expect("entry 1 should include a.md");
    assert!(
        file1
            .deleted_line_contents
            .iter()
            .any(|l| l.contains("human written text")),
        "entry 1 should record deleting 'human written text' for a.md, got: {:#?}",
        file1
    );
    assert!(
        file1
            .added_line_contents
            .iter()
            .any(|l| l.contains("ai generated text")),
        "entry 1 should record adding 'ai generated text' for a.md, got: {:#?}",
        file1
    );

    let file2 = change_history[2]
        .files
        .get("a.md")
        .expect("entry 2 should include a.md");
    assert!(
        file2
            .deleted_line_contents
            .iter()
            .any(|l| l.contains("ai generated text")),
        "entry 2 should record deleting 'ai generated text' for a.md, got: {:#?}",
        file2
    );
    assert!(
        file2
            .added_line_contents
            .iter()
            .any(|l| l.contains("human overwritten text")),
        "entry 2 should record adding 'human overwritten text' for a.md, got: {:#?}",
        file2
    );
}



#[test]
fn test_human_overwrites_ai_with_staging_after_each_edit() {
    // Scenario:
    // 1) Human creates a.md with 1 line "human written text"
    // 2) User stages the file
    // 3) AI modifies a.md's line to "ai generated text"
    // 4) User stages the file
    // 5) Human overwrites a.md's line to "human overwritten text"
    // 6) User stages the file and commits
    //
    // Expected: final committed content is "human overwritten text".
    // git-notes for the commit should contain three change_history entries:
    // 1) human written text
    // 2) ai modify human written text
    // 3) human overwritten ai-generated text

    let repo = TestRepo::new();

    // Create an initial HEAD so checkpoints/working logs have a stable base commit.
    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["initial".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("initial commit should succeed");

    // Step 1: human creates a.md with 1 line "human written text"
    std::fs::write(repo.path().join("a.md"), "human written text\n")
        .expect("write a.md should succeed");
    repo.git_ai(&["checkpoint", "--", "a.md"])
        .expect("human checkpoint should succeed");

    // Step 2: user stages the file
    repo.git(&["add", "a.md"]).expect("git add a.md should succeed");

    // Step 3: AI modifies a.md's line to "ai generated text"
    std::fs::write(repo.path().join("a.md"), "ai generated text\n")
        .expect("write a.md AI content should succeed");
    repo.git_ai(&["checkpoint", "mock_ai", "a.md"])
        .expect("AI checkpoint should succeed");

    // Step 4: user stages the file
    repo.git(&["add", "a.md"]).expect("git add a.md should succeed");

    // Step 5: human overwrites a.md's line to "human overwritten text"
    std::fs::write(repo.path().join("a.md"), "human overwritten text\n")
        .expect("write a.md human overwrite should succeed");
    repo.git_ai(&["checkpoint", "--", "a.md"])
        .expect("human overwrite checkpoint should succeed");

    // Step 6: user stages the file and commits
    repo.git(&["add", "a.md"]).expect("git add a.md should succeed");
    let commit = repo
        .commit("stage a.md between edits")
        .expect("git commit should succeed");

    // Verify final content is correct.
    let a_contents = std::fs::read_to_string(repo.path().join("a.md"))
        .expect("read a.md should succeed");
    assert_eq!(a_contents, "human overwritten text\n");

    // Verify change_history has exactly the three checkpoints we created, in order.
    let change_history = commit
        .authorship_log
        .metadata
        .change_history
        .expect("change_history should be present");
    assert_eq!(
        change_history.len(),
        3,
        "Expected exactly 3 change_history entries (human, ai_agent, human), got: {:#?}",
        change_history
    );
    assert_eq!(change_history[0].kind, "human");
    assert_eq!(change_history[1].kind, "ai_agent");
    assert_eq!(change_history[2].kind, "human");

    let file0 = change_history[0]
        .files
        .get("a.md")
        .expect("entry 0 should include a.md");
    assert!(
        file0
            .added_line_contents
            .iter()
            .any(|l: &String| l.contains("human written text")),
        "entry 0 should record adding 'human written text' for a.md, got: {:#?}",
        file0
    );

    let file1 = change_history[1]
        .files
        .get("a.md")
        .expect("entry 1 should include a.md");
    assert!(
        file1
            .deleted_line_contents
            .iter()
            .any(|l: &String| l.contains("human written text")),
        "entry 1 should record deleting 'human written text' for a.md, got: {:#?}",
        file1
    );
    assert!(
        file1
            .added_line_contents
            .iter()
            .any(|l: &String| l.contains("ai generated text")),
        "entry 1 should record adding 'ai generated text' for a.md, got: {:#?}",
        file1
    );

    let file2 = change_history[2]
        .files
        .get("a.md")
        .expect("entry 2 should include a.md");
    assert!(
        file2
            .deleted_line_contents
            .iter()
            .any(|l: &String| l.contains("ai generated text")),
        "entry 2 should record deleting 'ai generated text' for a.md, got: {:#?}",
        file2
    );
    assert!(
        file2
            .added_line_contents
            .iter()
            .any(|l: &String| l.contains("human overwritten text")),
        "entry 2 should record adding 'human overwritten text' for a.md, got: {:#?}",
        file2
    );
}




#[test]
fn test_human_overwrites_ai_after_staging_ai_edit() {
    // Scenario:
    // 1) Human creates a.md with 1 line "human written text"
    // 2) AI modifies a.md's line to "ai generated text"
    // 3) User stages the file
    // 4) Human overwrites a.md's line to "human overwritten text"
    // 5) User stages the file and commits
    //
    // Expected: final committed content is "human overwritten text".
    // git-notes for the commit should contain three change_history entries:
    // 1) human written text
    // 2) ai modify human written text
    // 3) human overwritten ai-generated text

    let repo = TestRepo::new();

    // Create an initial HEAD so checkpoints/working logs have a stable base commit.
    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["initial".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("initial commit should succeed");

    // Step 1: human creates a.md with 1 line "human written text"
    std::fs::write(repo.path().join("a.md"), "human written text\n")
        .expect("write a.md should succeed");
    repo.git_ai(&["checkpoint", "--", "a.md"])
        .expect("human checkpoint should succeed");

    // Step 2: AI modifies a.md's line to "ai generated text"
    std::fs::write(repo.path().join("a.md"), "ai generated text\n")
        .expect("write a.md AI content should succeed");
    repo.git_ai(&["checkpoint", "mock_ai", "a.md"])
        .expect("AI checkpoint should succeed");

    // Step 3: user stages the file
    repo.git(&["add", "a.md"]).expect("git add a.md should succeed");

    // Step 4: human overwrites a.md's line to "human overwritten text"
    std::fs::write(repo.path().join("a.md"), "human overwritten text\n")
        .expect("write a.md human overwrite should succeed");
    repo.git_ai(&["checkpoint", "--", "a.md"])
        .expect("human overwrite checkpoint should succeed");

    // Step 5: user stages the file and commits
    repo.git(&["add", "a.md"]).expect("git add a.md should succeed");
    let commit = repo
        .commit("overwrite AI after staging")
        .expect("git commit should succeed");

    // Verify final content is correct.
    let a_contents = std::fs::read_to_string(repo.path().join("a.md"))
        .expect("read a.md should succeed");
    assert_eq!(a_contents, "human overwritten text\n");

    // Verify change_history has exactly the three checkpoints we created, in order.
    let change_history = commit
        .authorship_log
        .metadata
        .change_history
        .expect("change_history should be present");
    assert_eq!(
        change_history.len(),
        3,
        "Expected exactly 3 change_history entries (human, ai_agent, human), got: {:#?}",
        change_history
    );
    assert_eq!(change_history[0].kind, "human");
    assert_eq!(change_history[1].kind, "ai_agent");
    assert_eq!(change_history[2].kind, "human");

    let file0 = change_history[0]
        .files
        .get("a.md")
        .expect("entry 0 should include a.md");
    assert!(
        file0
            .added_line_contents
            .iter()
            .any(|l: &String| l.contains("human written text")),
        "entry 0 should record adding 'human written text' for a.md, got: {:#?}",
        file0
    );

    let file1 = change_history[1]
        .files
        .get("a.md")
        .expect("entry 1 should include a.md");
    assert!(
        file1
            .deleted_line_contents
            .iter()
            .any(|l: &String| l.contains("human written text")),
        "entry 1 should record deleting 'human written text' for a.md, got: {:#?}",
        file1
    );
    assert!(
        file1
            .added_line_contents
            .iter()
            .any(|l: &String| l.contains("ai generated text")),
        "entry 1 should record adding 'ai generated text' for a.md, got: {:#?}",
        file1
    );

    let file2 = change_history[2]
        .files
        .get("a.md")
        .expect("entry 2 should include a.md");
    assert!(
        file2
            .deleted_line_contents
            .iter()
            .any(|l: &String| l.contains("ai generated text")),
        "entry 2 should record deleting 'ai generated text' for a.md, got: {:#?}",
        file2
    );
    assert!(
        file2
            .added_line_contents
            .iter()
            .any(|l: &String| l.contains("human overwritten text")),
        "entry 2 should record adding 'human overwritten text' for a.md, got: {:#?}",
        file2
    );
}



#[test]
fn test_human_overwrites_ai_after_soft_reset_of_ai_commit() {
    // Scenario:
    // 1) Human creates a.md with 1 line "human written text"
    // 2) AI modifies a.md's line to "ai generated text"
    // 3) User stages the file and commits
    // 4) User run `git reset --soft HEAD^` to undo the commit
    // 5) Human overwrites a.md's line to "human overwritten text"
    // 6) User stages the file and commits
    //
    // Expected: final committed content is "human overwritten text".
    // git-notes for the commit should contain three change_history entries:
    // 1) human written text
    // 2) ai modify human written text
    // 3) human overwritten ai-generated text

    let repo = TestRepo::new();

    // Create an initial HEAD so checkpoints/working logs have a stable base commit.
    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["initial".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("initial commit should succeed");

    // Step 1: human creates a.md with 1 line "human written text"
    std::fs::write(repo.path().join("a.md"), "human written text\n")
        .expect("write a.md should succeed");
    repo.git_ai(&["checkpoint", "--", "a.md"])
        .expect("human checkpoint should succeed");

    // Step 2: AI modifies a.md's line to "ai generated text"
    std::fs::write(repo.path().join("a.md"), "ai generated text\n")
        .expect("write a.md AI content should succeed");
    repo.git_ai(&["checkpoint", "mock_ai", "a.md"])
        .expect("AI checkpoint should succeed");

    // Step 3: user stages the file and commits
    repo.git(&["add", "a.md"]).expect("git add a.md should succeed");
    repo.commit("commit AI edit")
        .expect("first commit should succeed");

    // Step 4: undo the commit, keeping changes staged
    repo.git(&["reset", "--soft", "HEAD^"])
        .expect("git reset --soft should succeed");

    // Step 5: human overwrites a.md's line to "human overwritten text"
    std::fs::write(repo.path().join("a.md"), "human overwritten text\n")
        .expect("write a.md human overwrite should succeed");
    repo.git_ai(&["checkpoint", "--", "a.md"])
        .expect("human overwrite checkpoint should succeed");

    // Step 6: user stages the file and commits
    repo.git(&["add", "a.md"]).expect("git add a.md should succeed");
    let commit = repo
        .commit("commit human overwrite after soft reset")
        .expect("second commit should succeed");

    // Verify final content is correct.
    let a_contents = std::fs::read_to_string(repo.path().join("a.md"))
        .expect("read a.md should succeed");
    assert_eq!(a_contents, "human overwritten text\n");

    // Verify change_history has exactly the three checkpoints we created, in order.
    let change_history = commit
        .authorship_log
        .metadata
        .change_history
        .expect("change_history should be present");
    assert_eq!(
        change_history.len(),
        3,
        "Expected exactly 3 change_history entries (human, ai_agent, human), got: {:#?}",
        change_history
    );
    assert_eq!(change_history[0].kind, "human");
    assert_eq!(change_history[1].kind, "ai_agent");
    assert_eq!(change_history[2].kind, "human");

    let file0 = change_history[0]
        .files
        .get("a.md")
        .expect("entry 0 should include a.md");
    assert!(
        file0
            .added_line_contents
            .iter()
            .any(|l: &String| l.contains("human written text")),
        "entry 0 should record adding 'human written text' for a.md, got: {:#?}",
        file0
    );

    let file1 = change_history[1]
        .files
        .get("a.md")
        .expect("entry 1 should include a.md");
    assert!(
        file1
            .deleted_line_contents
            .iter()
            .any(|l: &String| l.contains("human written text")),
        "entry 1 should record deleting 'human written text' for a.md, got: {:#?}",
        file1
    );
    assert!(
        file1
            .added_line_contents
            .iter()
            .any(|l: &String| l.contains("ai generated text")),
        "entry 1 should record adding 'ai generated text' for a.md, got: {:#?}",
        file1
    );

    let file2 = change_history[2]
        .files
        .get("a.md")
        .expect("entry 2 should include a.md");
    assert!(
        file2
            .deleted_line_contents
            .iter()
            .any(|l: &String| l.contains("ai generated text")),
        "entry 2 should record deleting 'ai generated text' for a.md, got: {:#?}",
        file2
    );
    assert!(
        file2
            .added_line_contents
            .iter()
            .any(|l: &String| l.contains("human overwritten text")),
        "entry 2 should record adding 'human overwritten text' for a.md, got: {:#?}",
        file2
    );
}


crate::reuse_tests_in_worktree!(
    test_human_modifies_ai_added_line,
    test_human_deletes_all_ai_added_line,
    test_human_deletes_ai_added_line_2,
    test_human_modifies_ai_modified_line,
    test_human_deletes_ai_modified_line,
    test_human_reverts_ai_deleted_line,
    test_human_modifies_ai_deleted_line,
    test_human_overwrites_ai_without_intermediate_staging,
    test_human_overwrites_ai_with_staging_after_each_edit,
    test_human_overwrites_ai_after_staging_ai_edit,
    test_human_overwrites_ai_after_soft_reset_of_ai_commit,
);

