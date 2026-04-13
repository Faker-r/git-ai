use crate::repos::test_repo::TestRepo;

fn setup_repo_with_baseline_ab() -> TestRepo {
    let repo = TestRepo::new();
    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["initial".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("initial commit should succeed");

    std::fs::write(repo.path().join("a.md"), "A: line 1\n").expect("write baseline a.md");
    std::fs::write(repo.path().join("b.md"), "B: line 1\n").expect("write baseline b.md");
    repo.stage_all_and_commit("add baseline a.md and b.md")
        .expect("baseline commit should succeed");
    repo
}


#[test]
fn test_commit_subset_ai_changes() {
    // Scenario:
    // 1) Test repo has a.md file with 1 line as "A: line 1", has b.md file with 1 line as "B: line 1"
    // 2) AI appended to a.md's line 2 as "A: line 2" and b.md's line 2 as "B: line 2"
    // 3) User stages a.md using `git add a.md` command and commits 
    //
    // Expected: 
    // git-notes for the commit should contain one change_history entry:
    // 1) AI written to file a.md, line 2; changes to b.md should not be included in the entry
    let repo = setup_repo_with_baseline_ab();

    std::fs::write(repo.path().join("a.md"), "A: line 1\nA: line 2\n").expect("update a.md");
    std::fs::write(repo.path().join("b.md"), "B: line 1\nB: line 2\n").expect("update b.md");
    repo.git_ai(&["checkpoint", "mock_ai", "a.md", "b.md"])
        .expect("AI checkpoint should succeed");

    repo.git(&["add", "a.md"]).expect("git add a.md should succeed");
    let commit = repo
        .commit("commit subset from one AI checkpoint")
        .expect("commit should succeed");

    let a_head = repo
        .git(&["show", "HEAD:a.md"])
        .expect("show committed a.md should succeed");
    let b_head = repo
        .git(&["show", "HEAD:b.md"])
        .expect("show committed b.md should succeed");
    assert_eq!(a_head, "A: line 1\nA: line 2\n");
    assert_eq!(b_head, "B: line 1\n");

    let change_history = commit
        .authorship_log
        .metadata
        .change_history
        .expect("change_history should be present");
    assert_eq!(change_history.len(), 1, "expected one AI change entry");
    assert_eq!(change_history[0].kind, "ai_agent");

    let files = &change_history[0].files;
    let a_file = files.get("a.md").expect("entry should include a.md");
    assert!(
        a_file
            .added_line_contents
            .iter()
            .any(|line| line.contains("A: line 2")),
        "entry should include a.md line 2, got: {:#?}",
        a_file
    );
    assert!(
        !files.contains_key("b.md"),
        "entry should not include unstaged b.md changes, got: {:#?}",
        files
    );
}

#[test]
fn test_commit_ai_changes_from_one_prompt_in_multiple_commits() {
    // Scenario:
    // 1) Test repo has a.md file with 1 line as "A: line 1", has b.md file with 1 line as "B: line 1"
    // 2) AI appended to a.md's line 2 as "A: line 2" and b.md's line 2 as "B: line 2", in one prompt
    // 3) User stages a.md using `git add a.md` command and commits (as commit 1)
    // 4) User stages b.md using `git add b.md` command and commits (as commit 2)
    // 
    // Expected: 
    // git-notes for commit 1 should contain one change_history entry:
    // 1) AI written to file a.md, line 2
    //
    // git-notes for commit 2 should contain one change_history entry:
    // 1) AI written to file b.md, line 2
    let repo = setup_repo_with_baseline_ab();

    // One prompt/checkpoint modifies both files.
    std::fs::write(repo.path().join("a.md"), "A: line 1\nA: line 2\n").expect("update a.md");
    std::fs::write(repo.path().join("b.md"), "B: line 1\nB: line 2\n").expect("update b.md");
    repo.git_ai(&["checkpoint", "mock_ai", "a.md", "b.md"])
        .expect("AI checkpoint should succeed");

    // Commit 1: only a.md
    repo.git(&["add", "a.md"]).expect("git add a.md should succeed");
    let commit1 = repo.commit("commit a.md first").expect("commit1 should succeed");

    let change_history_1 = commit1
        .authorship_log
        .metadata
        .change_history
        .expect("commit1 change_history should be present");
    assert_eq!(
        change_history_1.len(),
        1,
        "commit1 should have one change_history entry"
    );
    assert_eq!(change_history_1[0].kind, "ai_agent");
    let files1 = &change_history_1[0].files;
    assert!(files1.contains_key("a.md"), "commit1 should include a.md");
    assert!(
        !files1.contains_key("b.md"),
        "commit1 should not include b.md, got: {:#?}",
        files1
    );

    // Commit 2: remaining b.md
    repo.git(&["add", "b.md"]).expect("git add b.md should succeed");
    let commit2 = repo
        .commit("commit b.md second")
        .expect("commit2 should succeed");

    let change_history_2 = commit2
        .authorship_log
        .metadata
        .change_history
        .expect("commit2 change_history should be present");
    assert_eq!(
        change_history_2.len(),
        1,
        "commit2 should have one change_history entry"
    );
    assert_eq!(change_history_2[0].kind, "ai_agent");
    let files2 = &change_history_2[0].files;
    assert!(files2.contains_key("b.md"), "commit2 should include b.md");
    assert!(
        !files2.contains_key("a.md"),
        "commit2 should not include a.md, got: {:#?}",
        files2
    );
}

#[test]
fn test_commit_prompt1_change_only() {
    // Scenario:
    // 1) Test repo has a a.md file with 1 line as "line 1"
    // 2) AI appended to a.md's line 2 as "line 2"
    // 3) User stages the file using `git add a.md` command
    // 4) AI appended to a.md's line 3 as "line 3"
    // 5) User commits staged changes using `git commit` command
    //
    // Expected: final committed a.md file should have two lines
    // git-notes for the commit should contain one change_history entry:
    // 1) AI written line 2
    let repo = TestRepo::new();

    // Create an initial HEAD so checkpoints/working logs have a stable base commit.
    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["initial".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("initial commit should succeed");

    // Step 1: baseline a.md
    std::fs::write(repo.path().join("a.md"), "line 1\n").expect("write baseline a.md should succeed");
    repo.stage_all_and_commit("add baseline a.md")
        .expect("baseline commit should succeed");

    // Step 2: AI appends line 2 and checkpoints it.
    std::fs::write(repo.path().join("a.md"), "line 1\nline 2\n")
        .expect("write AI line 2 should succeed");
    repo.git_ai(&["checkpoint", "mock_ai", "a.md"])
        .expect("AI checkpoint for line 2 should succeed");

    // Step 3: user stages only current content.
    repo.git(&["add", "a.md"]).expect("git add a.md should succeed");

    // Step 4: AI appends line 3 but this remains unstaged.
    std::fs::write(repo.path().join("a.md"), "line 1\nline 2\nline 3\n")
        .expect("write AI line 3 should succeed");
    repo.git_ai(&["checkpoint", "mock_ai", "a.md"])
        .expect("AI checkpoint for line 3 should succeed");

    // Step 5: commit staged changes only.
    let commit = repo
        .commit("commit staged subset after AI edits")
        .expect("commit should succeed");

    // Working tree still has unstaged line 3, but commit should only include up to line 2.
    let committed_a = repo
        .git(&["show", "HEAD:a.md"])
        .expect("read committed a.md should succeed");
    assert_eq!(committed_a, "line 1\nline 2\n");
    let working_a =
        std::fs::read_to_string(repo.path().join("a.md")).expect("read working tree a.md should succeed");
    assert_eq!(working_a, "line 1\nline 2\nline 3\n");

    let change_history = commit
        .authorship_log
        .metadata
        .change_history
        .expect("change_history should be present");

    assert_eq!(
        change_history.len(),
        1,
        "expected exactly one entry for committed staged AI change"
    );
    assert_eq!(change_history[0].kind, "ai_agent");
    let a_file = change_history[0]
        .files
        .get("a.md")
        .expect("change_history entry should include a.md");
    assert!(
        a_file
            .added_line_contents
            .iter()
            .any(|line| line.contains("line 2")),
        "entry should include added line 2, got: {:#?}",
        a_file
    );
    assert!(
        !a_file
            .added_line_contents
            .iter()
            .any(|line| line.contains("line 3")),
        "entry should not include unstaged line 3, got: {:#?}",
        a_file
    );
}


#[test]
fn test_commit_only_staged_changes() {
    // Scenario:
    // 1) Test repo has a a.md file with 1 line as "line 1"
    // 2) AI appended to a.md's line 2 as "line 2"
    // 3) User stages the file using `git add a.md` command
    // 4) AI appended to a.md's line 3 as "line 3"
    // 5) User commits staged changes using `git commit` command (assume this is commit 1)
    // 6) User stages the file using `git add a.md` command 
    // 7) User commits staged changes using `git commit` command (assume this is commit 2)
    //
    // Expected: in commit 2, final committed a.md file should have three lines
    // git-notes for commit 2 should contain one change_history entry:
    // 1) AI written line 3
    let repo = TestRepo::new();

    // Create an initial HEAD so checkpoints/working logs have a stable base commit.
    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["initial".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("initial commit should succeed");

    // Step 1: baseline a.md
    std::fs::write(repo.path().join("a.md"), "line 1\n")
        .expect("write baseline a.md should succeed");
    repo.stage_all_and_commit("add baseline a.md")
        .expect("baseline commit should succeed");

    // Step 2: AI appends line 2 and checkpoints.
    std::fs::write(repo.path().join("a.md"), "line 1\nline 2\n")
        .expect("write AI line 2 should succeed");
    repo.git_ai(&["checkpoint", "mock_ai", "a.md"])
        .expect("AI checkpoint for line 2 should succeed");

    // Step 3: user stages a.md.
    repo.git(&["add", "a.md"]).expect("git add a.md should succeed");

    // Step 4: AI appends line 3 and checkpoints (unstaged at this point).
    std::fs::write(repo.path().join("a.md"), "line 1\nline 2\nline 3\n")
        .expect("write AI line 3 should succeed");
    repo.git_ai(&["checkpoint", "mock_ai", "a.md"])
        .expect("AI checkpoint for line 3 should succeed");

    // Step 5: commit only staged content (commit 1 should include line 2 only).
    let _commit1 = repo
        .commit("commit staged line 2")
        .expect("commit1 should succeed");

    // Step 6 + 7: stage remaining line 3 and commit (commit 2).
    repo.git(&["add", "a.md"]).expect("git add a.md for commit2 should succeed");
    let commit2 = repo
        .commit("commit staged line 3")
        .expect("commit2 should succeed");

    let committed_a = repo
        .git(&["show", "HEAD:a.md"])
        .expect("read committed a.md at commit2 should succeed");
    assert_eq!(committed_a, "line 1\nline 2\nline 3\n");

    let change_history = commit2
        .authorship_log
        .metadata
        .change_history
        .expect("commit2 change_history should be present");
    assert_eq!(
        change_history.len(),
        1,
        "commit2 should contain one change_history entry for line 3"
    );
    assert_eq!(change_history[0].kind, "ai_agent");
    let a_file = change_history[0]
        .files
        .get("a.md")
        .expect("commit2 entry should include a.md");
    assert!(
        a_file
            .added_line_contents
            .iter()
            .any(|line| line.contains("line 3")),
        "commit2 entry should include line 3 addition, got: {:#?}",
        a_file
    );
    assert!(
        !a_file
            .added_line_contents
            .iter()
            .any(|line| line.contains("line 2")),
        "commit2 entry should not include line 2 (already committed), got: {:#?}",
        a_file
    );
}

#[test]
fn test_change_history_keeps_checkpoint_order_human_ai_human() {
    // Scenario:
    // 1) Start from baseline a.md
    // 2) Human edits + checkpoint
    // 3) AI edits + checkpoint
    // 4) Human edits + checkpoint
    // 5) Commit all
    //
    // Expected:
    // change_history keeps checkpoint order: human, ai_agent, human.
    let repo = TestRepo::new();

    let mut readme = repo.filename("README.md");
    readme.set_contents(vec!["initial".to_string()]);
    repo.stage_all_and_commit("initial commit")
        .expect("initial commit should succeed");

    std::fs::write(repo.path().join("a.md"), "line 1\n").expect("write baseline a.md");
    repo.stage_all_and_commit("add baseline a.md")
        .expect("baseline commit should succeed");

    // Human checkpoint #1.
    std::fs::write(repo.path().join("a.md"), "line 1\nhuman line 2\n").expect("write human edit");
    repo.git_ai(&["checkpoint", "--", "a.md"])
        .expect("human checkpoint should succeed");

    // AI checkpoint #2.
    std::fs::write(
        repo.path().join("a.md"),
        "line 1\nhuman line 2\nai line 3\n",
    )
    .expect("write AI edit");
    repo.git_ai(&["checkpoint", "mock_ai", "a.md"])
        .expect("AI checkpoint should succeed");

    // Human checkpoint #3.
    std::fs::write(
        repo.path().join("a.md"),
        "line 1\nhuman line 2\nai line 3\nhuman line 4\n",
    )
    .expect("write second human edit");
    repo.git_ai(&["checkpoint", "--", "a.md"])
        .expect("second human checkpoint should succeed");

    repo.git(&["add", "a.md"]).expect("git add a.md should succeed");
    let commit = repo
        .commit("commit ordered checkpoints")
        .expect("commit should succeed");

    let change_history = commit
        .authorship_log
        .metadata
        .change_history
        .expect("change_history should be present");
    assert_eq!(
        change_history.len(),
        3,
        "expected three checkpoint entries in commit change_history"
    );
    assert_eq!(change_history[0].kind, "human");
    assert_eq!(change_history[1].kind, "ai_agent");
    assert_eq!(change_history[2].kind, "human");
}

#[test]
fn test_change_history_contains_human_only_checkpoint_entry() {
    // Scenario:
    // 1) Human edits tracked file and checkpoints as human
    // 2) Commit the change
    //
    // Expected:
    // change_history includes a human entry with added human text.
    let repo = setup_repo_with_baseline_ab();

    std::fs::write(
        repo.path().join("a.md"),
        "A: line 1\nhuman generated line 2\n",
    )
    .expect("write human edit to a.md");
    repo.git_ai(&["checkpoint", "--", "a.md"])
        .expect("human checkpoint should succeed");

    repo.git(&["add", "a.md"]).expect("git add a.md should succeed");
    let commit = repo
        .commit("commit human-only checkpoint")
        .expect("commit should succeed");

    let change_history = commit
        .authorship_log
        .metadata
        .change_history
        .expect("change_history should be present");
    assert_eq!(change_history.len(), 1, "expected one human entry");
    assert_eq!(change_history[0].kind, "human");
    let a_file = change_history[0]
        .files
        .get("a.md")
        .expect("entry should include a.md");
    assert!(
        a_file
            .added_line_contents
            .iter()
            .any(|line| line.contains("human generated line 2")),
        "human entry should include added human line, got: {:#?}",
        a_file
    );
}

#[test]
fn test_change_history_excludes_gitignored_files() {
    // Scenario:
    // 1) Add .gitignore that ignores *.tmp
    // 2) AI edits tracked a.md and ignored ignored.tmp
    // 3) Commit tracked a.md
    //
    // Expected:
    // change_history entry includes a.md and excludes ignored.tmp.
    let repo = setup_repo_with_baseline_ab();

    std::fs::write(repo.path().join(".gitignore"), "*.tmp\n").expect("write .gitignore");
    repo.stage_all_and_commit("add gitignore")
        .expect("commit .gitignore should succeed");

    std::fs::write(repo.path().join("a.md"), "A: line 1\nA: line 2\n").expect("update a.md");
    std::fs::write(repo.path().join("ignored.tmp"), "ignored content\n")
        .expect("write ignored tmp file");

    repo.git_ai(&["checkpoint", "mock_ai", "a.md", "ignored.tmp"])
        .expect("AI checkpoint should succeed");

    repo.git(&["add", "a.md"]).expect("git add a.md should succeed");
    let commit = repo
        .commit("commit tracked edit with ignored file present")
        .expect("commit should succeed");

    let change_history = commit
        .authorship_log
        .metadata
        .change_history
        .expect("change_history should be present");
    let ai_entry = change_history
        .iter()
        .find(|entry| entry.kind == "ai_agent")
        .expect("expected an ai_agent entry");
    let files = &ai_entry.files;
    assert!(files.contains_key("a.md"), "entry should include a.md");
    assert!(
        !files.contains_key("ignored.tmp"),
        "ai entry should exclude ignored file, got: {:#?}",
        ai_entry
    );
}

#[test]
fn test_change_history_excludes_binary_files() {
    // Scenario:
    // 1) AI edits tracked text file a.md and binary file image.bin
    // 2) Commit only tracked text file
    //
    // Expected:
    // change_history entry includes text file and excludes binary file.
    let repo = setup_repo_with_baseline_ab();

    std::fs::write(repo.path().join("a.md"), "A: line 1\nA: line 2\n").expect("update a.md");
    std::fs::write(repo.path().join("image.bin"), vec![0_u8, 159, 146, 150, 0, 255, 10])
        .expect("write binary file");

    repo.git_ai(&["checkpoint", "mock_ai", "a.md", "image.bin"])
        .expect("AI checkpoint should succeed");

    repo.git(&["add", "a.md"]).expect("git add a.md should succeed");
    let commit = repo
        .commit("commit tracked text edit with binary present")
        .expect("commit should succeed");

    let change_history = commit
        .authorship_log
        .metadata
        .change_history
        .expect("change_history should be present");
    let ai_entry = change_history
        .iter()
        .find(|entry| entry.kind == "ai_agent")
        .expect("expected an ai_agent entry");
    let files = &ai_entry.files;
    assert!(files.contains_key("a.md"), "entry should include a.md");
    assert!(
        !files.contains_key("image.bin"),
        "ai entry should exclude binary file, got: {:#?}",
        ai_entry
    );
}

crate::reuse_tests_in_worktree!(
    test_commit_subset_ai_changes,
    test_commit_ai_changes_from_one_prompt_in_multiple_commits,
    test_commit_prompt1_change_only,
    test_commit_only_staged_changes,
    test_change_history_keeps_checkpoint_order_human_ai_human,
    test_change_history_contains_human_only_checkpoint_entry,
    test_change_history_excludes_gitignored_files,
    test_change_history_excludes_binary_files,
);
