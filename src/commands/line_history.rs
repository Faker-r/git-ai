use crate::authorship::authorship_log_serialization::ChangeHistoryEntry;
use crate::error::GitAiError;
use crate::git::find_repository_in_path;
use crate::git::refs::get_reference_as_authorship_log_v3;
use crate::git::repository::{Repository, exec_git};
use serde::Serialize;

#[derive(Serialize)]
pub struct LineHistoryOutput {
    pub file: String,
    pub line: u32,
    pub at_commit: String,
    pub line_content: String,
    pub history: Vec<CommitHistoryEntry>,
}

#[derive(Serialize)]
pub struct CommitHistoryEntry {
    pub commit_sha: String,
    pub commit_date: String,
    pub commit_message: String,
    pub target_line: u32,
    pub checkpoints: Vec<MatchedCheckpoint>,
}

#[derive(Serialize)]
pub struct MatchedCheckpoint {
    pub timestamp: u64,
    pub kind: String,
    pub agent_type: Option<String>,
    pub model: Option<String>,
    pub prompt_id: Option<String>,
    pub prompt_text: Option<String>,
    pub additions: u32,
    pub deletions: u32,
    /// The content of the tracked line as it existed after this checkpoint was applied.
    /// Present when the checkpoint introduced or modified the line (i.e. it appears in
    /// `added_line_contents`). `None` for checkpoints that pre-date line-content recording
    /// or where the line was not in the added set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_content: Option<String>,
}

struct CommitInfo {
    sha: String,
    date: String,
    subject: String,
    target_line_in_commit: u32,
}

#[derive(Debug, Clone)]
enum DiffOp {
    Equal(usize),
    Delete(usize),
    Insert(usize),
}

pub fn handle_line_history(args: &[String]) {
    if args.len() < 2 {
        eprintln!("Usage: git-ai line-history <file> <line> [--commit <sha>]");
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

    if let Err(e) = run_line_history(&repo, file, line, commit) {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

pub fn run_line_history(
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

    let output = LineHistoryOutput {
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
) -> Result<CommitHistoryEntry, GitAiError> {
    let checkpoints = match get_reference_as_authorship_log_v3(repo, &commit.sha) {
        Ok(log) => {
            if let Some(mut ch) = log.metadata.change_history {
                hydrate_stripped_entries(&mut ch);
                find_checkpoints_that_touched_line(&ch, file, commit.target_line_in_commit)
            } else {
                vec![]
            }
        }
        Err(GitAiError::Generic(msg)) if msg.contains("No authorship note found") => vec![],
        Err(e) => return Err(e),
    };
    Ok(CommitHistoryEntry {
        commit_sha: commit.sha.clone(),
        commit_date: commit.date.clone(),
        commit_message: commit.subject.clone(),
        target_line: commit.target_line_in_commit,
        checkpoints,
    })
}

/// Replace stripped entries (Default mode: only `timestamp + kind + url` populated)
/// with full bodies fetched from CAS. Order is preserved; entries that fail to resolve
/// are left untouched so callers see the same behavior as today's bulk-resolver miss.
///
/// Cache hits run inline. Cache misses are batched into a single
/// `read_ca_prompt_store` call to avoid N round-trips per commit.
fn hydrate_stripped_entries(entries: &mut [ChangeHistoryEntry]) {
    use crate::authorship::secrets::entry_is_stripped;

    // Indices and hashes for entries that need a CAS fetch.
    let mut pending: Vec<(usize, String)> = Vec::new();

    for (idx, entry) in entries.iter_mut().enumerate() {
        if !entry_is_stripped(entry) {
            continue;
        }
        let Some(url) = entry.url.as_deref() else {
            continue;
        };
        let Some(hash) = url.rsplit('/').next().filter(|h| !h.is_empty()).map(String::from) else {
            continue;
        };

        if let Some(fetched) = try_resolve_entry_from_cache(&hash) {
            apply_fetched_entry(entry, fetched);
        } else {
            pending.push((idx, hash));
        }
    }

    if pending.is_empty() {
        return;
    }

    let fetched_by_hash = fetch_entries_from_api(&pending);
    for (idx, hash) in pending {
        if let Some(fetched) = fetched_by_hash.get(&hash) {
            apply_fetched_entry(&mut entries[idx], fetched.clone());
        }
    }
}

/// Overlay a CAS-resolved entry onto the stripped placeholder.
/// Preserves the original `url` so re-stripping detection still works.
fn apply_fetched_entry(target: &mut ChangeHistoryEntry, fetched: ChangeHistoryEntry) {
    let preserved_url = target.url.clone();
    *target = fetched;
    if target.url.is_none() {
        target.url = preserved_url;
    }
}

fn try_resolve_entry_from_cache(hash: &str) -> Option<ChangeHistoryEntry> {
    use crate::api::types::CasChangeHistoryEntryObject;
    use crate::authorship::internal_db::InternalDatabase;

    let db_mutex = InternalDatabase::global().ok()?;
    let db_guard = db_mutex.lock().ok()?;
    let cached_json = db_guard.get_cas_cache(hash).ok().flatten()?;
    let obj: CasChangeHistoryEntryObject = serde_json::from_str(&cached_json).ok()?;
    tracing::debug!("line-history: resolved change_history entry from cas_cache");
    Some(obj.entry)
}

/// Batch-fetch missing entries from the CAS API.
/// On any error or missing auth, returns an empty map (callers fall back to the
/// stripped placeholder, matching today's degradation behavior).
fn fetch_entries_from_api(
    pending: &[(usize, String)],
) -> std::collections::HashMap<String, ChangeHistoryEntry> {
    use crate::api::client::{ApiClient, ApiContext};
    use crate::api::types::CasChangeHistoryEntryObject;
    use crate::authorship::internal_db::InternalDatabase;
    use std::collections::HashMap;

    let mut out: HashMap<String, ChangeHistoryEntry> = HashMap::new();

    let context = ApiContext::new(None);
    if context.auth_token.is_none() {
        tracing::debug!("line-history: no auth token, skipping CAS API for {} entries", pending.len());
        return out;
    }

    let hashes: Vec<&str> = pending.iter().map(|(_, h)| h.as_str()).collect();
    let client = ApiClient::new(context);
    let response = match client.read_ca_prompt_store(&hashes) {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("line-history: CAS API error: {}", e);
            return out;
        }
    };

    for result in &response.results {
        if result.status != "ok" {
            continue;
        }
        let Some(content) = &result.content else { continue };
        let Ok(obj) = serde_json::from_value::<CasChangeHistoryEntryObject>(content.clone()) else {
            continue;
        };
        let json_str = serde_json::to_string(content).unwrap_or_default();
        if let Ok(db_mutex) = InternalDatabase::global()
            && let Ok(mut db_guard) = db_mutex.lock()
        {
            let _ = db_guard.set_cas_cache(&result.hash, &json_str);
        }
        out.insert(result.hash.clone(), obj.entry);
    }

    tracing::debug!(
        "line-history: resolved {}/{} change_history entries from CAS API",
        out.len(),
        pending.len()
    );
    out
}

// --- Line mapping algorithm (ported from tests/line_mapping_tests.rs) ---

/// Look up the content for a specific line number from a `added_line_contents` /
/// `deleted_line_contents` slice.  Each entry has the format `"N: <text>"` or `"N:<text>"`.
/// Returns the text portion when a matching entry is found.
fn lookup_line_content(contents: &[String], line: u32) -> Option<String> {
    let prefix_space = format!("{}: ", line);
    let prefix_nospace = format!("{}:", line);
    for entry in contents {
        if let Some(rest) = entry.strip_prefix(&prefix_space) {
            return Some(rest.to_string());
        }
        if let Some(rest) = entry.strip_prefix(&prefix_nospace) {
            return Some(rest.to_string());
        }
    }
    None
}

fn parse_line_ranges(ranges: &[String]) -> Vec<(u32, u32)> {
    ranges
        .iter()
        .filter_map(|s| {
            if let Some((start, end)) = s.split_once('-') {
                Some((start.parse().ok()?, end.parse().ok()?))
            } else {
                let n = s.parse().ok()?;
                Some((n, n))
            }
        })
        .collect()
}

fn find_checkpoints_that_touched_line(
    change_history: &[ChangeHistoryEntry],
    file: &str,
    target_line: u32,
) -> Vec<MatchedCheckpoint> {
    let relevant: Vec<&ChangeHistoryEntry> = change_history
        .iter()
        .filter(|entry| entry.files.contains_key(file))
        .collect();

    let mut matched = Vec::new();
    let mut current_line = target_line;

    for entry in relevant.iter().rev() {
        let detail = &entry.files[file];
        let added = parse_line_ranges(&detail.added_lines);
        let deleted = parse_line_ranges(&detail.deleted_lines);

        match map_new_to_old(current_line, &added, &deleted) {
            None => {
                let line_content =
                    lookup_line_content(&detail.added_line_contents, current_line);
                matched.push(MatchedCheckpoint {
                    timestamp: entry.timestamp,
                    kind: entry.kind.clone(),
                    agent_type: entry.agent_type.clone(),
                    model: entry.model.clone(),
                    prompt_id: entry.prompt_id.clone(),
                    prompt_text: entry.prompt_text.clone(),
                    additions: entry.line_stats.additions,
                    deletions: entry.line_stats.deletions,
                    line_content,
                });
                current_line = reverse_through_insert(current_line, &added, &deleted);
            }
            Some(old_line) => {
                current_line = old_line;
            }
        }
    }

    matched.reverse();
    matched
}

/// Map a new-file line number to its old-file line number, returning None if
/// the line was inserted (i.e. has no old-file counterpart).
fn map_new_to_old(new_line: u32, added: &[(u32, u32)], deleted: &[(u32, u32)]) -> Option<u32> {
    let ops = reconstruct_diff_ops(added, deleted);
    let mut old_pos = 1u32;
    let mut new_pos = 1u32;

    for op in &ops {
        match op {
            DiffOp::Equal(n) => {
                let n = *n as u32;
                if new_line >= new_pos && new_line < new_pos + n {
                    return Some(old_pos + (new_line - new_pos));
                }
                old_pos += n;
                new_pos += n;
            }
            DiffOp::Insert(n) => {
                let n = *n as u32;
                if new_line >= new_pos && new_line < new_pos + n {
                    return None;
                }
                new_pos += n;
            }
            DiffOp::Delete(n) => {
                old_pos += *n as u32;
            }
        }
    }
    Some(old_pos + (new_line - new_pos))
}

/// When a line falls inside an Insert range, compute the corresponding old-file
/// position. If the Insert was preceded by a Delete (i.e. a replacement), this
/// returns the start of the replaced old-file range so that earlier checkpoints
/// can be traced through the content that was overwritten.
fn reverse_through_insert(
    new_line: u32,
    added: &[(u32, u32)],
    deleted: &[(u32, u32)],
) -> u32 {
    let ops = reconstruct_diff_ops(added, deleted);
    let mut old_pos = 1u32;
    let mut new_pos = 1u32;
    let mut pre_delete_old_pos = old_pos;

    for op in &ops {
        match op {
            DiffOp::Equal(n) => {
                let n = *n as u32;
                old_pos += n;
                new_pos += n;
                pre_delete_old_pos = old_pos;
            }
            DiffOp::Delete(n) => {
                pre_delete_old_pos = old_pos;
                old_pos += *n as u32;
            }
            DiffOp::Insert(n) => {
                let n = *n as u32;
                if new_line >= new_pos && new_line < new_pos + n {
                    return pre_delete_old_pos;
                }
                new_pos += n;
                pre_delete_old_pos = old_pos;
            }
        }
    }
    old_pos
}

/// Reconstruct interleaved Equal/Delete/Insert operations from separate
/// added_ranges (new-file coordinates) and deleted_ranges (old-file
/// coordinates) by walking two cursors.
fn reconstruct_diff_ops(added: &[(u32, u32)], deleted: &[(u32, u32)]) -> Vec<DiffOp> {
    let mut ops = Vec::new();
    let mut old_pos = 1u32;
    let mut new_pos = 1u32;
    let mut add_idx = 0usize;
    let mut del_idx = 0usize;

    loop {
        let next_del_start = deleted.get(del_idx).map(|(s, _)| *s);
        let next_add_start = added.get(add_idx).map(|(s, _)| *s);

        match (next_del_start, next_add_start) {
            (None, None) => break,
            (Some(del_start), None) => {
                let equal = del_start - old_pos;
                if equal > 0 {
                    ops.push(DiffOp::Equal(equal as usize));
                    old_pos += equal;
                    new_pos += equal;
                }
                let del_count = deleted[del_idx].1 - deleted[del_idx].0 + 1;
                ops.push(DiffOp::Delete(del_count as usize));
                old_pos += del_count;
                del_idx += 1;
            }
            (None, Some(add_start)) => {
                let equal = add_start - new_pos;
                if equal > 0 {
                    ops.push(DiffOp::Equal(equal as usize));
                    old_pos += equal;
                    new_pos += equal;
                }
                let add_count = added[add_idx].1 - added[add_idx].0 + 1;
                ops.push(DiffOp::Insert(add_count as usize));
                new_pos += add_count;
                add_idx += 1;
            }
            (Some(del_start), Some(add_start)) => {
                let gap_to_del = del_start - old_pos;
                let gap_to_add = add_start - new_pos;
                let equal = gap_to_del.min(gap_to_add);
                if equal > 0 {
                    ops.push(DiffOp::Equal(equal as usize));
                    old_pos += equal;
                    new_pos += equal;
                }
                if old_pos == del_start {
                    let del_count = deleted[del_idx].1 - deleted[del_idx].0 + 1;
                    ops.push(DiffOp::Delete(del_count as usize));
                    old_pos += del_count;
                    del_idx += 1;
                }
                if new_pos == add_start {
                    let add_count = added[add_idx].1 - added[add_idx].0 + 1;
                    ops.push(DiffOp::Insert(add_count as usize));
                    new_pos += add_count;
                    add_idx += 1;
                }
            }
        }
    }

    ops
}