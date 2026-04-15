# Git AI Standard v4.0.0 — Differences from v3.0.0

This document describes only what changed between v3.0.0 and v4.0.0. For unchanged sections (Attestation Section format, History Rewriting core behaviors, Notes Reference), refer to the [v3.0.0 specification](git_ai_standard_v3.0.0.md).

---

## 1. Schema Version

**v3.0.0:**
```
authorship/3.0.0
```

**v4.0.0:**
```
authorship/4.0.0
```

### Version Check Loosened

v3.0.0 required an exact match against the `schema_version` constant. v4.0.0 changes this to accept any version starting with `"authorship/"`, so implementations can read both v3.x and v4.x notes. This is a one-way upgrade: old v3 implementations cannot read v4 notes, but v4 implementations can read v3 notes.

---

## 2. Metadata Section — New Optional Field

The `AuthorshipMetadata` object gains one new optional field:

| Field | Type | Description |
|-------|------|-------------|
| `change_history` | `array` or `null` | Ordered list of `ChangeHistoryEntry` objects recording every checkpoint (human and AI) that occurred between the parent commit and this commit. Omitted from JSON when `null`/absent. |

---

## 3. Prompt Record — New Fields

Two new optional fields on `PromptRecord`:

| Field | Type | Description |
|-------|------|-------------|
| `cursor_subagents` | `array` of strings | Conversation IDs for Cursor subagent sessions spawned by this conversation. Omitted when absent/empty. |
| (context conversations) | *(behavioral)* | Prompt records MAY now represent zero-code-change conversations (planning, Q&A). Identified by all stats fields being `0`. |

### Context Conversations (New Concept)

v3.0.0 only stored prompt records for conversations that produced code changes. v4.0.0 adds "context conversations" — planning, research, and Q&A sessions that occurred between commits but made no code changes. These are collected from the IDE's conversation database and inserted into the `prompts` map with zero stats. Requirements:

- MUST have `human_author` populated.
- MUST be scoped to the current workspace (unrelated workspaces excluded).
- Identified by `total_additions == 0 && total_deletions == 0 && accepted_lines == 0 && overriden_lines == 0`.

---

## 4. Message Object — New Fields and Types

### New field: `id`

Every `Message` variant (`User`, `Assistant`, `Thinking`, `Plan`, `ToolUse`) gains:

| Field | Type | Description |
|-------|------|-------------|
| `id` | string or null | Unique message identifier from the IDE (e.g., Cursor `bubble_id`). Used for cross-referencing with IDE-native conversation data and prompt_id backfilling. Omitted when absent. |


---

## 5. Checkpoint Behavior Changes

### Human changes no longer skipped

**v3.0.0:** Pre-commit checkpoints exited early if there were no AI edits (`has_no_ai_edits && !has_initial_attributions` returned `Ok(None)`).

**v4.0.0:** That early exit is removed. Pre-commit always runs, recording human changes with line stats and line change ranges even when there are no AI attributions. Human-only file entries are kept with empty attributions but populated line data.

### Deleted files re-included

**v3.0.0:** `get_all_tracked_files()` only listed files currently on disk.

**v4.0.0:** Files from previous checkpoints that no longer exist on disk are re-included so deletion events appear in the change history.

---

## 6. Change History (Entirely New)

The core new data structure. A chronological array of `ChangeHistoryEntry` objects stored in `metadata.change_history`, recording every checkpoint between parent commit and this commit.

### ChangeHistoryEntry

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `timestamp` | integer | REQUIRED | Unix epoch seconds |
| `kind` | string | REQUIRED | `"human"`, `"ai_agent"`, or `"ai_tab"` |
| `conversation_id` | string | OPTIONAL | SHA-256 hash of agent ID, linking to `prompts` map |
| `agent_type` | string | OPTIONAL | AI tool name (e.g., `"cursor"`, `"claude_code"`) |
| `prompt_id` | string | OPTIONAL | IDE-specific message identifier (e.g., Cursor `bubble_id`) |
| `model` | string | OPTIONAL | AI model used |
| `prompt_text` | string | OPTIONAL | Actual user prompt text |
| `files` | object | REQUIRED | Map of file paths to `FileChangeDetail` |
| `line_stats` | object | REQUIRED | Aggregate `CheckpointLineStats` |

### FileChangeDetail

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `added_lines` | array of strings | REQUIRED | Compact range format (`"n"` or `"n-m"`), new-content coordinates |
| `deleted_lines` | array of strings | REQUIRED | Compact range format, old-content coordinates |
| `added_line_contents` | array of strings | OPTIONAL | `"N: <text>"` format, actual added line content |
| `deleted_line_contents` | array of strings | OPTIONAL | `"N: <text>"` format, actual deleted line content |

Contiguous single-line changes SHOULD be merged into ranges.

### CheckpointLineStats

| Field | Type | Default |
|-------|------|---------|
| `additions` | integer | `0` |
| `deletions` | integer | `0` |
| `additions_sloc` | integer | `0` |
| `deletions_sloc` | integer | `0` |

### Prompt text resolution strategy

1. Look up user message matching `prompt_id` in the transcript.
2. Fall back to last user message with timestamp <= checkpoint timestamp.
3. Build a `conv_transcripts` map so earlier checkpoints in the same conversation can resolve prompt text even when the transcript is only attached to the last checkpoint.

### Prompt ID backfilling

v3.0.0 only backfilled `prompt_id` for the last checkpoint per conversation. v4.0.0 builds a full user message timeline from the latest transcript and backfills ALL checkpoints by assigning the last user message whose timestamp <= checkpoint timestamp.

### Example

```json
{
  "change_history": [
    {
      "timestamp": 1712000100,
      "kind": "ai_agent",
      "conversation_id": "d9978a8723e02b52",
      "agent_type": "cursor",
      "prompt_id": "bubble-abc-123",
      "model": "claude-4.5-opus",
      "prompt_text": "Add error handling to the main function",
      "files": {
        "src/main.rs": {
          "added_lines": ["15-22", "30"],
          "deleted_lines": ["15-18"],
          "added_line_contents": [
            "15:     let result = match parse_args() {",
            "16:         Ok(args) => args,",
            "17:         Err(e) => {",
            "18:             eprintln!(\"Error: {}\", e);",
            "19:             std::process::exit(1);",
            "20:         }",
            "21:     };",
            "22: ",
            "30:     Ok(())"
          ],
          "deleted_line_contents": [
            "15:     let args = parse_args();",
            "16:     // no error handling",
            "17:     run(args);",
            "18:     return;"
          ]
        }
      },
      "line_stats": {
        "additions": 9,
        "deletions": 4,
        "additions_sloc": 8,
        "deletions_sloc": 3
      }
    },
    {
      "timestamp": 1712000200,
      "kind": "human",
      "files": {
        "src/main.rs": {
          "added_lines": ["1"],
          "deleted_lines": [],
          "added_line_contents": ["1: // Author: Human Developer"]
        }
      },
      "line_stats": {
        "additions": 1,
        "deletions": 0,
        "additions_sloc": 0,
        "deletions_sloc": 0
      }
    }
  ]
}
```

---

## 7. Working Log Extensions (Internal)

These are internal implementation details not part of the serialized authorship note, but affect what data is available for building `change_history`.

### WorkingLogEntry — New fields

| Field | Type | Description |
|-------|------|-------------|
| `added_line_ranges` | `Vec<(u32, u32)>` or null | 1-based inclusive ranges of added lines in new-content coordinates |
| `deleted_line_ranges` | `Vec<(u32, u32)>` or null | 1-based inclusive ranges of deleted lines in old-content coordinates |
| `added_line_entries` | `Vec<(u32, String)>` or null | (line_number, content) pairs for each added line |
| `deleted_line_entries` | `Vec<(u32, String)>` or null | (line_number, content) pairs for each deleted line |

### Checkpoint — New field

| Field | Type | Description |
|-------|------|-------------|
| `prompt_id` | string or null | The Cursor `bubble_id` of the user message that triggered this checkpoint |

### CheckpointLineStats — Derive change

`CheckpointLineStats` gains `PartialEq` derive (needed for test assertions).

---

## 8. Line History Command (Entirely New)

New CLI command for tracing the authorship history of a specific line.

### Usage

```
git-ai line-history <file> <line> [--commit <sha>]
```

### Algorithm

1. Read line content at the specified commit.
2. `git log -L<line>,<line>:<file>` to find all commits touching that line.
3. For each commit, read authorship note and `change_history`.
4. Walk backward through checkpoints using line mapping to determine which checkpoint introduced/modified the line.

### Line mapping functions

- `reconstruct_diff_ops()` — Rebuilds interleaved Equal/Delete/Insert operations from separate added/deleted range lists.
- `map_new_to_old()` — Maps new-file line number to old-file position; `None` if the line was inserted.
- `reverse_through_insert()` — When a line is in an Insert range, finds old-file position of what was replaced.
- `find_checkpoints_that_touched_line()` — Walks checkpoints in reverse, tracking line position through each diff.

### Output schema

```json
{
  "file": "string",
  "line": "integer",
  "at_commit": "string",
  "line_content": "string",
  "history": [
    {
      "commit_sha": "string",
      "commit_date": "string",
      "commit_message": "string",
      "target_line": "integer",
      "checkpoints": [
        {
          "timestamp": "integer",
          "kind": "string",
          "agent_type": "string | null",
          "model": "string | null",
          "prompt_id": "string | null",
          "prompt_text": "string | null",
          "line_content": "string | null",
          "additions": "integer",
          "deletions": "integer"
        }
      ]
    }
  ]
}
```

---

## 9. Cursor Integration Extensions (Entirely New)

### Workspace-composer map

Scan `~/.cursor/User/workspaceStorage/*/` to map conversation IDs to workspace paths. 

### Conversation workspace root fallback

Query `cursorDiskKV` for `messageRequestContext:<conversation_id>:<bubble_id>` and extract `projectLayouts[0].listDirV2Result.directoryTreeRoot.absPath`.

### Workspace scoping change

Conversations with unknown workspace are excluded. Scoping uses canonical paths.

### Subagent discovery

Scan `~/.cursor/projects/*/agent-transcripts/<conversation_id>/subagents/*.jsonl` to populate `cursor_subagents` on parent `PromptRecord`.

---

## 10. History Rewriting — Additions

> **Status: NOT YET IMPLEMENTED.** The rebase/history-rewriting code (`rebase_authorship.rs`) does not currently handle `change_history`. During rebase, cherry-pick, or squash operations, `change_history` will be silently lost. The rules below describe the intended behavior for a future implementation.

Core rewriting behaviors (rebase, merge, reset, cherry-pick, stash, amend) are unchanged from v3.0.0. The following rules SHOULD be added for the new `change_history` field:

- `change_history` SHOULD be preserved in the same manner as `prompts` during copy/merge/split.
- **Squash:** Concatenate `change_history` arrays chronologically.
- **Split:** Distribute entries based on which files they affect.
- **Drop:** Dropped commits' `change_history` MUST NOT appear in resulting notes.

---

## 11. Backwards Compatibility Changes

**v3.0.0:**
> Implementations of 3.0.0 or later SHOULD NOT attempt to process earlier versions. Implementations > 3.0.0 MUST process earlier versions, provided they are valid and match the schema they advertise.

**v4.0.0:**
- Implementations MUST accept any `schema_version` starting with `"authorship/"` (both v3 and v4 readable).
- v3 implementations cannot read v4 notes (forward-incompatible).
- New optional fields (`change_history`, `cursor_subagents`, message `id`) use `skip_serializing_if` semantics — omitted from JSON when absent. A v4 note with no change history looks identical to v3 except for `schema_version`.

### Errata

E-001 (carried from v3.0.0): `overriden_lines` typo remains for compatibility.
