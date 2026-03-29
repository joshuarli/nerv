# Built-in Tools

Nerv provides 10 tools to the LLM. Readonly tools (`read`, `grep`, `find`,
`ls`, `symbols`, `codemap`) execute in parallel when all calls in a turn are
readonly; otherwise tools run sequentially. File-mutating tools (`edit`,
`write`) serialize through a per-file mutex to prevent concurrent writes
to the same path.

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
| `offset` | integer | no | 1 (first line) |
| `limit` | integer | no | all lines |

Output is `cat -n` style: `{line_number}\t{content}`. Line numbers are
1-based. Files are read as bytes and decoded via lossy UTF-8. Output is
head-truncated at 3000 lines when no explicit range is given.

**Deduplication**: an in-memory cache tracks `(path, mtime, ranges_served)`.
Full re-reads of unchanged files return `[unchanged since last read]`.
Ranged re-reads that are fully contained within a previously-served range
return `[already read {path} lines N-M]`. This prevents degenerate loops
where the model reads the same code region repeatedly. The cache invalidates
automatically when the file is modified (mtime change).

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

**Output filter pipeline**: before the result is stored in the context, the
raw output passes through `tools::output_filter::filter_bash_output`. See
[Context optimization § 6](context.md#6-bash-output-filter-pipeline) for the
full pipeline description. The key property is that the pipeline is zero-alloc
for plain output (no ANSI, no dedup run, no JSON, no known command) via
`Cow::Borrowed` passthrough.

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

## symbols

Search the tree-sitter symbol index for definitions.

| Param | Type | Required |
|---|---|---|
| `query` | string | yes (empty string matches all) |
| `kind` | string | no |
| `file` | string | no |
| `references` | boolean | no (default false) |

Returns symbol names, kinds, file locations, and signatures. When `query`
is empty and no `file` filter is given, the output also includes a DOCS
section listing markdown files in the project (via `rg --files --glob *.md`),
capped at 20 entries.

When `references` is true, also runs ripgrep (`--word-regexp`) on the query
to find call sites / usages.

## codemap

Show symbol implementations from the codebase. Uses the tree-sitter symbol
index to find matching definitions, reads their source bodies from disk, and
returns a structured assembly grouped by file. Replaces multiple `read` calls
when the model needs to understand how something works.

| Param | Type | Required | Default |
|---|---|---|---|
| `query` | string | yes | — |
| `kind` | string | no | all kinds |
| `file` | string | no | whole project |
| `depth` | string (`signatures`/`full`) | no | `signatures` |

**Depth modes**:
- `signatures`: one-line signature per symbol, no disk reads beyond the index
- `full`: complete source bodies for matched symbols, read from disk

Output is grouped by file with line numbers. If total output exceeds ~4000
lines, excess symbols are demoted from `full` to `signatures`.

**Redirect on miss**: when a non-empty query returns no results but
definitions exist in scope, returns a redirect message like "No symbols
matching 'foo'. 42 definitions exist in this scope — use query: \"\" to see
them all." This prevents grep spirals that start from a failed codemap lookup.

The search and render phases are split (`codemap::search` + `codemap::render`)
so that the index lock is released before file I/O, enabling parallel codemap
calls without serializing on the mutex.

Also available as a CLI subcommand: `nerv codemap <query> [--kind <kind>]
[--file <path>] [--depth full|signatures]`. CLI defaults to `full` depth.

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

### Tree-sitter symbol index (`index/mod.rs`, `index/codemap.rs`)

`symbols` and `codemap` are backed by an in-process tree-sitter parse of the
project. The index maps file paths to their parsed `SymbolDef` list (name,
kind, start/end byte offsets, one-line signature). Currently indexed languages:
Rust, TypeScript/TSX, Python, Go, C/C++.

**Lazy, query-time indexing.** The index is not updated when `edit` or `write`
runs — it is rebuilt on each `symbols`/`codemap` call. `force_index_dir` walks
the directory, compares stored mtimes against the current filesystem, and
re-parses only changed or new files. Deleted files are evicted. This means a
`symbols` call immediately after an `edit` sees the updated symbols; there is
no background watcher or stale-index window.

**On-disk SQLite cache (`~/.nerv/symbol_cache.db`).** Parsing is the expensive
step. The cache stores `(path, mtime) → JSON-serialized Vec<SymbolDef>` so
that unchanged files skip tree-sitter entirely on the next run. Schema:

```sql
CREATE TABLE symbol_cache (
    path  TEXT    NOT NULL,
    mtime INTEGER NOT NULL,
    data  TEXT    NOT NULL,
    PRIMARY KEY (path, mtime)
);
```

Cache lifecycle:
- **Hit**: file path + mtime matches a row → deserialize, skip parse.
- **Miss**: parse with tree-sitter → insert new row.
- **Stale eviction**: when a file's mtime has changed, the old row is
  implicitly bypassed (different mtime key) and a fresh row is written.
  `remove()` explicitly deletes all rows for a path when the file is deleted.
- **Graceful degradation**: if the cache DB can't be opened (permissions,
  missing `~/.nerv/` dir), `SymbolIndex` falls back to in-memory-only mode
  with no error.

The cache is a separate file from `sessions.db` to avoid schema coupling.
WAL journal mode is enabled for concurrent reads.

`SymbolIndex::new()` creates an index with no persistent cache (used by
tests). `SymbolIndex::new_with_cache(nerv_dir)` is the production path, wired
up in bootstrap.

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
