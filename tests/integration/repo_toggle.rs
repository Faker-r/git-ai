use crate::repos::test_repo::TestRepo;
use serial_test::serial;
use std::fs;
use std::path::PathBuf;

struct EnvVarGuard {
    key: &'static str,
    old: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let old = std::env::var(key).ok();
        // SAFETY: tests marked `serial` avoid concurrent env mutation.
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, old }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // SAFETY: tests marked `serial` avoid concurrent env mutation.
        unsafe {
            if let Some(old) = &self.old {
                std::env::set_var(self.key, old);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}

fn git_common_dir(repo: &TestRepo) -> PathBuf {
    let common_dir = PathBuf::from(
        repo.git(&["rev-parse", "--git-common-dir"])
            .expect("failed to resolve git common dir")
            .trim(),
    );
    if common_dir.is_absolute() {
        common_dir
    } else {
        repo.path().join(common_dir)
    }
}

fn git_hooks_ai_dir(repo: &TestRepo) -> PathBuf {
    git_common_dir(repo).join("ai")
}

#[test]
#[serial]
fn repo_disable_creates_disabled_marker() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "wrapper");
    let repo = TestRepo::new();
    let disabled_marker = git_hooks_ai_dir(&repo).join("disabled");

    assert!(
        !disabled_marker.exists(),
        "disabled marker should not exist initially"
    );

    repo.git_ai(&["repo", "disable"])
        .expect("repo disable should succeed");

    assert!(
        disabled_marker.exists(),
        "disabled marker should exist after repo disable"
    );
}

#[test]
#[serial]
fn repo_enable_removes_disabled_marker() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "wrapper");
    let repo = TestRepo::new();
    let disabled_marker = git_hooks_ai_dir(&repo).join("disabled");

    repo.git_ai(&["repo", "disable"])
        .expect("repo disable should succeed");
    assert!(disabled_marker.exists(), "disabled marker should exist after disable");

    repo.git_ai(&["repo", "enable"])
        .expect("repo enable should succeed");
    assert!(
        !disabled_marker.exists(),
        "disabled marker should be removed after repo enable"
    );
}

#[test]
#[serial]
fn repo_status_shows_enabled_disabled_and_reason() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "wrapper");
    let repo = TestRepo::new();

    // Default state: enabled
    let output = repo
        .git_ai(&["repo", "status"])
        .expect("repo status should succeed");
    assert!(
        output.contains("enabled"),
        "default status should say enabled, got: {output}"
    );
    assert!(
        output.contains("default"),
        "default reason should be 'default', got: {output}"
    );

    // After disable
    repo.git_ai(&["repo", "disable"])
        .expect("repo disable should succeed");
    let output = repo
        .git_ai(&["repo", "status"])
        .expect("repo status should succeed");
    assert!(
        output.contains("disabled"),
        "status after disable should say disabled, got: {output}"
    );
    assert!(
        output.contains("explicitly_disabled"),
        "reason after disable should be 'explicitly_disabled', got: {output}"
    );
}

#[test]
#[serial]
fn disabled_repo_stays_disabled_on_checkpoint() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "wrapper");
    let repo = TestRepo::new();
    let disabled_marker = git_hooks_ai_dir(&repo).join("disabled");

    repo.git_ai(&["repo", "disable"])
        .expect("repo disable should succeed");

    // Run a checkpoint -- should NOT re-enable
    fs::write(repo.path().join("file.txt"), "content\n").expect("write");
    repo.git(&["add", "file.txt"]).expect("add");
    repo.git_ai(&["checkpoint", "mock_ai", "file.txt"])
        .expect("checkpoint should succeed");

    assert!(
        disabled_marker.exists(),
        "checkpoint should not remove disabled marker after repo disable"
    );
}

#[test]
#[serial]
fn disable_survives_checkpoint_with_git_hooks_enabled_then_reenable() {
    let _mode = EnvVarGuard::set("GIT_AI_TEST_GIT_MODE", "wrapper");
    let repo = TestRepo::new();
    let disabled_marker = git_hooks_ai_dir(&repo).join("disabled");

    repo.git_ai(&["repo", "disable"])
        .expect("repo disable should succeed");
    assert!(disabled_marker.exists(), "disabled marker should be created");

    let status = repo
        .git_ai(&["repo", "status"])
        .expect("repo status should succeed");
    assert!(
        status.contains("explicitly_disabled"),
        "should show explicitly_disabled after disable, got: {status}"
    );

    // checkpoint with git_hooks_enabled=true should NOT override the disable
    fs::write(repo.path().join("file.txt"), "content\n").expect("write");
    repo.git(&["add", "file.txt"]).expect("add");
    repo.git_ai_with_env(
        &["checkpoint", "mock_ai", "file.txt"],
        &[("GIT_AI_GIT_HOOKS_ENABLED", "true")],
    )
    .expect("checkpoint should succeed");

    assert!(
        disabled_marker.exists(),
        "disabled marker should persist after checkpoint with git_hooks_enabled=true"
    );

    // Re-enable and verify it clears the disabled state
    repo.git_ai(&["repo", "enable"])
        .expect("repo enable should succeed");
    let status = repo
        .git_ai(&["repo", "status"])
        .expect("repo status should succeed");
    assert!(
        status.contains("enabled") && status.contains("default"),
        "should show enabled/default after re-enable, got: {status}"
    );
    assert!(
        !disabled_marker.exists(),
        "disabled marker should be removed after enable"
    );
}

crate::reuse_tests_in_worktree_with_attrs!(
    (#[serial_test::serial])
    repo_disable_creates_disabled_marker,
    repo_enable_removes_disabled_marker,
    repo_status_shows_enabled_disabled_and_reason,
    disabled_repo_stays_disabled_on_checkpoint,
    disable_survives_checkpoint_with_git_hooks_enabled_then_reenable,
);
