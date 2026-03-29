# Context optimization

Every request sends the full conversation history to the LLM. Context
transforms reduce token usage without losing information the model needs.

## Pipeline

```
AgentMessage[]
  → transform_context()    7 optimizations: strip thinking/orphans/denied args,
                           truncate stale, supersede reads, bash output filters, strip edit args
  → context gate           circuit breaker for unexpected context growth
  → convert_to_llm()       AgentMessage → LlmMessage (merge consecutive same-role)
  → build_request_body()   LlmMessage → provider-specific JSON
  → serialize              JSON → bytes for HTTP
```

**Bash output pipeline** (runs at tool execution time, before the above):

```
bash.execute() raw output
  → output_filter::filter_bash_output()   ANSI strip → dedup → JSON schema → language compression
  → output gate (run_one_tool)             human y/n when filtered output > 50KB
  → AgentMessage::ToolResult              details.filtered: true tells transform_context to skip
```

## transform_context (`src/agent/convert.rs`)

Applied before every LLM request. Seven optimizations (all zero-LLM-cost):

### 1. Strip thinking blocks

Extended thinking content is never referenced by the model in subsequent
turns. A single thinking block can be 10k+ tokens. All `ContentBlock::Thinking`
blocks are removed from assistant messages.

**Savings**: 1k–10k+ tokens per response with thinking enabled.

### 2. Strip denied tool call arguments

When a tool call was denied by the permission system (tool result contains
"denied" and `is_error: true`), the tool_use block's arguments are replaced
with `{}`. The model only needs to know the tool name and that it was denied,
not the full 5KB file content it tried to write.

**Savings**: up to 1.5k tokens per denied tool call.

### 3. Remove orphaned tool calls

If the model produced a `tool_use` block but no matching `tool_result` exists
(e.g., the request was aborted mid-stream), the tool call is removed from
the assistant message. Anthropic's API requires every tool_use to have a
matching tool_result — orphans cause 400 errors.

If removing all content from an assistant message leaves it empty, the
entire message is dropped.

### 4. Truncate stale tool results

Tool results older than `RECENT_TURNS` (10 turns from the end) are truncated
to a 200-character preview with a `[truncated: N lines, N chars]` suffix.

Recent results are preserved in full because the model may reference them.

**Savings**: varies, but large file reads (2k+ chars) become ~200 chars.

### 5. Superseded read deduplication

When the model reads the same file multiple times (common in edit-verify cycles),
earlier reads are replaced with `[superseded by later read]`. Walk backwards
through messages tracking `(tool="read", path)` pairs; earlier reads of the same
path are marked as superseded. Error reads are preserved.

**Savings**: 200-2k+ tokens per redundant read. In a typical mass-edit session
with 8 redundant reads, saves ~4-8k tokens.

### 6. Bash output filter pipeline

Every bash `ToolResult` passes through `tools::output_filter::filter_bash_output`
**at execution time** (inside `bash.execute()`, before the result enters
`run_one_tool`). This means the output gate sees the post-compression size.
The `details.filtered: true` flag on the stored message tells
`transform_context` to skip this step for that tool call (it has already
been applied). This is a four-stage pipeline:

1. **ANSI strip** — removes all escape sequences. Returns `Cow::Borrowed`
   (zero allocation) when the input contains no escape codes.
2. **Line dedup** — collapses runs of ≥3 identical consecutive lines to
   `line (×N)`. Returns `Cow::Borrowed` when no run is present.
3. **JSON schema** — if the output is a large JSON blob (>500 chars, valid
   JSON object/array), replaces it with a key/type skeleton. Useful when
   the model calls an API and gets a huge response back.
4. **Language filter** — command-aware compression for known test runners
   and build tools. Routing is two-tier:
   - *Command-based*: substring match on the command string.
   - *Heuristic fallback*: content-signal match for commands that wrap
     known tools (e.g. `make test` running `cargo test` internally).

   | Command pattern | Filter |
   |---|---|
   | `cargo test` | `rust::filter_cargo_test` |
   | `cargo build/check/clippy` | `rust::filter_cargo_build` |
   | `go test` | `go::filter_go_test` |
   | `pytest`, `py.test` | `python::filter_pytest` |
   | `python -m unittest` | `python::filter_unittest` |
   | `jest` | `ts::filter_jest` |
   | `vitest` | `ts::filter_jest` |

   Heuristic signals (content-based, fires when no command match):
   - First line matches `{"Action":` → Go JSON test output
   - Output contains `test result:` → cargo test
   - Output contains `Compiling ` or `error[E` → cargo build
   - Output contains `test session starts` → pytest
   - Any line starts with `PASS ` or `FAIL ` → jest/vitest

Each language filter returns `None` (pass-through) when it recognises
no content to compress, so unrecognised output is never mangled.

**Savings**: 100–2k tokens per successful build/test output. On error,
filters extract just the relevant failures/errors, discarding progress
noise and unrelated stdout.

### 7. Strip stale edit/write arguments

For tool calls in stale turns (before the `RECENT_TURNS` cutoff), edit and
write tool call arguments are reduced to just the `path` field. The `old_text`,
`new_text`, and `content` fields are stripped — the edit already happened and
the model doesn't need the full payload to understand what was changed.

**Savings**: 100-5k tokens per stale edit (depends on payload size).

## Tool-level optimizations

These are applied at tool execution time, not in `transform_context`.

### 8. Read tool: mtime cache + range dedup (`src/tools/read.rs`)

The read tool supports optional `offset`/`limit` parameters for reading
specific line ranges. An in-memory cache tracks `path → (mtime, line_count,
ranges_served)` and provides two levels of dedup:

- **Full-file dedup**: when the model re-reads an entire file and the mtime
  hasn't changed, returns `[unchanged since last read: path (N lines)]`.
- **Range dedup**: when the model requests a line range fully contained
  within a previously-served range (same file, unchanged mtime), returns
  `[already read path lines N-M]`. This prevents degenerate loops where
  the model reads the same function body 5+ times.

Cache invalidates automatically when writes change the file (mtime check).

**Savings**: eliminates redundant re-reads (200-12k+ tokens each).

### 9. Grep context lines (`src/tools/grep.rs`)

The grep tool passes `--context=3` to ripgrep, so the model gets surrounding
lines with each match. Reduces follow-up read calls for understanding call
sites.

## Circuit breaker (`src/agent/agent.rs`)

Before each API call in the agentic loop, the estimated token count is
compared to the previous call. If **all** of these hold, the user is prompted:
- At least 4 tool rounds have completed (warmup — early reads are expected)
- Absolute delta exceeds 20k tokens
- Relative growth exceeds 30%

This catches runaway context growth (e.g., reading a massive file, verbose
test output) before it becomes an expensive API call. Uses the same y/n
TUI prompt as permission requests.

## Output gate (`src/agent/agent.rs` + `src/core/agent_session.rs`)

After bash executes and the output filter pipeline runs, if the filtered
result still exceeds **50 KB** (~12k tokens), the user is prompted:

```
⚠ Output gate: bash
  cargo build --verbose
  1247 lines / ~18k tokens
  y = allow, n = deny (model gets hint to retry)
```

- **y (allow)**: full filtered output goes into context, same as today.
- **n (deny)**: the tool result is replaced with a structured hint error:

  ```
  [output-too-large: 1247 lines / ~18k tokens]
  Command: cargo build --verbose
  Output was too large to include in context. Options:
  - Pipe through grep/awk/sed to filter first: <cmd> | grep pattern
  - Redirect to a file and use the read tool with offset/limit
  - Use a more targeted command
  ```

The model reads this as `is_error: true` and self-corrects (e.g. re-runs
with `| grep "^error"`). The gate fires exactly once per tool call.

**Print mode**: `output_gate_fn` is only wired in interactive TUI mode
(`agent_session.rs`). In `--print` / headless mode the gate is absent and
large outputs pass through to context unblocked.

**Pipeline position**: output gate runs *after* `filter_bash_output` and
*before* `AgentMessage::ToolResult` is pushed to `agent.state.messages`.
This is later than the context gate (which fires before the API call).
The two are complementary:
- Output gate: per-tool, post-execute, on raw result size
- Context gate: per-API-call, on aggregate context token delta

## Compaction (`src/compaction/`)

Separate from `transform_context`. Compaction summarizes and removes old
messages entirely, triggered by:

- **Threshold**: context usage exceeds 80% of window (proactive)
- **Overflow**: API returns context-too-long error (reactive, retry after)
- **Manual**: user runs `/compact`

Compaction uses the LLM itself to summarize the removed messages into a
`CompactionSummary` message. The summary replaces everything before the
cut point. A `first_kept_entry_id` tracks where the kept messages begin.

### Token counting

Uses a simple `chars / 4` heuristic for token estimation. Counts are
approximate — the real count comes from the API's usage response.

## convert_to_llm (`src/agent/convert.rs`)

Converts internal `AgentMessage` types to provider-neutral `LlmMessage`:

- `User` → `LlmMessage::User`
- `Assistant` → `LlmMessage::Assistant`
- `ToolResult` → `LlmMessage::ToolResult`
- `Custom`, `BashExecution`, `CompactionSummary`, `BranchSummary` → `LlmMessage::User` (text)

Consecutive same-role messages are merged (Anthropic requires alternating
user/assistant roles).

## build_request_body

Provider-specific. Anthropic and OpenAI-compat have different wire formats:

**Anthropic**: system prompt as array of content blocks, messages with
`tool_use`/`tool_result` types, cache_control annotations on last user
message and system prompt.

**OpenAI-compat**: system as first message, tool calls as `function` type,
tool results as `role: "tool"` messages.

## Benchmarks

Run with `cargo bench --bench json_encoding`. Results on M1 Pro, 50 turns
with tool calls:

| Stage | Time |
|---|---|
| transform_context | 41us |
| convert_to_llm | 16us |
| build_request_body (Anthropic) | 146us |
| JSON serialize | 37us |
| **Full pipeline** | **265us** |

The encoding cost is negligible compared to network + inference time.
The real savings come from reducing the token count that the LLM processes.
