use crate::authorship::authorship_log_serialization::ChangeHistoryEntry;
use crate::commands::line_history::hydrate_stripped_entries;
use crate::error::GitAiError;
use crate::git::find_repository_in_path;
use crate::git::refs::get_reference_as_authorship_log_v3;
use crate::git::repository::{Repository, exec_git};
use serde::Serialize;

#[derive(Serialize)]
pub struct ChangeHistoryOutput {
    pub start: String,
    pub end: String,
    pub commits: Vec<CommitChangeHistory>,
}

#[derive(Serialize)]
pub struct CommitChangeHistory {
    pub commit_sha: String,
    pub commit_date: String,
    pub commit_message: String,
    pub change_history: Vec<ChangeHistoryEntry>,
}

struct CommitInfo {
    sha: String,
    date: String,
    subject: String,
}

pub fn handle_change_history(args: &[String]) {
    if args.len() < 2 {
        eprintln!("Usage: git-ai change-history <start> <end>");
        std::process::exit(1);
    }

    let start = &args[0];
    let end = &args[1];

    let current_dir = std::env::current_dir().unwrap();
    let repo = match find_repository_in_path(current_dir.to_str().unwrap()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: not in a git repository: {}", e);
            std::process::exit(1);
        }
    };

    if let Err(e) = run_change_history(&repo, start, end) {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

pub fn run_change_history(repo: &Repository, start: &str, end: &str) -> Result<(), GitAiError> {
    let commits = git_log_commits(repo, start, end)?;

    let mut output_commits = Vec::new();
    for c in &commits {
        output_commits.push(build_commit_change_history(repo, c)?);
    }

    let output = ChangeHistoryOutput {
        start: start.to_string(),
        end: end.to_string(),
        commits: output_commits,
    };
    println!("{}", serde_json::to_string_pretty(&output).unwrap());
    Ok(())
}

/// Enumerate commits in `<start>^..<end>`, newest-first.
///
/// `<start>^..<end>` is git's conventional "both endpoints inclusive" range
/// syntax. It will fail if `<start>` is a root commit (since `<start>^` doesn't
/// resolve); callers passing a root commit should pass `<start>` as `<end>` of
/// a separate single-commit query instead.
///
/// Uses git log's default output format and parses it directly.
fn git_log_commits(
    repo: &Repository,
    start: &str,
    end: &str,
) -> Result<Vec<CommitInfo>, GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.extend([
        "log".to_string(),
        format!("{}^..{}", start, end),
    ]);
    let output = exec_git(&args)?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    let mut commits = Vec::new();
    let mut current: Option<CommitInfo> = None;
    let mut subject_captured = false;

    for output_line in stdout.lines() {
        if let Some(rest) = output_line.strip_prefix("commit ") {
            if let Some(c) = current.take() {
                commits.push(c);
            }
            let sha = rest.split_whitespace().next().unwrap_or("").to_string();
            current = Some(CommitInfo {
                sha,
                date: String::new(),
                subject: String::new(),
            });
            subject_captured = false;
        } else if let Some(c) = current.as_mut() {
            if let Some(d) = output_line.strip_prefix("Date:") {
                c.date = d.trim().to_string();
            } else if !subject_captured
                && let Some(sub) = output_line.strip_prefix("    ")
                && !sub.is_empty()
            {
                c.subject = sub.to_string();
                subject_captured = true;
            }
        }
    }
    if let Some(c) = current.take() {
        commits.push(c);
    }
    Ok(commits)
}

fn build_commit_change_history(
    repo: &Repository,
    commit: &CommitInfo,
) -> Result<CommitChangeHistory, GitAiError> {
    let change_history = match get_reference_as_authorship_log_v3(repo, &commit.sha) {
        Ok(log) => {
            if let Some(mut ch) = log.metadata.change_history {
                hydrate_stripped_entries(&mut ch);
                ch
            } else {
                vec![]
            }
        }
        Err(GitAiError::Generic(msg)) if msg.contains("No authorship note found") => vec![],
        Err(e) => return Err(e),
    };
    Ok(CommitChangeHistory {
        commit_sha: commit.sha.clone(),
        commit_date: commit.date.clone(),
        commit_message: commit.subject.clone(),
        change_history,
    })
}
