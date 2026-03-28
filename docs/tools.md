# Built-in Tools

Nerv provides 9 tools to the LLM. All execute synchronously in the session
thread. File-mutating tools (`edit`, `write`) serialize through a per-file
mutex to prevent concurrent writes to the same path.

## Design principle: separate LLM content from display

Every tool result has three channels:

- **`content`** (string) — sent to the LLM as the tool result. Must be
  minimal. The LLM already knows what it asked for; it doesn't need
  verbose confirmation. Every token here costs money and consumes context.
- **`details.display`** (string, optional) — compact summary shown in the
  TUI instead of the full content. If absent, the TUI falls back to
  truncated content.
- **`details`** (JSON, optional) — rich metadata for the TUI. Diffs,
  exit codes, truncation info. Never sent to the LLM.

What the user sees vs what the LLM sees:

| Tool | LLM (`content`) | User (`details.display`) |
|---|---|---|
| read | Full file with line numbers | `foo.rs (50 lines)` |
| edit | `Edited foo.rs` | Full unified diff |
| grep | All matching lines | `12 matches` |
| find | All file paths | `8 files` |
| ls | Full tree | `. (24 entries)` |
| bash | Full stdout + stderr | First 3 lines + count |
| write | `Wrote 1234 bytes to foo.rs` | same |

## read

Read file contents with line numbers.

| Param | Type | Required | Default |
|---|---|---|---|
| `path` | string | yes | — |
| `offset` | integer | no | 0 (first line) |
| `limit` | integer | no | 3000 |

Output is `cat -n` style: `{line_number}\t{content}`. Line numbers are
1-based. Files are read as bytes and decoded via lossy UTF-8. Output is
head-truncated at 3000 lines.

## write

Write content to a file, creating parent directories as needed.

| Param | Type | Required |
|---|---|---|
| `path` | string | yes |
| `content` | string | yes |

Overwrites the file entirely. Serialized through the file mutation queue.
Returns byte count on success.

## edit

Replace exact text in a file. Two modes: single replacement and multi-edit.

### Single replacement

| Param | Type | Required |
|---|---|---|
| `path` | string | yes |
| `old_text` | string | yes |
| `new_text` | string | yes |

Finds `old_text` in the file and replaces it with `new_text`. The match must
be unique — if `old_text` appears more than once, the edit is rejected.

**Fuzzy matching fallback**: if an exact match fails, the tool normalizes
smart quotes (`\u201C` → `"`), em dashes (`\u2014` → `-`), and trailing
whitespace, then retries. Fuzzy matches must also be unique. The output
indicates when a fuzzy match was applied.

### Multi-edit

| Param | Type | Required |
|---|---|---|
| `path` | string | yes |
| `edits` | array of `{old_text, new_text}` | yes |

Applies multiple disjoint replacements atomically. Cannot be combined with
top-level `old_text`/`new_text`.

**Algorithm**:
1. All `old_text` values are matched against the **original** file content
   (not incrementally after each edit). Each must match exactly once.
2. Matches are sorted by position (top-to-bottom).
3. Overlap detection: if any two matches overlap, the edit is rejected.
4. Replacements are applied in reverse position order to preserve byte offsets.
5. A unified diff is generated for the whole file (stored in `details.diff`
   for TUI display, not sent to the LLM).

Multi-edit requires exact matches — no fuzzy fallback. This is intentional:
fuzzy normalization is lossy and applying it across multiple edits on the
same file content risks position drift.

### Shared behaviors

- **Line ending preservation**: CRLF files stay CRLF. Edits are internally
  normalized to LF for matching, then restored on write.
- **BOM preservation**: UTF-8 BOM (`\uFEFF`) is detected, stripped for
  matching, and restored on write.
- **No-change detection**: if old_text and new_text produce identical
  content, the edit is rejected with an error.
- **File size guard**: files over 10MB are rejected.
- **LLM result**: terse confirmation (`"Edited {path}"`). The LLM wrote
  the edit — it doesn't need the diff back.
- **TUI display**: full unified diff (Myers algorithm, 3 lines context)
  stored in `details.diff` for interactive display to the user.
- **Mutation queue**: edits to the same file are serialized.

## bash

Execute a shell command.

| Param | Type | Required | Default |
|---|---|---|---|
| `command` | string | yes | — |
| `timeout` | integer | no | — |

Runs `/bin/bash -c "{command}"` with piped stdout/stderr. The shell is
always `/bin/bash` regardless of `$SHELL` — interactive shells (like custom
`ish`) fail without a tty.

**Streaming**: stdout is read in 8KB chunks and forwarded to the update
callback for real-time display. Stderr is drained on a background thread
to prevent pipe deadlocks.

**Output**: stdout followed by `\n[stderr]\n{stderr}` if stderr is
non-empty. Non-zero exit codes are reported as `[exit code: N]` and
marked as errors. Output is tail-truncated at 200KB / 3000 lines.

## grep

Search file contents using ripgrep.

| Param | Type | Required |
|---|---|---|
| `pattern` | string | yes |
| `path` | string | no (default `.`) |
| `include` | string | no |

Runs `rg --no-heading --line-number --color=never {pattern} {path}`. The
`include` parameter maps to `--glob`. No matches returns a non-error
"No matches found" message. Output is tail-truncated at 200KB / 3000 lines.

## find

Find files by name pattern using fd.

| Param | Type | Required |
|---|---|---|
| `pattern` | string | yes |
| `path` | string | no (default `.`) |

Runs `fd --glob {pattern} {path}`. Output is tail-truncated.

## ls

List directory contents as a tree.

| Param | Type | Required |
|---|---|---|
| `path` | string | no (default `.`) |

Runs `eza --tree -L2 --icons=never {path}`. Output is tail-truncated.

## memory

Read and write persistent memories stored in `~/.nerv/memory.md`.

| Param | Type | Required |
|---|---|---|
| `action` | string (`read`/`add`/`remove`) | yes |
| `content` | string | for `add`/`remove` |

Memories are stored as plain text lines. The memory file is reloaded into
the system prompt before each agent turn, so changes take effect on the
next interaction.

## Infrastructure

### Output truncation (`truncate.rs`)

All external command output is truncated to prevent context blowup:
- **Max bytes**: 200,000
- **Max lines**: 3,000
- `truncate_tail`: keeps the last N bytes/lines (used by bash, grep, find, ls)
- `truncate_head`: keeps the first N lines (used by read)

Truncation messages indicate how much was omitted.

### File mutation queue (`file_mutation_queue.rs`)

A per-file mutex that serializes concurrent mutations. The `edit` and `write`
tools use `mutation_queue.with(path, || { ... })` to ensure only one
operation modifies a file at a time. Different files can be written in
parallel.

The key is the resolved absolute path, so `./foo.rs` and
`/absolute/path/foo.rs` correctly serialize against each other.

### Diff generation (`diff.rs`)

In-process Myers diff algorithm (~170 lines). Generates standard unified
diff format with configurable context lines. No external dependencies —
replaces the `similar` crate that previously added 51KB to the binary.

Used by the edit tool to show what changed. The diff goes to the LLM as
tool output and to the TUI for display.
