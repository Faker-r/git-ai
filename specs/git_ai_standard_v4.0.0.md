# Git AI Standard v4.0.0

This document defines the Git AI Authorship Log format for tracking AI-generated code contributions within Git repositories.

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT", "SHOULD", "SHOULD NOT", "RECOMMENDED", "MAY", and "OPTIONAL" in this document are to be interpreted as described in [RFC 2119](https://datatracker.ietf.org/doc/html/rfc2119).

---

The [Git AI project](https://github.com/git-ai-project/git-ai) is a full, production-ready implementation of this standard built as a Git extension. Another project would be considered compliant with this standard if it also attached AI Authorship Logs with Git Notes, even if it was implemented in another way.

If you are trying to add support for your Coding Agent to Git AI format, that is best done by [integrating with published implementation](https://usegitai.com/docs/cli/add-your-agent), not implementing this spec.

## Changes from v3.0.0

v4.0.0 is a research-oriented extension of the authorship log format. The primary goals are:

1. **Full change history**: Record every checkpoint (both human and AI) that occurred between commits, with line-level detail and the prompt text that triggered each change.
2. **Human change tracking**: Capture human edits alongside AI edits, not just AI-attributed content.
3. **Context conversations**: Record planning, research, and Q&A conversations that influenced development but did not directly produce code changes.
4. **Line history command**: Enable tracing any line back through its edit history via the new `line-history` command.
5. **Subagent tracking**: Link parent conversations to their spawned subagent conversations.
6. **Message IDs**: Add IDs to transcript messages for finer prompt traceability

### Backward Compatibility

- v4.0.0 implementations MUST accept authorship logs with any `schema_version` starting with `"authorship/"` (i.e., both v3.x and v4.x notes).
- v3.0.0 implementations cannot read v4.0.0 notes due to new required fields. This is a forward-incompatible change.
- The attestation section wire format is unchanged from v3.0.0. However, v4.0.0 formally specifies `h_`-prefixed known-human hashes, which were implemented but undocumented in v3.

### Errata carried from v3.0.0

E-001: The field name `overriden_lines` (single 'd') remains in v4.0.0 for compatibility with the reference implementation. It will be renamed to `overridden_lines` in a future version.

---

## 1. Authorship Logs

Authorship logs provide a record of which lines in a commit were authored by AI agents, along with the conversation threads that generated them. The line numbers are only accurate in the context of that commit, with the version of each file at the time of committing.

### 1.1 Attaching Git Notes to Commits

Git AI uses [Git Notes](https://git-scm.com/docs/git-notes) to attach authorship metadata to commits without modifying commit history.

#### Notes Reference

- Authorship logs MUST be stored under the `refs/notes/ai` namespace
- Implementations MUST NOT use the default `refs/notes/commits` namespace to avoid conflicts with other tools
- Each commit SHA MAY have at most one authorship log attached

### 1.2 Log Format

The Authorship Log MUST consist of two sections separated by a divider line containing exactly `---`:

1. **Attestation Section** — Line-level attribution mapping
2. **Metadata Section** — JSON object containing prompt records, change history, and versioning

#### 1.2.1 Schema Version

The schema version for this specification is:

```
authorship/4.0.0
```

Implementations MUST include this version string in the `schema_version` field of the metadata section.

Implementations MUST accept any `schema_version` that starts with `"authorship/"` when reading notes. This allows forward- and backward-reading of v3.x and v4.x notes.

#### 1.2.2 Overall Structure

```
<attestation-section>
---
<metadata-section>
```

The divider `---` MUST appear on its own line with no leading or trailing whitespace. This allows a buffer to quickly read just the Attestation Section without loading the metadata (for very fast `git-ai blame` operations).

---

### 1.2.3 Attestation Section

The attestation section maps files to the AI sessions or known-human authors responsible for specific lines. Each entry associates a hash with line ranges. Hashes route to either `metadata.prompts` (AI sessions) or `metadata.humans` (known-human authors). Lines with no attestation entry are untracked.

#### File Path Lines

- File paths MUST appear at the start of a line (no leading whitespace)

```
src/main.rs
```

- File paths containing spaces, tabs, or newlines MUST be wrapped in double quotes (`"`)

```
"src/my file.rs"
```

- File paths SHOULD NOT contain the quote character (`"`)
- Files with no attestation entries MUST NOT be included in the Attestation Section

#### Attestation Entry Lines

Each attestation entry MUST be indented with exactly two spaces and contain:
1. A **hash** pointing to either an AI session or a known human author, both 16 characters
2. A single space
3. A **line range specification**

```
  d9978a8723e02b52 1-4,9-10,12,14,16
```

Attestation entries MUST be sorted by their **hash**

#### Line Range Specification

Line ranges MUST use one of the following formats:

| Format | Description | Example |
|--------|-------------|---------|
| Single line | A single line number | `42` |
| Range | Inclusive start and end, hyphen-separated | `19-222` |
| Multiple | Comma-separated combination of singles and ranges | `1,2,19-222,300` |

Line numbers MUST be:
- 1-indexed (first line is `1`, not `0`)
- Positive integers
- Sorted in ascending order within each entry

Line ranges:
- MUST NOT contain spaces
- SHOULD be sorted by their start position
- SHOULD use ranges for consecutive lines (e.g., `1-5` instead of `1,2,3,4,5`)

#### Attestation Section Example

```
src/main.rs
  abcd1234abcd1234 1-10,15-20
src/lib.rs
  abcd1234abcd1234 1-50
```

The above example can be read as:

In `src/main.rs`, the prompt `abcd1234abcd1234` generated the lines above

#### Hash Semantics

Hashes in the attestation section identify who authored specific lines. There are two kinds:

**AI session hashes** reference a key in the `prompts` object of the metadata section:
- MUST be 16 characters: lowercase hexadecimal only
- MUST be generated using SHA-256 of `{tool}:{conversation_id}`, taking the first 16 hex characters, lowercase. Ie `cursor:${conversation_id}` or `claude:${conversation_id}` or `amp:${thread_id}`.
- SHOULD remain stable for the same AI session across commits

**Human author hashes** reference a key in the `humans` object of the metadata section:
- MUST be 16 characters: the prefix `h_` followed by 14 lowercase hexadecimal characters
- MUST be generated using SHA-256 of the git committer identity string (e.g., `"Alice Smith <alice@example.com>"`), taking the first 14 hex characters and prepending `h_`

Implementations MUST route hashes starting with `h_` to the `humans` map and all other hashes to the `prompts` map.

**Hash Length:**
- New implementations MUST generate 16-character hashes
- Implementations SHOULD accept 7-character hashes for backward compatibility with earlier versions

---

### 1.2.4 Metadata Section

The metadata section MUST be a valid JSON object containing the following fields:

#### Required Fields

| Field | Type | Description |
|-------|------|-------------|
| `schema_version` | string | MUST be `"authorship/4.0.0"` |
| `base_commit_sha` | string | The commit SHA this authorship log was computed against |
| `prompts` | object | Map of session hashes to prompt records |

#### Optional Fields

| Field | Type | Description |
|-------|------|-------------|
| `git_ai_version` | string | Version of the git-ai tool that generated this log |
| `humans` | object | Map of human-author hashes to human records. Omitted when empty. |
| `change_history` | array | **NEW in v4.0.0.** Ordered list of `ChangeHistoryEntry` objects recording every checkpoint (human and AI) that occurred between the parent commit and this commit. See [Section 1.2.7](#127-change-history). |

---

### 1.2.5 Prompt Record Object

Each entry in the `prompts` object MUST contain:

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `agent_id` | object | REQUIRED | Identifies the AI agent |
| `human_author` | string | OPTIONAL | The human who prompted the AI (e.g., `"Name <email>"`) |
| `messages` | array | REQUIRED | The conversation transcript |
| `total_additions` | integer | REQUIRED | Total lines added by this session |
| `total_deletions` | integer | REQUIRED | Total lines deleted by this session |
| `accepted_lines` | integer | REQUIRED | Lines accepted in the final commit |
| `overriden_lines` | integer | REQUIRED | Lines that were later modified by human (see Errata E-001) |
| `messages_url` | string | OPTIONAL | URL to CAS-stored messages (format: `{api_base_url}/cas/{hash}`) |
| `custom_attributes` | object | OPTIONAL | Map of string key-value pairs for agent-specific metadata |
| `subagents` | array | OPTIONAL | **NEW in v4.0.0.** Array of conversation ID strings for subagent sessions spawned by this conversation. |

#### Context Conversations

**NEW in v4.0.0.** Prompt records MAY represent "context conversations" — planning, research, or Q&A sessions that occurred between commits but produced no code changes. These are identified by having `total_additions`, `total_deletions`, `accepted_lines`, and `overriden_lines` all set to `0`.

Context conversations:
- MUST be fetched iff the agent workspace (ex. IDE workspace, directory where Claude Code is initiated, ...) matches the git repository of the commit 
- MUST be included to provide a complete picture of the development process, even though they did not directly produce code
- MUST have been last updated between the parent commit and the current commit (conversations whose most recent activity falls outside this window MUST be excluded)

#### Agent ID Object

| Field | Type | Description |
|-------|------|-------------|
| `tool` | string | The AI tool/IDE (e.g., `"cursor"`, `"claude"`, `"copilot"`) |
| `id` | string | Unique session identifier (typically a UUID) |
| `model` | string | The AI model used (e.g., `"claude-4.5-opus-high-thinking"`) |

#### Message Object

Each message in the `messages` array MUST contain:

| Field | Type | Description |
|-------|------|-------------|
| `type` | string | One of: `"user"`, `"assistant"`, `"thinking"`, `"plan"`, `"tool_use"` |
| `text` | string | The message content (for `user`, `assistant`, `thinking`, and `plan` types) |
| `timestamp` | string | ISO 8601 timestamp (OPTIONAL) |
| `id` | string | **NEW in v4.0.0.** OPTIONAL. Unique message identifier from the AI tool/IDE (e.g., Cursor `bubble_id`). Used for message granularity for prompt to code traceability.
| `name` | string | Tool name (for `tool_use` type only) |
| `input` | object | Tool input parameters (for `tool_use` type only) |


#### Message Array Requirements

The `messages` array:
- MUST contain all human prompts (`type: "user"`)
- MUST contain all assistant responses (`type: "assistant"`)
- MUST contain all tool calls made by the assistant (`type: "tool_use"`)
- MUST contain thinking messages (`type: "thinking"`)
- MUST contain plan messages (`type: "plan"`)
- MUST NOT contain tool responses (the results returned from tool executions)

Tool responses are excluded because they often contain large amounts of file content, command output, or other verbose data that would bloat the authorship log without adding meaningful attribution context.

---

### 1.2.6 Human Record Object

Each entry in the `humans` object MUST contain:

| Field | Type | Description |
|-------|------|-------------|
| `author` | string | Git committer identity in the standard `"Name <email>"` format |

---

### 1.2.7 Checkpoint Behavior

A checkpoint is a snapshot of file contents and the diff from the previous checkpoint, attributed to either a human or AI author. Checkpoints record:

- File contents
- Diffs since the previous checkpoint
- The author (human or AI)
- Timestamp and metadata (model, prompt text, etc.)

A subset of Checkpoint data is persisted into the `change_history` field in the metadata section. You can see the exact data being persisted in the `ChangeHistoryEntry` schema.

#### Authorship

Checkpoints MUST correctly attribute their changes to the author (human or AI) that produced them.

For the reference git-ai implementation, checkpoints are taken before and after each fileEdit tool call. See [How Git AI Works](https://usegitai.com/docs/cli/how-git-ai-works#part-1-what-happens-on-a-developers-machine) for a visualization.

#### Prompt Association

User messages and checkpoints have a 1-to-N relationship, where N can be `0` for non-code prompts. A single user message MAY trigger multiple events, each producing its own checkpoint. AI checkpoints MUST record the prompt that triggered them.

#### Tracked Files

A checkpoint MUST diff file contents for all tracked files. Tracked files are:

- All files reported by `git status`
- All files included in a previous checkpoint (ensures deleted files are captured)

Files matching the following SHOULD be excluded:
- Well-known generated paths (e.g., `node_modules`, `*.lock`)
- Files included in a `.git-ai-ignore` file at the repository root
For the git-ai reference implementation, see ignored patterns in git-ai/src/authorship/ignore.rs



---

### 1.2.8 Change History

**NEW in v4.0.0.** The `change_history` field in the metadata section records a chronological sequence of every checkpoint that occurred between the parent commit and this commit. This is the primary research data structure — it captures both human and AI changes at line-level granularity.

Note that it records all changes between the timestamps of the parent commit and the current commit, regardless of whether those changes were committed. 

#### ChangeHistoryEntry Object

Each entry in the `change_history` array MUST contain:

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `timestamp` | integer | REQUIRED | Unix timestamp (seconds since epoch) when the checkpoint was created |
| `kind` | string | REQUIRED | One of: `"human"`, `"ai_agent"`, `"ai_tab"`, `"known_human"` |
| `conversation_id` | string | OPTIONAL | SHA-256 hash of the agent ID, linking to the `prompts` map. Present for AI checkpoints, absent for human checkpoints. |
| `agent_type` | string | OPTIONAL | The AI tool that produced this checkpoint (e.g., `"cursor"`, `"claude_code"`). Present for AI checkpoints. |
| `prompt_id` | string | OPTIONAL | IDE-specific message identifier (e.g., Cursor `bubble_id`) linking to the user message that triggered this checkpoint. |
| `model` | string | OPTIONAL | The AI model used for this checkpoint (e.g., `"claude-4.5-opus"`). |
| `prompt_text` | string | OPTIONAL | The actual text of the user prompt that triggered this checkpoint. Resolved by looking up the user message in the transcript matching `prompt_id`, or falling back to the last user message before the checkpoint timestamp. |
| `files` | object | REQUIRED | Map of file paths to `FileChangeDetail` objects describing what changed in each file. |
| `line_stats` | object | REQUIRED | Aggregate `CheckpointLineStats` for this checkpoint. |

#### FileChangeDetail Object

Each value in the `files` map MUST contain:

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `added_lines` | array | REQUIRED | Array of strings in compact range format (`"n"` for single lines, `"n-m"` for ranges). Represents line ranges added in new-content coordinates (1-based, inclusive). |
| `deleted_lines` | array | REQUIRED | Array of strings in compact range format. Represents line ranges deleted in old-content coordinates (1-based, inclusive). |
| `added_line_contents` | array | OPTIONAL | Array of strings in `"N: <text>"` format showing the actual content of each added line, where `N` is the 1-based line number in new-content coordinates. MAY be empty or omitted. |
| `deleted_line_contents` | array | OPTIONAL | Array of strings in `"N: <text>"` format showing the actual content of each deleted line, where `N` is the 1-based line number in old-content coordinates. MAY be empty or omitted. |

Contiguous single-line changes SHOULD be merged into ranges to avoid O(n) explosion (e.g., `"5-8"` instead of `"5"`, `"6"`, `"7"`, `"8"`).

#### CheckpointLineStats Object

| Field | Type | Description |
|-------|------|-------------|
| `additions` | integer | Total lines added at this checkpoint |
| `deletions` | integer | Total lines deleted at this checkpoint |
| `additions_sloc` | integer | Source lines of code added (excluding blank lines and comments) |
| `deletions_sloc` | integer | Source lines of code deleted (excluding blank lines and comments) |

All fields default to `0` if absent.

---

### 1.2.9 Complete Example

```
src/main.rs
  abcd1234abcd1234 1-10,15-20
src/lib.rs
  abcd1234abcd1234 1-50
---
{
  "schema_version": "authorship/4.0.0",
  "git_ai_version": "1.3.0",
  "base_commit_sha": "7734793b756b3921c88db5375a8c156e9532447b",
  "prompts": {
    "abcd1234abcd1234": {
      "agent_id": {
        "tool": "cursor",
        "id": "6ef2299e-a67f-432b-aa80-3d2fb4d28999",
        "model": "claude-4.5-opus"
      },
      "human_author": "Developer <dev@example.com>",
      "messages": [
        {
          "type": "user",
          "text": "Add error handling to the main function",
          "timestamp": "2025-12-05T01:22:13.211Z",
          "id": "bubble-abc-123"
        },
        {
          "type": "assistant",
          "text": "I'll add comprehensive error handling...",
          "timestamp": "2025-12-05T01:22:38.724Z",
          "id": "bubble-abc-124"
        }
      ],
      "total_additions": 66,
      "total_deletions": 5,
      "accepted_lines": 66,
      "overriden_lines": 0,
      "subagents": ["sub-conv-001", "sub-conv-002"]
    },
    "1234abcd5678ef01": {
      "agent_id": {
        "tool": "cursor",
        "id": "planning-session-uuid",
        "model": "claude-4.5-opus"
      },
      "human_author": "Developer <dev@example.com>",
      "messages": [
        {
          "type": "user",
          "text": "What's the best approach for adding error handling here?",
          "timestamp": "2025-12-05T01:00:00.000Z"
        },
        {
          "type": "assistant",
          "text": "I'd recommend using the Result type with a custom error enum...",
          "timestamp": "2025-12-05T01:00:15.000Z"
        }
      ],
      "total_additions": 0,
      "total_deletions": 0,
      "accepted_lines": 0,
      "overriden_lines": 0
    }
  },
  "change_history": [
    {
      "timestamp": 1733362933,
      "kind": "ai_agent",
      "conversation_id": "abcd1234abcd1234",
      "agent_type": "cursor",
      "prompt_id": "bubble-abc-123",
      "model": "claude-4.5-opus",
      "prompt_text": "Add error handling to the main function",
      "files": {
        "src/main.rs": {
          "added_lines": ["1-10", "15-20"],
          "deleted_lines": ["1-5"],
          "added_line_contents": [
            "1: use std::process;",
            "2: use anyhow::Result;",
            "3: ",
            "4: fn main() -> Result<()> {",
            ...
          ],
          "deleted_line_contents": [
            "1: fn main() {",
            "2:     run();",
            "3: }",
            ...
          ]
        },
        "src/lib.rs": {
          "added_lines": ["1-50"],
          "deleted_lines": [],
          "added_line_contents": [
            "1: use std::process;",
            "2: use anyhow::Result;",
            ...
          ],
        }
      },
      "line_stats": {
        "additions": 66,
        "deletions": 5,
        "additions_sloc": 55,
        "deletions_sloc": 4
      }
    },
    {
      "timestamp": 1733363000,
      "kind": "human",
      "files": {
        "src/main.rs": {
          "added_lines": ["25", "30-35"],
          "deleted_lines": [],
          "added_line_contents": [
            ...
          ],
        }
      },
      "line_stats": {
        "additions": 7,
        "deletions": 0,
        "additions_sloc": 6,
        "deletions_sloc": 0
      }
    },
  ]
}
```

In the above example:
- Two prompt sessions are recorded: one that produced code changes and one context conversation (planning session) with zero code stats.
- The `change_history` shows two checkpoints in chronological order: an AI edit, then a human edit.
- The context conversation (`1234abcd5678ef01`) has zero stats but preserves the planning discussion.
- The first prompt record includes `subagents` linking to two subagent conversations.
- Messages include the `id` field for finer granularity in change_history attribution. 

---

## 2. Line History Command

**NEW in v4.0.0.** The `line-history` command traces the authorship history of a specific line through git history using the `change_history` data.

### 2.1 Usage

```
git-ai line-history <file> <line> [--commit <sha>]
```

- `<file>`: Path to the file (relative to repository root)
- `<line>`: 1-based line number to trace
- `--commit <sha>`: Optional commit to start from (defaults to `HEAD`)

### 2.2 Algorithm

1. Read the line content at the specified commit.
2. Use `git log -L<line>,<line>:<file>` to find all commits that touched that line.
3. For each commit, read the authorship note and its `change_history`.
4. Walk backward through each checkpoint's added/deleted ranges to determine if/when the line was introduced or modified.

#### Line Mapping

The line mapping algorithm reconstructs diff operations from the `change_history` entries:

- **`reconstruct_diff_ops()`**: Reconstructs interleaved Equal/Delete/Insert operations from separate added and deleted range lists.
- **`map_new_to_old()`**: Maps a new-file line number to its old-file position; returns `None` if the line was an insertion.
- **`reverse_through_insert()`**: When a line falls in an Insert range, finds the old-file position of what was replaced (handles delete-then-insert replacement patterns).
- **`find_checkpoints_that_touched_line()`**: Walks checkpoints in reverse order, tracking the line position through each diff.

### 2.3 Output

The command outputs a JSON object:

```json
{
  "file": "src/main.rs",
  "line": 15,
  "at_commit": "HEAD",
  "line_content": "    let result = match parse_args() {",
  "history": [
    {
      "commit_sha": "abc123",
      "commit_date": "2026-03-27T10:11:49-07:00",
      "commit_message": "Add error handling",
      "target_line": 15,
      "checkpoints": [
        {
          "timestamp": 1733362933,
          "kind": "ai_agent",
          "agent_type": "cursor",
          "model": "claude-4.6-sonnet-medium-thinking",
          "prompt_id": "54e7cc1b-e560-4690-800c-20abcfb028a2",
          "prompt_text": "Add error handling to the main function",
          "line_content": "15:     let result = match parse_args() {",
          "additions": 65,
          "deletions": 5
        },
        {                                                                                                                        
          "timestamp": 1774631490,                                                                                               
          "kind": "human",                                                                                                       
          "agent_type": null,                                                                                                    
          "model": null,                                                                                                         
          "prompt_id": null,                                                                                                     
          "prompt_text": null,  
          "line_content": "15:     let curr_result = match parse_args() {",                                                                                    
          "additions": 1,                                                                                                        
          "deletions": 1                                                                                                         
        },  
      ]
    }
  ]
}
```

#### Output Fields

**LineHistoryOutput:**

| Field | Type | Description |
|-------|------|-------------|
| `file` | string | The file path queried |
| `line` | integer | The line number queried |
| `at_commit` | string | The commit SHA the query was resolved against |
| `line_content` | string | The content of the line at the queried commit |
| `history` | array | Array of `CommitHistoryEntry` objects |

**CommitHistoryEntry:**

| Field | Type | Description |
|-------|------|-------------|
| `commit_sha` | string | The commit SHA |
| `commit_date` | string | The commit date |
| `commit_message` | string | The commit subject line |
| `target_line` | integer | The line number as it existed at this commit |
| `checkpoints` | array | Array of `MatchedCheckpoint` objects that touched this line |

**MatchedCheckpoint:**

| Field | Type | Description |
|-------|------|-------------|
| `timestamp` | integer | Unix timestamp of the checkpoint |
| `kind` | string | One of: `"human"`, `"ai_agent"`, `"ai_tab"`, `"known_human"` |
| `agent_type` | string | OPTIONAL. The AI tool identifier |
| `model` | string | OPTIONAL. The AI model used |
| `prompt_id` | string | OPTIONAL. The IDE-specific message identifier |
| `prompt_text` | string | OPTIONAL. The user prompt text |
| `line_content` | string | OPTIONAL. The content of the tracked line after this checkpoint |
| `additions` | integer | Total lines added at this checkpoint |
| `deletions` | integer | Total lines deleted at this checkpoint |

---

## 3. History Rewriting Behaviors 

Authorship Logs can be attached to one, and only one commit SHA. When users do Git operations like `rebase`, `cherry-pick`, `reset`, `merge`, `stash`/`pop`, that rewrite the worktree and history, corresponding changes to Authorship Logs are required. 

> **NOTE: NOT YET IMPLEMENTED for Change History.** History-rewriting code (`rebase_authorship.rs`) does not currently handle `change_history`. During rebase, cherry-pick, or squash operations, `change_history` will be silently lost.

### 3.1 Rebase

A rebase takes a range of commits and rewrites history, creating new commits with different SHAs. Implementations MUST preserve AI authorship attribution through all rebase scenarios.

#### Core Principles

1. **SHA Independence**: Authorship is attached to commit SHAs. When a commit's SHA changes, the authorship log MUST be copied to the new commit
2. **Content-Based Attribution**: Line attributions MUST reflect the actual content at each commit, not the original commit's state
3. **Prompt Preservation**: All prompt records from original commits MUST be preserved in the corresponding new commits

#### Standard Rebase (1:1 Mapping)

When commits are rebased without modification (e.g., `git rebase main`):

- For each original commit → new commit mapping, implementations MUST copy the authorship log
- The `base_commit_sha` field SHOULD be updated to reflect the new parent commit
- Line numbers in attestations remain valid because file content is unchanged

```
Original: A → B → C → D (feature)
                ↑
              main

After rebase onto main':
main' → B' → C' → D'

Authorship mapping:
  B → B' (copy authorship log)
  C → C' (copy authorship log)  
  D → D' (copy authorship log)
```

#### Interactive Rebase: Commit Reordering

When commits are reordered (e.g., `pick C` before `pick B`):

- Each new commit MUST have authorship reflecting its actual content at that point in history
- Line numbers MUST be recalculated based on the file state at each new commit
- Implementations MUST track content through the reordered sequence and adjust attributions accordingly

```
Original order: B → C → D
Reordered:      C' → B' → D'

For C' (now first):
  - Attributions based on C's changes applied to main'
  
For B' (now second):
  - Attributions based on B's changes applied after C'
  - Line numbers adjusted for C's prior changes
```

#### Interactive Rebase: Squash/Fixup (N → 1)

When multiple commits are squashed into one:

- The resulting commit's authorship log MUST contain prompt records from ALL squashed commits
- Line attributions MUST be calculated against the final file state
- Session hashes from all contributing commits MUST be preserved
- If the same lines were modified by different sessions, the LAST session's attribution wins

```
Squashing B, C, D into single commit S:

S's authorship log contains:
  - All prompts from B, C, D
  - Line attributions reflecting final state after all changes
  - Multiple session hashes if different AI sessions contributed
```

#### Interactive Rebase: Splitting Commits (1 → N)

When a single commit is split into multiple commits:

- The original commit's authorship data MUST be distributed across the new commits
- Each new commit MUST only contain attributions for lines present in THAT commit's diff
- Prompt records MAY be duplicated across commits if the same session contributed to multiple splits
- When content from the original commit reappears in a later split, implementations MUST restore its original attribution

```
Splitting D into D1, D2, D3:

D1's authorship: lines 1-10 from D's original authorship
D2's authorship: lines 11-20 from D's original authorship
D3's authorship: lines 21-30 from D's original authorship
```

#### Interactive Rebase: Dropping Commits

When commits are dropped (removed from the rebase):

- Authorship logs for dropped commits MUST NOT be attached to any new commits
- If dropped content reappears in later commits (via conflict resolution or manual edits), it SHOULD be attributed to the human author, not the original AI session
- Implementations MUST NOT create authorship notes for commits that no longer exist

#### Interactive Rebase: Editing Commits

When a commit is edited during interactive rebase (`edit`):

- If the edit modifies AI-attributed lines, those lines SHOULD be re-attributed to the human
- If the edit adds new content, that content follows normal attribution rules
- The original session's prompt record MUST be preserved (for audit trail)
- The `overriden_lines` counter SHOULD be incremented for lines modified by the human

#### Amending During Rebase

When `git commit --amend` is used during a rebase:

- The amended commit's authorship MUST reflect the combined changes
- If the amend includes new AI-generated content, that session MUST be added to prompts
- If the amend removes AI-generated lines, those lines MUST be removed from attestations
- The `base_commit_sha` MUST reference the amended commit's parent

#### Conflict Resolution

When conflicts occur during rebase:

- Implementations MUST wait until the conflict is resolved and the rebase continues
- Conflict resolution changes made by humans SHOULD NOT be attributed to AI
- If an AI assists with conflict resolution, that SHOULD be tracked as a new session
- Lines where conflict markers were present and manually resolved SHOULD be attributed to the human resolver

#### Abort and Failure Handling

When a rebase is aborted (`git rebase --abort`):

- Implementations MUST NOT create any new authorship notes
- The original commits retain their original authorship logs (unchanged)
- Any partial authorship state MUST be discarded

When a rebase fails mid-operation:

- Implementations SHOULD log the failure for debugging
- No authorship notes SHOULD be written for incomplete rebases
- Recovery is handled when the user either continues or aborts

#### Edge Cases

**Empty Commits**: If a rebase results in empty commits (no changes), those commits:
- MAY have empty authorship logs (no attestations)
- SHOULD still have the metadata section with `base_commit_sha`

**No AI Content**: If rebased commits contain no AI-attributed content:
- Implementations MAY skip authorship processing entirely
- No authorship notes are required for purely human-authored commits

**Commits Already Have Notes**: When processing new commits, if a commit already has an authorship log (from the target branch):
- Implementations MUST skip that commit
- Only newly created commits from the rebase need processing

**Merge Commits in Rebase**: If a rebase includes merge commits:
- The merge commit's authorship reflects the resolution, not the merged content
- Implementations SHOULD handle these as special cases with potentially empty attestations

---

### 3.2 Merge

A merge combines changes from one branch into another. Implementations MUST preserve AI authorship attribution through all merge scenarios.

#### Core Principles

1. **Working State Preservation**: For merge operations that leave changes uncommitted (e.g., `merge --squash`), AI attributions MUST be moved from committed authorship logs to the implementation's working state so they appear in Authorship Logs after the next commit
1. **Prompt Preservation**: All prompt records from merged commits MUST be preserved

#### Standard Merge

When a merge creates a merge commit:

- The merged commits retain their authorship logs in history (no action needed)
- The merge commit's authorship log MUST only contain attributions for conflict resolution changes
- If conflicts were resolved with AI assistance, that MUST be tracked as a new session
- If conflicts were resolved manually, those changes SHOULD be attributed to the human resolver
- If no conflicts occurred, the merge commit MAY have an empty authorship log (no attestations)
- The `base_commit_sha` field MUST reference the merge commit itself

#### Merge --squash

When `git merge --squash` is used, the merge leaves changes staged but uncommitted:

- **AI attributions MUST be moved from committed authorship logs to the implementation's working state**
- When the user commits, all accurate AI attributions from the source branch will appear in the new commit's authorship log. 
- Prompt records from all squashed commits MUST be preserved

```
Before merge --squash:
  main: A → B → C
  feature: D → E → F (with AI attributions)

After merge --squash (before commit):
  - Changes from D, E, F are staged
  - AI attributions from D, E, F are in working state (INITIAL)
  
After commit:
  - New commit G contains all changes
  - G's authorship log contains attributions from D, E, F
```

#### Conflict Resolution

When conflicts occur during merge:

- Implementations MUST wait until the conflict is resolved and the merge completes
- If an AI assists with conflict resolution, that SHOULD be tracked as a new session
- Lines where conflict markers were present and manually resolved SHOULD be attributed to the human resolver

---

### 3.3 Reset

A reset moves HEAD to a different commit, potentially discarding commits. Implementations MUST preserve AI authorship attribution by moving it to working state when commits are unwound.

#### Core Principles

1. **Working State Migration**: AI attributions from "unwound" commits MUST be moved from committed authorship logs to the implementation's working state

#### Reset --soft

When `git reset --soft` is used:

- HEAD moves to the target commit, but the index and working directory remain unchanged
- **AI attributions from unwound commits MUST be moved to the implementation's working state**
- When the user commits, these attributions will appear in the new commit's authorship log

#### Reset --mixed (Default)

When `git reset --mixed` (or `git reset`) is used:

- HEAD and the index move to the target commit, but the working directory remains unchanged
- **AI attributions from unwound commits MUST be moved to the implementation's working state**
- When the user commits, these attributions will appear in the new commit's authorship log

#### Reset --hard

When `git reset --hard` is used:

- HEAD, index, and working directory all move to the target commit
- AI Attributions in your implementation's working state MUST be cleared 
- AI Authorship Notes SHOULD NOT be deleted. 

#### Partial Reset

When reset is used with pathspecs (e.g., `git reset HEAD -- file.txt`):

- Only specified files are reset
- **AI attributions for reset files MUST be moved from committed authorship logs to the implementation's working state**
- Other files' attributions remain unchanged
- The working log MUST be updated accordingly

```
Before reset --soft:
  HEAD: A → B → C (with AI attributions in C)
  
After reset --soft to A:
  HEAD: A
  Index: Contains changes from B and C
  Working log: Contains INITIAL attributions from B and C
  
After commit:
  New commit D contains changes from B and C
  D's authorship log contains attributions from B and C
```

---

### 3.4 Cherry-pick

A cherry-pick applies changes from one or more commits to the current branch. Implementations MUST preserve AI authorship attribution through cherry-pick operations.

#### Core Principles

1. **SHA Independence**: When a commit is cherry-picked, it gets a new SHA. The authorship log MUST be copied to the new commit
2. **Content-Based Attribution**: Line attributions MUST reflect the actual content at the new commit location
3. **Working State for Uncommitted**: When cherry-pick is used with `--no-commit`, AI attributions MUST be moved to working state

#### Standard Cherry-pick (With Commit)

When `git cherry-pick` creates a new commit:

- The new commit's authorship log MUST contain attributions from the source commit
- Line numbers MUST be recalculated based on the file state at the new commit location
- Prompt records from the source commit MUST be preserved
- The `base_commit_sha` field MUST reference the new commit

#### Cherry-pick --no-commit

When `git cherry-pick --no-commit` is used:

- Changes are applied to the working directory and index but not committed
- **AI attributions from the source commit(s) MUST be moved from committed authorship logs to the implementation's working state**
- When the user commits, these attributions will appear in the new commit's authorship log

```
Before cherry-pick --no-commit:
  Current branch: A → B
  Source commit: C (with AI attributions)
  
After cherry-pick --no-commit:
  Changes from C are staged
  Working log: Contains INITIAL attributions from C
  
After commit:
  New commit D contains changes from C
  D's authorship log contains attributions from C
```

#### Multiple Cherry-picks

When multiple commits are cherry-picked:

- Each new commit MUST have its own authorship log
- Attributions MUST be calculated based on the sequential application of changes
- Prompt records from all source commits MUST be preserved

#### Conflict Resolution

When conflicts occur during cherry-pick:

- Implementations MUST wait until the conflict is resolved and the cherry-pick continues
- Conflict resolution changes made by humans SHOULD NOT be attributed to AI
- If an AI assists with conflict resolution, that SHOULD be tracked as a new session
- Lines where conflict markers were present and manually resolved SHOULD be attributed to the human resolver

---

### 3.5 Stash / Pop

Stash operations temporarily save working directory changes. Implementations MUST preserve AI authorship attribution through stash and pop operations.

#### Core Principles

1. **Working State Preservation**: When stashing, AI attributions from the working log MUST be saved with the stash
2. **Attribution Restoration**: When popping/applying a stash, AI attributions MUST be restored to the working state
3. **Working State Migration**: **AI attributions MUST be moved from committed authorship logs (if any) to the implementation's working state when stashing, and restored to working state when popping**

#### Stash Push / Save

When `git stash` (or `git stash push` / `git stash save`) is used:

- The current working log's AI attributions MUST be saved as an authorship log in git notes (under `refs/notes/ai-stash`)
- The authorship log MUST be associated with the stash commit SHA
- The working log entries for stashed files MUST be removed from the current working state
- If pathspecs are specified, only attributions for matching files are saved

#### Stash Pop

When `git stash pop` is used:

- The stash's authorship log MUST be read from git notes (`refs/notes/ai-stash`)
- **AI attributions from the stash MUST be moved to the implementation's working state**
- The working log MUST be updated with these attributions
- When the user commits, these attributions will appear in the new commit's authorship log
- The stash's authorship log note MAY be deleted after successful pop

#### Stash Apply

When `git stash apply` is used:

- The stash's authorship log MUST be read from git notes (`refs/notes/ai-stash`)
- **AI attributions from the stash MUST be moved to the implementation's working state **
- The working log MUST be updated with these attributions
- When the user commits, these attributions will appear in the new commit's authorship log
- The stash's authorship log note is preserved (unlike pop)

#### Stash with Pathspecs

When stashing specific files (e.g., `git stash push -- file.txt`):

- Only attributions for the specified files are saved
- Only those files' working log entries are removed
- When popping/applying, only those files' attributions are restored

```
Before stash:
  Working log: Contains INITIAL attributions for file1.txt and file2.txt
  
After stash:
  Stash commit created with SHA abc123
  Git note at refs/notes/ai-stash/abc123 contains authorship log
  Working log: Empty (files were stashed)
  
After stash pop:
  Changes from stash are applied
  Working log: Contains INITIAL attributions from stash
  Git note may be deleted
  
After commit:
  New commit contains changes from stash
  Commit's authorship log contains attributions from stash
```

---

### 3.6 Amend

An amend modifies the most recent commit, creating a new commit with a different SHA. Implementations MUST preserve AI authorship attribution through amend operations.

#### Core Principles

1. **SHA Independence**: When a commit is amended, it gets a new SHA. The authorship log MUST be moved to the new commit
2. **Working State Integration**: AI attributions from the original commit's authorship log and any uncommitted working state MUST be combined
3. **Content-Based Attribution**: Line attributions MUST reflect the actual content at the amended commit

#### Standard Amend

When `git commit --amend` is used:

- The original commit's authorship log MUST be read
- Any uncommitted AI attributions from the working log MUST be included
- The new commit's authorship log MUST reflect the combined state
- The `base_commit_sha` field MUST reference the amended commit (which is the new commit SHA)
- The original commit's authorship log note SHOULD be removed (since the commit no longer exists)

#### Amend with New AI Content

When amend includes new AI-generated content:

- The new AI session MUST be added to the prompts
- Attributions for new content MUST be added to the attestations
- Existing attributions MUST be preserved unless lines were modified

#### Amend Removing AI Content

When amend removes AI-generated lines:

- Those lines MUST be removed from attestations
- Prompt records SHOULD be preserved (for audit trail)
- The `accepted_lines` counter SHOULD be updated

#### Amend Modifying AI Content

When amend modifies AI-attributed lines:

- Those lines SHOULD be re-attributed to the human (if modified by human)
- The `overriden_lines` counter SHOULD be incremented
- Original prompt records MUST be preserved (for audit trail)

```
Before amend:
  Commit A (with authorship log)
  Working log: Contains INITIAL attributions for new changes
  
After amend:
  New commit A' (different SHA)
  A''s authorship log: Contains attributions from A + working log
  Original A's authorship log note is removed
```

#### Amend During Other Operations

When amend is used during a rebase or other operation:

- The amend operation MUST be processed after the base operation completes
- Attributions MUST reflect the state after both operations
- See section 3.1.7 for details on amending during rebase

---

### 3.7 Change History Through Rewrites

> **Status: NOT YET IMPLEMENTED.** The rebase/history-rewriting code (`rebase_authorship.rs`) does not currently handle `change_history`. During rebase, cherry-pick, or squash operations, `change_history` will be silently lost. The rules below describe the intended behavior for a future implementation.

When authorship logs are copied, merged, or split during history rewriting operations:

- `change_history` SHOULD be preserved in the same manner as `prompts`.
- When squashing commits, the `change_history` arrays from all squashed commits SHOULD be concatenated in chronological order.
- When splitting commits, `change_history` entries SHOULD be distributed based on which files they affect, matching the split.
- When dropping commits, the `change_history` from dropped commits MUST NOT appear in any resulting notes.

---

## 4. Backwards Compatibility

- Implementations of 4.0.0 MUST accept authorship logs with any `schema_version` starting with `"authorship/"` (i.e., v3.x and v4.x notes are both readable). 
- v3.0.0 implementations cannot read v4.0.0 notes. This is a forward-incompatible upgrade.
- The `change_history`, `subagents`, and message `id` fields all use `skip_serializing_if` semantics — when absent or empty, they are omitted from the serialized JSON. This means a v4.0.0 note with no change history, no message IDs, and no subagents will look identical to a v3.0.0 note aside from the `schema_version` field.
