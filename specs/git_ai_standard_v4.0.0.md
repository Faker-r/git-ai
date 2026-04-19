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
5. **Cursor subagent tracking**: Link parent Cursor conversations to their spawned subagent conversations.
6. **Message IDs**: Add IDs to transcript messages for finer prompt traceability

### Backward Compatibility

- v4.0.0 implementations MUST accept authorship logs with any `schema_version` starting with `"authorship/"` (i.e., both v3.x and v4.x notes).
- v3.0.0 implementations cannot read v4.0.0 notes due to new required fields. This is a forward-incompatible change.
- The attestation section format is unchanged from v3.0.0.

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

The attestation section is unchanged from v3.0.0. It maps files to the AI sessions that authored specific lines.

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
- Files with no AI Attributions MUST NOT be included in the Attestation Section

#### Attestation Entry Lines

Each attestation entry MUST be indented with exactly two spaces and contain:
1. A **hash** pointing to a prompt in the Metadata Section (16 hexadecimal characters)
2. A single space
3. A **line range specification**

```
  d9978a8723e02b52 1-4,9-10,12,14,16
```

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

#### Session Hash Semantics

Each session hash in the attestation section MUST correspond to a key in the `prompts` object of the metadata section. Session hashes:
- MUST be hexadecimal characters only
- MUST be generated using SHA-256 of `{tool}:{conversation_id}`, taking the first 16 hex characters. Ie `cursor-${conversation_id}` or `claude-code-${conversation_id}` or `amp-${thread_id}`
- SHOULD remain stable for the same AI session across commits

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
| `cursor_subagents` | array | OPTIONAL | **NEW in v4.0.0.** Array of conversation ID strings for Cursor subagent sessions spawned by this conversation. Only populated for Cursor conversations that have a non-empty subagents directory. |

#### Context Conversations

**NEW in v4.0.0.** Prompt records MAY represent "context conversations" — planning, research, or Q&A sessions that occurred between commits but produced no code changes. These are identified by having `total_additions`, `total_deletions`, `accepted_lines`, and `overriden_lines` all set to `0`.

Context conversations:
- MUST be scoped to the current workspace (conversations from unrelated workspaces MUST be excluded)
- SHOULD be included to provide a complete picture of the development process, even though they did not directly produce code

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
- MAY contain thinking messages (`type: "thinking"`)
- MAY contain plan messages (`type: "plan"`)
- MUST NOT contain tool responses (the results returned from tool executions)

Tool responses are excluded because they often contain large amounts of file content, command output, or other verbose data that would bloat the authorship log without adding meaningful attribution context.

---

### 1.2.6 Checkpoint Behavior

Checkpoints are the mechanism with which git-ai captures human vs AI changes. 
A checkpoint is a diff between previously captured file contents and the file contents at the new checkpoint, with an attribution to either a human or AI author. 

#### When checkpointing occurs
Checkpointing occurs at the boundary between human and AI changes. 
When an AI tool uses a fileEdit command, it will fire a preToolUse hook, initiating a human checkpoint. All file changes detected during this checkpoint will be attributed to the human developer. 
This captures all previous human changes before AI tools begin making changes. 

Then, a postToolUse hook is fired after the AI tool completes editing. This initiates an AI checkpoint and attributes changes made by the AI during that fileEdit command. 

Checkpoint metadata is persisted, with a subset of collected data being placed into the change_history field in the metadata section. 

You can see a visualization at https://usegitai.com/docs/cli/how-git-ai-works#part-1-what-happens-on-a-developers-machine


#### Prompt association
For AI checkpoints, the prompt that generated the fileEdit will be saved. 
This results in a 1 to N relationship between prompts and checkpoints, as a single prompt can lead to multiple fileEdits.


#### What files are compared during checkpointing
A checkpoint only diffs file contents for tracked files. 
Tracked files have the following inclusion-exclusion criteria:

Inclusion:
- git status output - all modified files reported by git status, including `Untracked files` files. 
- files with previous checkpoint - all files that were included in a previous checkpoint for this base commit. 
  This includes untracked + deleted files which would not get picked up by git status' output (enables tracking of temporary files that get deleted)

Exclusion:
- A list of unnecessary generated content, such as node_modules, .DS_store, ...

#### Human Change Tracking

**NEW in v4.0.0.** Implementations MUST capture human changes at pre-commit time, not just AI changes. The pre-commit checkpoint MUST NOT skip early when there are no AI edits. This ensures that every commit produces authorship data, including purely human work.

For human-only files during pre-commit, the implementation MUST still create working log entries with line stats and line change ranges, even though no AI attributions are recorded.

#### Deleted File Tracking

**NEW in v4.0.0.** When computing tracked files for a checkpoint, implementations MUST re-include files from previous checkpoints that have been deleted from disk. This ensures file deletion events are captured in the change history.

---

### 1.2.7 Change History

**NEW in v4.0.0.** The `change_history` field in the metadata section records a chronological sequence of every checkpoint that occurred between the parent commit and this commit. This is the primary research data structure — it captures both human and AI changes at line-level granularity.

Note that it records all changes between the timestamps of the parent commit and the current commit, regardless of whether those changes were committed. 

#### ChangeHistoryEntry Object

Each entry in the `change_history` array MUST contain:

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `timestamp` | integer | REQUIRED | Unix timestamp (seconds since epoch) when the checkpoint was created |
| `kind` | string | REQUIRED | One of: `"human"`, `"ai_agent"`, `"ai_tab"` |
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

#### Change History Example

```json
{
  "change_history": [
    {
      "timestamp": 1776571396,
      "kind": "ai_agent",
      "conversation_id": "5c9cf6c50078d7f6",
      "agent_type": "cursor",
      "prompt_id": "015fc1a5-1fe9-4ebc-a77c-f2461a8ee974",
      "model": "gpt-5.4-medium",
      "prompt_text": "edit @test.py to add a comment",
      "files": {
        "test.py": {
          "added_lines": [
            "31"
          ],
          "deleted_lines": [],
          "added_line_contents": [
            "31: # These lines should be attributed to Cursor (AI)"
          ],
        }
      },
      "line_stats": {
        "additions": 1,
        "deletions": 0,
        "additions_sloc": 1,
        "deletions_sloc": 0
      }
    },
    {
      "timestamp": 1776571429,
      "kind": "human",
      "files": {
        "tmp_testing.py": {
          "added_lines": [
            "15-17"
          ],
          "deleted_lines": [],
          "added_line_contents": [
            "15: ",
            "16: ",
            "17: # human added stuff."
          ]
        }
      },
    }
  ]
}
```

---

### 1.2.8 Complete Example

```
src/main.rs
  abcd1234abcd1234 1-10,15-20
  efgh5678efgh5678 25,30-35
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
      "total_additions": 25,
      "total_deletions": 5,
      "accepted_lines": 20,
      "overriden_lines": 0,
      "cursor_subagents": ["sub-conv-001", "sub-conv-002"]
    },
    "1234abcd5678efgh": {
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
            "4: fn main() -> Result<()> {"
          ],
          "deleted_line_contents": [
            "1: fn main() {",
            "2:     run();",
            "3: }"
          ]
        },
        "src/lib.rs": {
          "added_lines": ["1-50"],
          "deleted_lines": []
        }
      },
      "line_stats": {
        "additions": 65,
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
          "deleted_lines": []
        }
      },
      "line_stats": {
        "additions": 7,
        "deletions": 0,
        "additions_sloc": 6,
        "deletions_sloc": 0
      }
    },
    {
      "timestamp": 1733363100,
      "kind": "ai_agent",
      "conversation_id": "efgh5678efgh5678",
      "agent_type": "cursor",
      "prompt_id": null,
      "model": "claude-3-sonnet",
      "prompt_text": "Add logging",
      "files": {
        "src/main.rs": {
          "added_lines": ["25", "30-35"],
          "deleted_lines": []
        }
      },
      "line_stats": {
        "additions": 6,
        "deletions": 0,
        "additions_sloc": 6,
        "deletions_sloc": 0
      }
    }
  ]
}
```

In the above example:
- Two prompt sessions are recorded: one that produced code changes and one context conversation (planning session) with zero code stats.
- The `change_history` shows three checkpoints in chronological order: an AI edit, a human edit, and another AI edit.
- The context conversation (`1234abcd5678efgh`) has zero stats but preserves the planning discussion.
- The first prompt record includes `cursor_subagents` linking to two subagent conversations.
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
| `kind` | string | One of: `"human"`, `"ai_agent"`, `"ai_tab"` |
| `agent_type` | string | OPTIONAL. The AI tool identifier |
| `model` | string | OPTIONAL. The AI model used |
| `prompt_id` | string | OPTIONAL. The IDE-specific message identifier |
| `prompt_text` | string | OPTIONAL. The user prompt text |
| `line_content` | string | OPTIONAL. The content of the tracked line after this checkpoint |
| `additions` | integer | Total lines added at this checkpoint |
| `deletions` | integer | Total lines deleted at this checkpoint |

---

## 3. Cursor Integration Extensions

**NEW in v4.0.0.** The following extensions support deeper integration with Cursor IDE.

### 3.4 Subagent Discovery

For Cursor conversations, implementations SHOULD discover subagent conversation IDs and populate the `cursor_subagents` field on the parent `PromptRecord`.

---

## 4. History Rewriting Behaviors

Authorship Logs can be attached to one, and only one commit SHA. When users do Git operations like `rebase`, `cherry-pick`, `reset`, `merge`, `stash`/`pop`, that rewrite the worktree and history, corresponding changes to Authorship Logs are required.

The history rewriting behaviors are unchanged from v3.0.0 (see [Git AI Standard v3.0.0, Section 2](git_ai_standard_v3.0.0.md#2-history-rewriting-behaviors)) with the following additions:

### 4.1 Change History Through Rewrites

> **Status: NOT YET IMPLEMENTED.** The rebase/history-rewriting code (`rebase_authorship.rs`) does not currently handle `change_history`. During rebase, cherry-pick, or squash operations, `change_history` will be silently lost. The rules below describe the intended behavior for a future implementation.

When authorship logs are copied, merged, or split during history rewriting operations:

- `change_history` SHOULD be preserved in the same manner as `prompts`.
- When squashing commits, the `change_history` arrays from all squashed commits SHOULD be concatenated in chronological order.
- When splitting commits, `change_history` entries SHOULD be distributed based on which files they affect, matching the split.
- When dropping commits, the `change_history` from dropped commits MUST NOT appear in any resulting notes.

---

## 5. Backwards Compatibility

- Implementations of 4.0.0 MUST accept authorship logs with any `schema_version` starting with `"authorship/"` (i.e., v3.x and v4.x notes are both readable).
- v3.0.0 implementations cannot read v4.0.0 notes. This is a forward-incompatible upgrade.
- The `change_history`, `cursor_subagents`, and message `id` fields all use `skip_serializing_if` semantics — when absent or empty, they are omitted from the serialized JSON. This means a v4.0.0 note with no change history and message ids will look identical to a v3.0.0 note aside from the `schema_version` field.
