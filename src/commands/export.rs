use crate::authorship::authorship_log::{LineRange, PromptRecord};
use crate::authorship::authorship_log_serialization::{
    AuthorshipLog, AuthorshipMetadata, ChangeHistoryEntry, FileChangeDetail,
};
use crate::authorship::transcript::Message;
use crate::error::GitAiError;
use crate::git::find_repository;
use crate::git::refs::{CommitAuthorship, get_commits_with_notes_from_list};
use crate::git::repository::{CommitRange, Repository, exec_git};
use chrono::{DateTime, TimeZone, Utc};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;

pub fn handle_export(args: &[String]) {
    let mut revspec: Option<String> = None;
    let mut output_path: Option<String> = None;
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        match a.as_str() {
            "-o" | "--output" => {
                output_path = iter.next().cloned();
                if output_path.is_none() {
                    eprintln!("Error: -o requires a file path");
                    std::process::exit(1);
                }
            }
            "-h" | "--help" => {
                print_usage();
                return;
            }
            other => {
                if revspec.is_some() {
                    eprintln!("Error: export accepts exactly one revision or range");
                    std::process::exit(1);
                }
                revspec = Some(other.to_string());
            }
        }
    }

    let Some(spec) = revspec else {
        print_usage();
        std::process::exit(1);
    };

    let repo = match find_repository(&Vec::<String>::new()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Failed to find repository: {}", e);
            std::process::exit(1);
        }
    };

    if let Err(e) = run_export(&repo, &spec, output_path.as_deref()) {
        eprintln!("Failed to export authorship: {}", e);
        std::process::exit(1);
    }
}

fn print_usage() {
    eprintln!("Usage: git-ai export <rev|range> [-o <file.html>]");
    eprintln!();
    eprintln!("Renders the authorship log for a revision or range as a self-contained HTML page.");
    eprintln!("If -o is omitted, HTML is written to stdout.");
}

fn run_export(repo: &Repository, spec: &str, output_path: Option<&str>) -> Result<(), GitAiError> {
    let commits = resolve_commits(repo, spec)?;
    let entries = if commits.is_empty() {
        Vec::new()
    } else {
        get_commits_with_notes_from_list(repo, &commits)?
    };

    let html = render_html(repo, spec, &entries);

    match output_path {
        Some(path) => {
            fs::write(path, html)
                .map_err(|e| GitAiError::Generic(format!("Failed to write {}: {}", path, e)))?;
            eprintln!("Wrote authorship HTML to {}", path);
        }
        None => {
            print!("{}", html);
        }
    }
    Ok(())
}

fn resolve_commits(repo: &Repository, spec: &str) -> Result<Vec<String>, GitAiError> {
    if let Some((start, end)) = spec.split_once("..") {
        if start.is_empty() || end.is_empty() {
            return Err(GitAiError::Generic(
                "Invalid commit range format. Expected <start>..<end>".to_string(),
            ));
        }
        let range = CommitRange::new_infer_refname(repo, start.to_string(), end.to_string(), None)?;
        let mut commits: Vec<String> = range.into_iter().map(|c| c.id()).collect();
        if commits.is_empty() {
            let end_commit = repo.revparse_single(end)?;
            commits.push(end_commit.id());
        }
        Ok(commits)
    } else {
        let commit = repo.revparse_single(spec)?;
        Ok(vec![commit.id()])
    }
}

struct CommitMeta {
    commit_message: String,
    date: String,
    author: String,
    /// Distinct AI agent labels (`tool (model)`) appearing in attestations for this commit.
    attributing_agents: Vec<String>,
}

fn fetch_commit_meta(repo: &Repository, sha: &str) -> CommitMeta {
    let mut args = repo.global_args_for_exec();
    args.extend([
        "log".to_string(),
        "-1".to_string(),
        "--format=%an <%ae>%n%aI%n%s".to_string(),
        sha.to_string(),
    ]);
    let default = CommitMeta {
        commit_message: String::new(),
        date: String::new(),
        author: String::new(),
        attributing_agents: Vec::new(),
    };
    let Ok(out) = exec_git(&args) else { return default };
    let s = String::from_utf8_lossy(&out.stdout);
    let mut lines = s.lines();
    CommitMeta {
        author: lines.next().unwrap_or("").to_string(),
        date: format_git_iso_date_for_export(lines.next().unwrap_or("")),
        commit_message: lines.next().unwrap_or("").to_string(),
        attributing_agents: Vec::new(),
    }
}

/// Single display format for every timestamp in export HTML (`YYYY-MM-DD HH:MM:SS UTC`).
fn format_export_datetime_utc(dt: DateTime<Utc>) -> String {
    format!("{} UTC", dt.format("%Y-%m-%d %H:%M:%S"))
}

/// Git `%aI` author date (RFC3339). Converts to UTC [format_export_datetime_utc].
fn format_git_iso_date_for_export(raw: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        return String::new();
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return format_export_datetime_utc(dt.with_timezone(&Utc));
    }
    raw.to_string()
}

fn parse_transcript_timestamp_to_utc(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    if s.chars().all(|c| c.is_ascii_digit()) {
        if let Ok(n) = s.parse::<i64>() {
            if n > 9_999_999_999 {
                return DateTime::<Utc>::from_timestamp_millis(n);
            }
            return DateTime::<Utc>::from_timestamp(n, 0);
        }
    }
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
        return Some(Utc.from_utc_datetime(&naive));
    }
    None
}

fn format_transcript_timestamp_for_export(ts: &Option<String>) -> Option<String> {
    let raw = ts.as_ref().map(|s| s.trim())?;
    if raw.is_empty() {
        return None;
    }
    Some(
        parse_transcript_timestamp_to_utc(raw)
            .map(format_export_datetime_utc)
            .unwrap_or_else(|| raw.to_string()),
    )
}

fn render_html(repo: &Repository, spec: &str, entries: &[CommitAuthorship]) -> String {
    let mut body = String::new();
    let _ = writeln!(body, "<header><h1>Authorship log</h1></header>");
    kv_grid(
        &mut body,
        &[
            ("Revision", html_code(spec)),
            ("Commits", entries.len().to_string()),
        ],
    );
    render_message_filter_bar(&mut body);

    if entries.is_empty() {
        body.push_str("<p class=\"empty\">No authorship data found for this revision.</p>");
    }

    for entry in entries {
        match entry {
            CommitAuthorship::Log {
                sha,
                authorship_log,
                ..
            } => {
                let mut meta = fetch_commit_meta(repo, sha);
                meta.attributing_agents = collect_attributing_agents(authorship_log);
                render_commit(&mut body, sha, &meta, Some(authorship_log));
            }
            CommitAuthorship::NoLog { sha, .. } => {
                let meta = fetch_commit_meta(repo, sha);
                render_commit(&mut body, sha, &meta, None);
            }
        }
    }

    wrap_document(spec, &body)
}

fn render_commit(out: &mut String, sha: &str, meta: &CommitMeta, log: Option<&AuthorshipLog>) {
    // let short = &sha[..sha.len().min(12)];
    let short = sha;
    let _ = writeln!(out, "<details class=\"commit\" open>");
    let _ = writeln!(
        out,
        "<summary>
        <span class=\"sha\">Commit {}</span> 

        </summary>",
        esc(short)
        // esc(&meta.author)
    );

    let mut rows: Vec<(&str, String)> = vec![
        // ("Commit", html_code(sha)),
        ("Author", esc(&meta.author)),
        ("Date", esc(&meta.date)),
        ("Message", esc(&meta.commit_message)),
    ];
    if !meta.attributing_agents.is_empty() {
        let agents = meta
            .attributing_agents
            .iter()
            .map(|s| esc(s))
            .collect::<Vec<_>>()
            .join(", ");
        rows.push(("AI agents (attested)", agents));
    }
    kv_grid(out, &rows);

    let Some(log) = log else {
        out.push_str("<p class=\"empty\">No authorship data for this commit.</p></details>\n");
        return;
    };

    render_attestations(out, log);
    render_prompts(out, &log.metadata);
    render_humans(out, &log.metadata);
    render_change_history(out, &log.metadata);

    out.push_str("</details>\n");
}

/// Render a labeled key-value grid. Values are HTML strings (already escaped).
fn kv_grid(out: &mut String, rows: &[(&str, String)]) {
    let visible: Vec<_> = rows.iter().filter(|(_, v)| !v.is_empty()).collect();
    if visible.is_empty() {
        return;
    }
    out.push_str("<dl class=\"kv\">");
    for (k, v) in visible {
        let _ = write!(out, "<dt>{}</dt><dd>{}</dd>", esc(k), v);
    }
    out.push_str("</dl>");
}

fn html_code(s: &str) -> String {
    format!("<code>{}</code>", esc(s))
}

fn render_attestations(out: &mut String, log: &AuthorshipLog) {
    if log.attestations.is_empty() {
        return;
    }
    out.push_str("<section><h2>Attestations</h2>");
    for fa in &log.attestations {
        let _ = writeln!(
            out,
            "<details class=\"file\" open><summary class=\"file-path\">{}</summary>",
            esc(&fa.file_path)
        );
        out.push_str("<table class=\"attest\"><thead><tr><th>Hash</th><th>Human/AI agents</th><th>Lines</th></tr></thead><tbody>");
        for entry in &fa.entries {
            let (label, css_class) = describe_author(&entry.hash, &log.metadata);
            let ranges = entry
                .line_ranges
                .iter()
                .map(format_range)
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(
                out,
                "<tr class=\"{}\"><td><code>{}</code></td><td>{}</td><td><code>{}</code></td></tr>",
                css_class,
                esc(&entry.hash),
                esc(&label),
                esc(&ranges)
            );
        }
        out.push_str("</tbody></table></details>");
    }
    out.push_str("</section>");
}

fn format_range(r: &LineRange) -> String {
    match r {
        LineRange::Single(l) => l.to_string(),
        LineRange::Range(a, b) => format!("{}-{}", a, b),
    }
}

fn prompt_agent_label(p: &PromptRecord) -> String {
    format!("{} ({})", p.agent_id.tool, p.agent_id.model)
}

/// Distinct `tool (model)` labels for AI sessions that appear in this log’s attestations.
fn collect_attributing_agents(log: &AuthorshipLog) -> Vec<String> {
    let mut labels: BTreeSet<String> = BTreeSet::new();
    for fa in &log.attestations {
        for entry in &fa.entries {
            if let Some(p) = log.metadata.prompts.get(&entry.hash) {
                labels.insert(prompt_agent_label(p));
            }
        }
    }
    labels.into_iter().collect()
}

fn describe_author(hash: &str, meta: &AuthorshipMetadata) -> (String, &'static str) {
    if let Some(stripped) = hash.strip_prefix("h_") {
        if let Some(h) = meta.humans.get(hash) {
            return (format!("{} (human)", h.author), "human");
        }
        return (format!("human {}", stripped), "human");
    }
    if let Some(p) = meta.prompts.get(hash) {
        let label = prompt_agent_label(p);
        return (label, "agent");
    }
    (hash.to_string(), "unknown")
}

fn render_prompts(out: &mut String, meta: &AuthorshipMetadata) {
    if meta.prompts.is_empty() {
        return;
    }
    out.push_str("<section><h2>AI Conversations</h2>");
    for (hash, p) in &meta.prompts {
        let _ = writeln!(out, "<details class=\"prompt\">");
        let who = p.human_author.clone().unwrap_or_else(|| "(unknown)".into());
        let _ = writeln!(
            out,
            "<summary>
            <code class=\"hash\">{}</code> 
            <span class=\"badge agent\">{} ({})</span>
            <span class=\"sub\">+{} lines, −{} lines</span>
            </summary>",
            esc(hash),
            esc(&p.agent_id.tool),
            esc(&p.agent_id.model),
            // p.messages.len(),
            p.total_additions,
            p.total_deletions
        );

        let user_message_count = p
            .messages
            .iter()
            .filter(|m| matches!(m, Message::User { .. }))
            .count();
        let assistant_message_count = p
            .messages
            .iter()
            .filter(|m| matches!(m, Message::Assistant { .. }))
            .count();
        let tool_message_count = p
            .messages
            .iter()
            .filter(|m| matches!(m, Message::ToolUse { .. }))
            .count();
        let thinking_message_count = p
            .messages
            .iter()
            .filter(|m| matches!(m, Message::Thinking { .. }))
            .count();
        let plan_message_count = p
            .messages
            .iter()
            .filter(|m| matches!(m, Message::Plan { .. }))
            .count();
        let mut rows: Vec<(&str, String)> = vec![
            ("Hash", html_code(hash)),
            ("Tool", esc(&p.agent_id.tool)),
            ("Model", esc(&p.agent_id.model)),
            ("Conversation ID", html_code(&p.agent_id.id)),
            ("Human Author", esc(&who)),
            ("All Messages Count", p.messages.len().to_string()),
            ("- User Messages", user_message_count.to_string()),
            ("- Assistant Messages", assistant_message_count.to_string()),
            ("- Tool Use Messages", tool_message_count.to_string()),
            ("- Thinking Messages", thinking_message_count.to_string()),
            ("- Plan Messages", plan_message_count.to_string()),
            ("Total Added Lines", p.total_additions.to_string()),
            ("Total Deleted Lines", p.total_deletions.to_string()),
        ];
        if p.accepted_lines > 0 || p.overriden_lines > 0 {
            rows.push(("Total Accepted Lines", p.accepted_lines.to_string()));
            rows.push(("Total Overridden Lines", p.overriden_lines.to_string()));
        }
        if let Some(url) = &p.messages_url {
            rows.push((
                "Conversation URL",
                format!("<a href=\"{}\">{}</a>", esc(url), esc(url)),
            ));
        }
        if let Some(subs) = &p.subagents {
            if !subs.is_empty() {
                rows.push(("Subagents", esc(&subs.join(", "))));
            }
        }
        kv_grid(out, &rows);

        if let Some(attrs) = &p.custom_attributes {
            if !attrs.is_empty() {
                let attr_rows: Vec<(&str, String)> = attrs
                    .iter()
                    .map(|(k, v)| (k.as_str(), esc(v)))
                    .collect();
                out.push_str("<h3 class=\"sub-h\">Custom attributes</h3>");
                kv_grid(out, &attr_rows);
            }
        }

        if !p.messages.is_empty() {
            let _ = writeln!(
                out,
                "<details class=\"messages-block\"><summary class=\"sub-h\">All Messages ({})</summary>",
                p.messages.len()
            );
            render_messages(out, &p.messages);
            out.push_str("</details>");
        }
        out.push_str("</details>");
    }
    out.push_str("</section>");
}

/// One row in `<ol class="messages">`, matching transcript rendering in AI Sessions.
fn render_message_line_item(
    out: &mut String,
    idx: usize,
    role: &str,
    body_html: &str,
    ts: Option<&str>,
    id: Option<&str>,
) {
    let ts_html = ts
        .map(|t| format!(" <span class=\"ts\">{}</span>", esc(t)))
        .unwrap_or_default();
    let id_html = id
        .map(|v| format!(" <span class=\"msg-id\">ID: <code>{}</code></span>", esc(v)))
        .unwrap_or_default();
    let _ = writeln!(
        out,
        "<li class=\"msg msg-{role}\">
        <div class=\"msg-head\">
        <span class=\"msg-idx\">#{idx}</span>
        <span class=\"role\">{role}</span>
        {ts}
        {id}
        </div>
        {body}
        </li>",
        role = role,
        idx = idx,
        id = id_html,
        ts = ts_html,
        body = body_html,
    );
}

fn render_message_filter_bar(out: &mut String) {
    out.push_str(
        r#"<div class="msg-filter" role="group" aria-label="Message type filter">
        <span class="msg-filter-label">Show messages:</span>
        <label><input type="checkbox" class="msg-filter-cb" data-type="user" checked> User</label>
        <label><input type="checkbox" class="msg-filter-cb" data-type="assistant" checked> Assistant</label>
        <label><input type="checkbox" class="msg-filter-cb" data-type="tool" checked> Tool</label>
        <label><input type="checkbox" class="msg-filter-cb" data-type="thinking" checked> Thinking</label>
        <label><input type="checkbox" class="msg-filter-cb" data-type="plan" checked> Plan</label>
        </div>"#,
    );
}

fn render_messages(out: &mut String, messages: &[Message]) {
    if messages.is_empty() {
        return;
    }
    out.push_str("<ol class=\"messages\">");
    for (i, m) in messages.iter().enumerate() {
        let (role, body_html, ts_raw) = match m {
            Message::User { text, timestamp, .. } => {
                ("user", format!("<pre>{}</pre>", esc(text)), timestamp.clone())
            }
            Message::Assistant { text, timestamp, .. } => (
                "assistant",
                format!("<pre>{}</pre>", esc(text)),
                timestamp.clone(),
            ),
            Message::Thinking { text, timestamp, .. } => (
                "thinking",
                format!("<pre>{}</pre>", esc(text)),
                timestamp.clone(),
            ),
            Message::Plan { text, timestamp, .. } => {
                ("plan", format!("<pre>{}</pre>", esc(text)), timestamp.clone())
            }
            Message::ToolUse {
                name,
                input,
                timestamp,
                ..
            } => (
                "tool",
                render_tool_use(name, input),
                timestamp.clone(),
            ),
        };
        let ts = format_transcript_timestamp_for_export(&ts_raw);
        let id = m.id().map(|s| s.as_str());
        render_message_line_item(
            out,
            i,
            role,
            &body_html,
            ts.as_deref(),
            id,
        );
    }
    out.push_str("</ol>");
}

fn render_tool_use(name: &str, input: &serde_json::Value) -> String {
    let mut out = String::new();
    let _ = write!(
        out,
        "<div class=\"tool-head\"><span class=\"tool-label\">tool</span> <code class=\"tool-name\">{}</code></div>",
        esc(name)
    );

    // For object inputs, render top-level keys as a kv-grid: scalars stay inline,
    // nested structures (including strings that themselves parse as JSON) render
    // as their own pretty-printed <pre>. This makes nested JSON readable instead
    // of one long escaped line.
    if let serde_json::Value::Object(map) = input {
        let mut rows: Vec<(String, String)> = Vec::new();
        for (k, v) in map {
            let value_html = match v {
                serde_json::Value::String(s) => render_string_field(s),
                serde_json::Value::Null => "<span class=\"sub\">null</span>".to_string(),
                serde_json::Value::Bool(b) => format!("<code>{}</code>", b),
                serde_json::Value::Number(n) => format!("<code>{}</code>", n),
                _ => format!(
                    "<pre class=\"json\">{}</pre>",
                    esc(&serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string()))
                ),
            };
            rows.push((k.clone(), value_html));
        }
        out.push_str("<dl class=\"kv tool-args\">");
        for (k, v) in &rows {
            let _ = write!(out, "<dt>{}</dt><dd>{}</dd>", esc(k), v);
        }
        out.push_str("</dl>");
    } else {
        let pretty = serde_json::to_string_pretty(input).unwrap_or_else(|_| input.to_string());
        let _ = write!(out, "<pre class=\"json\">{}</pre>", esc(&pretty));
    }

    out
}

/// Render a string field: if it parses as JSON, pretty-print; if it has multiple lines,
/// use a <pre>; otherwise inline as <code>.
fn render_string_field(s: &str) -> String {
    let trimmed = s.trim();
    if (trimmed.starts_with('{') && trimmed.ends_with('}'))
        || (trimmed.starts_with('[') && trimmed.ends_with(']'))
    {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Ok(pretty) = serde_json::to_string_pretty(&v) {
                return format!("<pre class=\"json\">{}</pre>", esc(&pretty));
            }
        }
    }
    if s.contains('\n') {
        format!("<pre>{}</pre>", esc(s))
    } else {
        format!("<code>{}</code>", esc(s))
    }
}

fn render_humans(out: &mut String, meta: &AuthorshipMetadata) {
    if meta.humans.is_empty() {
        return;
    }
    out.push_str("<section><h2>Known humans</h2><table class=\"humans\"><thead><tr><th>Hash</th><th>Author</th></tr></thead><tbody>");
    for (hash, h) in &meta.humans {
        let _ = writeln!(
            out,
            "<tr><td><code>{}</code></td><td>{}</td></tr>",
            esc(hash),
            esc(&h.author)
        );
    }
    out.push_str("</tbody></table></section>");
}

/// `ChangeHistoryEntry::timestamp` is Unix seconds (spec git_ai_standard_v4).
fn format_change_history_timestamp(secs: u64) -> String {
    let Ok(secs_i64) = i64::try_from(secs) else {
        return secs.to_string();
    };
    match DateTime::<Utc>::from_timestamp(secs_i64, 0) {
        Some(dt) => format_export_datetime_utc(dt),
        None => secs.to_string(),
    }
}

fn render_change_history(out: &mut String, meta: &AuthorshipMetadata) {
    let Some(history) = &meta.change_history else { return };
    if history.is_empty() {
        return;
    }
    out.push_str("<section><h2>Change history</h2>");
    for (i, e) in history.iter().enumerate() {
        render_change_entry(out, i, e);
    }
    out.push_str("</section>");
}

fn render_change_entry(out: &mut String, idx: usize, e: &ChangeHistoryEntry) {
    let _ = writeln!(out, "<details class=\"che\">");
    let _ = writeln!(
        out,
        "<summary>
        <span class=\"badge\">#{}</span> 
        <span class=\"badge kind\">{}</span> 
        <span class=\"sub\">+{} lines, −{} lines</span>
        </summary>",
        idx,
        esc(&e.kind),
        e.line_stats.additions,
        e.line_stats.deletions
    );

    let mut rows: Vec<(&str, String)> = vec![
        ("Index", idx.to_string()),
        ("Kind", esc(&e.kind)),
        ("Timestamp", format_change_history_timestamp(e.timestamp)),
    ];
    if let Some(v) = &e.agent_type {
        rows.push(("Agent", esc(v)));
    }
    if let Some(v) = &e.model {
        rows.push(("Model", esc(v)));
    }
    if let Some(v) = &e.conversation_id {
        rows.push(("AI Conversation ID", html_code(v)));
    }
    if let Some(v) = &e.prompt_id {
        rows.push(("User Message ID", html_code(v)));
    }
    rows.extend([
        ("Total Added Lines", e.line_stats.additions.to_string()),
        ("Total Deleted Lines", e.line_stats.deletions.to_string()),
        ("Total Added SLOC", e.line_stats.additions_sloc.to_string()),
        ("Total Deleted SLOC", e.line_stats.deletions_sloc.to_string()),
    ]);
    if let Some(v) = &e.url {
        rows.push((
            "Change History URL",
            format!("<a href=\"{}\">{}</a>", esc(v), esc(v)),
        ));
    }
    kv_grid(out, &rows);

    if let Some(text) = &e.prompt_text {
        out.push_str("<h3 class=\"sub-h\">User Message</h3>");
        let _ = writeln!(
            out,
            "<div class=\"msg msg-user\"><pre>{}</pre></div>",
            esc(text)
        );
    }
    if !e.files.is_empty() {
        out.push_str("<h3 class=\"sub-h\">Files</h3>");
        out.push_str("<div class=\"che-files\">");
        for (path, detail) in &e.files {
            render_che_file(out, path, detail);
        }
        out.push_str("</div>");
    }
    out.push_str("</details>");
}

fn render_che_file(out: &mut String, path: &str, detail: &FileChangeDetail) {
    let _ = writeln!(
        out,
        "<details class=\"che-file\"><summary><span class=\"file-path\">{}</span></summary>",
        esc(path)
    );

    if !detail.added_line_contents.is_empty() || !detail.deleted_line_contents.is_empty() {
        out.push_str("<table class=\"diff-table\"><tbody>");
        for content in &detail.deleted_line_contents {
            let _ = writeln!(
                out,
                "<tr class=\"del\"><td class=\"sign\">-</td><td class=\"content\">{}</td></tr>",
                esc(content)
            );
        }
        for content in &detail.added_line_contents {
            let _ = writeln!(
                out,
                "<tr class=\"add\"><td class=\"sign\">+</td><td class=\"content\">{}</td></tr>",
                esc(content)
            );
        }
        out.push_str("</tbody></table>");
    }
    out.push_str("</details>");
}

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn wrap_document(spec: &str, body: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>git-ai authorship: {title}</title>
<style>
:root {{ color-scheme: light dark; }}
body {{ font: 14px/1.5 -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif; max-width: 1100px; margin: 2rem auto; padding: 0 1rem; }}
header h1 {{ margin: 0 0 0.25rem; font-size: 1.4rem; }}
.sub {{ color: #666; font-size: 0.9em; margin: 0.25rem 0; }}
.empty {{ color: #888; font-style: italic; }}
code, pre {{ font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }}
pre {{ background: rgba(127,127,127,0.08); padding: 0.5rem 0.75rem; border-radius: 6px; overflow: auto; white-space: pre-wrap; word-break: break-word; }}
details {{ margin: 0.5rem 0; }}
details > summary {{ cursor: pointer; padding: 0.25rem 0; }}
.commit {{ border: 1px solid rgba(127,127,127,0.3); border-radius: 8px; padding: 0.75rem 1rem; margin: 1rem 0; }}
.commit > summary {{ font-weight: 600; }}
.commit .sha {{ font-family: ui-monospace, SFMono-Regular, Menlo, monospace; color: #b58900; margin-right: 0.5rem; }}
section {{ margin-top: 1rem; padding-top: 0.5rem; border-top: 1px solid rgba(127,127,127,0.15); }}
section h2 {{ font-size: 0.85rem; margin: 0.25rem 0 0.5rem; text-transform: uppercase; letter-spacing: 0.06em; color: #555; }}
.sub-h {{ font-size: 0.78rem; margin: 0.75rem 0 0.35rem; text-transform: uppercase; letter-spacing: 0.05em; color: #777; font-weight: 600; }}
dl.kv {{ display: grid; grid-template-columns: max-content 1fr; gap: 0.2rem 1rem; margin: 0.4rem 0 0.6rem; padding: 0.5rem 0.75rem; background: rgba(127,127,127,0.05); border-radius: 6px; font-size: 0.9em; }}
dl.kv dt {{ color: #666; font-weight: 600; }}
dl.kv dd {{ margin: 0; word-break: break-word; }}
table {{ border-collapse: collapse; width: 100%; font-size: 0.9em; margin: 0.4rem 0; }}
th, td {{ text-align: left; padding: 0.35rem 0.6rem; border-bottom: 1px solid rgba(127,127,127,0.2); vertical-align: top; }}
th {{ font-weight: 600; color: #555; background: rgba(127,127,127,0.06); }}
/* Attestations: same column widths for every file’s table (fixed layout + %). */
table.attest {{ table-layout: fixed; }}
table.attest th:nth-child(1), table.attest td:nth-child(1) {{ width: 22%; word-break: break-all; }}
table.attest th:nth-child(2), table.attest td:nth-child(2) {{ width: 38%; }}
table.attest th:nth-child(3), table.attest td:nth-child(3) {{ width: 40%; word-break: break-word; }}
table.attest code {{ word-break: break-word; }}
table.attest th:nth-child(2), table.attest td:nth-child(2) {{ color: #000; }}
.file-path {{ font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }}
.badge {{ display: inline-block; font-size: 0.75em; padding: 0.1rem 0.45rem; border-radius: 999px; background: rgba(127,127,127,0.15); color: #444; font-weight: 600; letter-spacing: 0.03em; }}
.badge.agent {{ background: rgba(37,99,235,0.15); color: #2563eb; }}
.badge.kind {{ background: rgba(168,85,247,0.15); color: #a855f7; text-transform: uppercase; }}
/* Prompt session ID in AI conversations — match body text (not gold/yellow). */
.prompt code.hash {{ color: inherit; }}
.messages {{ list-style: none; padding-left: 0; margin: 0.5rem 0; }}
.msg {{ margin: 0.4rem 0; padding: 0.4rem 0.6rem; border-left: 3px solid transparent; background: rgba(127,127,127,0.05); border-radius: 4px; }}
.msg-user {{ border-color: #16a34a; }}
.msg-assistant {{ border-color: #2563eb; }}
.msg-thinking {{ border-color: #a855f7; }}
.msg-plan {{ border-color: #f59e0b; }}
.msg-tool {{ border-color:rgb(181, 187, 189); }}
.msg pre {{ margin: 0.25rem 0 0; max-height: 28em; }}
.msg-head {{ display: flex; align-items: baseline; gap: 0.5rem; flex-wrap: wrap; }}
.msg-idx {{ color: #888; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 0.8em; }}
.msg-id {{ color: #888; font-size: 0.8em; }}
.msg-id code {{ color: inherit; }}
.messages-block > summary {{ cursor: pointer; }}
.tool-head {{ display: flex; align-items: baseline; gap: 0.4rem; margin: 0.2rem 0 0.3rem; }}
.tool-label {{ font-size: 0.7em; text-transform: uppercase; letter-spacing: 0.06em; color: #6b7280; font-weight: 600; }}
.tool-name {{ color:rgb(106, 109, 116); font-weight: 600; }}
dl.kv.tool-args {{ margin: 0.25rem 0 0; padding: 0.4rem 0.6rem; }}
dl.kv.tool-args dd pre {{ margin: 0; max-height: 24em; }}
dl.kv.tool-args dd pre.json {{ background: rgba(37,99,235,0.06); }}
pre.json {{ background: rgba(37,99,235,0.06); }}
.role {{ text-transform: uppercase; font-size: 0.75em; letter-spacing: 0.06em; color: #555; font-weight: 600; }}
.ts {{ color: #888; font-size: 0.8em; margin-left: 0.5rem; }}
.diff-stats {{ font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }}
.diff .add {{ color: #16a34a; display: block; }}
.diff .del {{ color: #dc2626; display: block; }}
/* GitHub-style unified diff table for change-history files. */
table.diff-table {{ border-collapse: collapse; width: 100%; margin: 0.4rem 0; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 0.85em; border: 1px solid rgba(127,127,127,0.25); border-radius: 6px; overflow: hidden; }}
table.diff-table td {{ padding: 0 0.5rem; border: none; vertical-align: top; white-space: pre-wrap; word-break: break-word; line-height: 1.45; }}
table.diff-table td.ln {{ width: 1%; min-width: 2.5rem; text-align: right; color: #6b7280; background: rgba(127,127,127,0.08); user-select: none; border-right: 1px solid rgba(127,127,127,0.18); font-variant-numeric: tabular-nums; }}
table.diff-table td.sign {{ width: 1%; min-width: 1rem; text-align: center; user-select: none; color: #6b7280; }}
table.diff-table td.content {{ width: auto; }}
table.diff-table tr.add {{ background: rgba(46,160,67,0.15); }}
table.diff-table tr.add td.ln {{ background: rgba(46,160,67,0.25); color: #137333; }}
table.diff-table tr.add td.sign, table.diff-table tr.add td.content {{ color: #0a3d20; }}
table.diff-table tr.del {{ background: rgba(248,81,73,0.15); }}
table.diff-table tr.del td.ln {{ background: rgba(248,81,73,0.25); color: #a40e26; }}
table.diff-table tr.del td.sign, table.diff-table tr.del td.content {{ color: #67060c; }}
@media (prefers-color-scheme: dark) {{
  table.diff-table tr.add {{ background: rgba(46,160,67,0.18); }}
  table.diff-table tr.add td.ln {{ background: rgba(46,160,67,0.3); color: #7ee2a8; }}
  table.diff-table tr.add td.sign, table.diff-table tr.add td.content {{ color: #aff5c4; }}
  table.diff-table tr.del {{ background: rgba(248,81,73,0.18); }}
  table.diff-table tr.del td.ln {{ background: rgba(248,81,73,0.3); color: #ffa198; }}
  table.diff-table tr.del td.sign, table.diff-table tr.del td.content {{ color: #ffd7d5; }}
}}
/* Change-history file list: visually nested under the “Files” subheading. */
.che-files {{ margin: 0.35rem 0 0.6rem; padding-left: 1rem; border-left: 2px solid rgba(127,127,127,0.28); }}
.che-files .che-file {{ margin-left: 0; }}
.kind {{ text-transform: uppercase; font-size: 0.8em; letter-spacing: 0.05em; color: #2563eb; }}
/* Message-type filter bar (sticky so it stays visible while scrolling). */
.msg-filter {{ position: sticky; top: 0; z-index: 10; display: flex; flex-wrap: wrap; gap: 0.4rem 0.9rem; align-items: center; margin: 0.5rem 0 0.75rem; padding: 0.5rem 0.75rem; background: rgba(255,255,255,0.92); backdrop-filter: blur(4px); border: 1px solid rgba(127,127,127,0.25); border-radius: 6px; font-size: 0.9em; }}
.msg-filter-label {{ font-weight: 600; color: #555; }}
.msg-filter label {{ cursor: pointer; user-select: none; display: inline-flex; align-items: center; gap: 0.3rem; }}
@media (prefers-color-scheme: dark) {{ .msg-filter {{ background: rgba(20,20,20,0.85); }} }}
body.hide-user .msg-user {{ display: none; }}
body.hide-assistant .msg-assistant {{ display: none; }}
body.hide-tool .msg-tool {{ display: none; }}
body.hide-thinking .msg-thinking {{ display: none; }}
body.hide-plan .msg-plan {{ display: none; }}
</style>
</head>
<body>
{body}
<script>
(function () {{
  var cbs = document.querySelectorAll('.msg-filter-cb');
  function apply() {{
    cbs.forEach(function (cb) {{
      document.body.classList.toggle('hide-' + cb.dataset.type, !cb.checked);
    }});
  }}
  cbs.forEach(function (cb) {{ cb.addEventListener('change', apply); }});
  apply();
}})();
</script>
</body>
</html>
"#,
        title = esc(spec),
        body = body
    )
}
