use crate::authorship::transcript::Message;
use crate::error::GitAiError;
use crate::git::find_repository_in_path;
use crate::git::refs::get_reference_as_authorship_log_v3;
use crate::git::repository::{Repository, exec_git};
use serde::Serialize;

/// Output of `git ai message-history <file> <line>`. Walks `git log -L<line>:<file>`
/// and, for every commit that touched the line, looks up the latest user message
/// that the message-level attestation attributes to that line.
#[derive(Serialize)]
pub struct MessageHistoryOutput {
    pub file: String,
    pub line: u32,
    pub at_commit: String,
    pub line_content: String,
    pub history: Vec<CommitMessageEntry>,
}

#[derive(Serialize)]
pub struct CommitMessageEntry {
    pub commit_sha: String,
    pub commit_date: String,
    pub commit_message: String,
    pub target_line: u32,
    /// The line's content at this commit (from `git show <commit>:<file>` at
    /// `target_line`). `None` when the file/line couldn't be read at that commit
    /// (e.g. file didn't yet exist, or `target_line` is out of range).
    pub line_content: Option<String>,
    /// The user message that owns the line at this commit, or `None` if the
    /// note has no message_attestations covering this line (pre-feature commits,
    /// or lines whose latest editor was a Human checkpoint — see Q2=a in the plan).
    pub message: Option<MatchedMessage>,
}

#[derive(Serialize)]
pub struct MatchedMessage {
    pub message_id: String,
    /// Conversation hash (matches a key in `metadata.prompts`) that owns the message.
    pub conversation_id: Option<String>,
    pub agent_type: Option<String>,
    pub model: Option<String>,
    pub text: Option<String>,
    pub timestamp: Option<String>,
}

struct CommitInfo {
    sha: String,
    date: String,
    subject: String,
    target_line_in_commit: u32,
}

pub fn handle_message_history(args: &[String]) {
    if args.len() < 2 {
        eprintln!("Usage: git-ai message-history <file> <line> [--commit <sha>]");
        std::process::exit(1);
    }

    let file = &args[0];
    let line: u32 = match args[1].parse() {
        Ok(n) => n,
        Err(_) => {
            eprintln!("Error: line must be a number, got '{}'", args[1]);
            std::process::exit(1);
        }
    };
    let commit = args
        .iter()
        .position(|a| a == "--commit")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str());

    let current_dir = std::env::current_dir().unwrap();
    let repo = match find_repository_in_path(current_dir.to_str().unwrap()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: not in a git repository: {}", e);
            std::process::exit(1);
        }
    };

    if let Err(e) = run_message_history(&repo, file, line, commit) {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

pub fn run_message_history(
    repo: &Repository,
    file: &str,
    line: u32,
    commit: Option<&str>,
) -> Result<(), GitAiError> {
    let commit_ref = commit.unwrap_or("HEAD");

    let line_content = read_line_at_commit(repo, file, line, commit_ref)?;
    let commits = git_log_line_history(repo, file, line, commit_ref)?;

    let mut history = Vec::new();
    for c in &commits {
        history.push(build_commit_entry(repo, c, file)?);
    }

    let output = MessageHistoryOutput {
        file: file.to_string(),
        line,
        at_commit: commit_ref.to_string(),
        line_content,
        history,
    };
    println!("{}", serde_json::to_string_pretty(&output).unwrap());
    Ok(())
}

fn read_line_at_commit(
    repo: &Repository,
    file: &str,
    line: u32,
    commit: &str,
) -> Result<String, GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.push("show".to_string());
    args.push(format!("{}:{}", commit, file));
    let output = exec_git(&args)?;
    let content = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = content.lines().collect();
    if line == 0 || line as usize > lines.len() {
        return Err(GitAiError::Generic(format!(
            "Line {} is out of range (file has {} lines)",
            line,
            lines.len()
        )));
    }
    Ok(lines[(line - 1) as usize].to_string())
}

/// Walk `git log -L<line>,<line>:<file>` and track how the target line number
/// shifts across commits via the `@@ ... +N` headers. Mirrors the equivalent
/// helper in `commands/line_history.rs`.
fn git_log_line_history(
    repo: &Repository,
    file: &str,
    line: u32,
    commit: &str,
) -> Result<Vec<CommitInfo>, GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.extend([
        "log".to_string(),
        format!("-L{},{}:{}", line, line, file),
        "--format=COMMIT %H %aI %s".to_string(),
        commit.to_string(),
    ]);
    let output = exec_git(&args)?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    let mut commits = Vec::new();
    let mut current: Option<(String, String, String)> = None;
    let mut current_target_line = line;

    for output_line in stdout.lines() {
        if let Some(rest) = output_line.strip_prefix("COMMIT ") {
            if let Some((sha, date, subject)) = current.take() {
                commits.push(CommitInfo {
                    sha,
                    date,
                    subject,
                    target_line_in_commit: current_target_line,
                });
                current_target_line = line;
            }
            if let (Some(sha), Some(rest)) = (rest.get(..40), rest.get(41..)) {
                if let Some((date, subject)) = rest.split_once(' ') {
                    current = Some((sha.to_string(), date.to_string(), subject.to_string()));
                }
            }
        } else if output_line.starts_with("@@") {
            if let Some(plus_part) = output_line.split('+').nth(1) {
                let num_str = plus_part
                    .split(|c: char| !c.is_ascii_digit())
                    .next()
                    .unwrap_or("0");
                if let Ok(n) = num_str.parse::<u32>() {
                    current_target_line = n;
                }
            }
        }
    }
    if let Some((sha, date, subject)) = current.take() {
        commits.push(CommitInfo {
            sha,
            date,
            subject,
            target_line_in_commit: current_target_line,
        });
    }

    Ok(commits)
}

fn build_commit_entry(
    repo: &Repository,
    commit: &CommitInfo,
    file: &str,
) -> Result<CommitMessageEntry, GitAiError> {
    let message = match get_reference_as_authorship_log_v3(repo, &commit.sha) {
        Ok(log) => find_message_for_line(&log, file, commit.target_line_in_commit),
        Err(GitAiError::Generic(msg)) if msg.contains("No authorship note found") => None,
        Err(e) => return Err(e),
    };

    // Read what the line looked like at this commit. Errors (file missing, out of
    // range) are non-fatal — they just produce `None` so the caller still gets the
    // commit row.
    let line_content =
        read_line_at_commit(repo, file, commit.target_line_in_commit, &commit.sha).ok();

    Ok(CommitMessageEntry {
        commit_sha: commit.sha.clone(),
        commit_date: commit.date.clone(),
        commit_message: commit.subject.clone(),
        target_line: commit.target_line_in_commit,
        line_content,
        message,
    })
}

/// Look up the message_id whose attestation entry for `file` contains `line`.
/// Resolve it to its conversation by scanning `metadata.prompts.<conv>.messages`
/// for the matching `Message::User { id, .. }`.
fn find_message_for_line(
    log: &crate::authorship::authorship_log_serialization::AuthorshipLog,
    file: &str,
    line: u32,
) -> Option<MatchedMessage> {
    let file_att = log
        .message_attestations
        .iter()
        .find(|fa| fa.file_path == file)?;

    let entry = file_att
        .entries
        .iter()
        .find(|e| e.line_ranges.iter().any(|r| r.contains(line)))?;

    let message_id = entry.hash.clone();

    // Find the conversation that owns this message id.
    for (conv_id, prompt) in &log.metadata.prompts {
        for m in &prompt.messages {
            if let Message::User { id: Some(id), text, timestamp } = m
                && id == &message_id
            {
                return Some(MatchedMessage {
                    message_id,
                    conversation_id: Some(conv_id.clone()),
                    agent_type: Some(prompt.agent_id.tool.clone()),
                    model: Some(prompt.agent_id.model.clone()),
                    text: Some(text.clone()),
                    timestamp: timestamp.clone(),
                });
            }
        }
    }

    // Message id present in the attestation but not findable in any conversation's
    // transcript — surface what we know rather than failing.
    Some(MatchedMessage {
        message_id,
        conversation_id: None,
        agent_type: None,
        model: None,
        text: None,
        timestamp: None,
    })
}
