# Context optimization

Every request sends the full conversation history to the LLM. Context
transforms reduce token usage without losing information the model needs.

## Pipeline

```
AgentMessage[]
  → transform_context()    7 optimizations: strip thinking/orphans/denied args,
                           truncate stale, supersede reads, compress bash, strip edit args
  → context gate           circuit breaker for unexpected context growth
  → convert_to_llm()       AgentMessage → LlmMessage (merge consecutive same-role)
  → build_request_body()   LlmMessage → provider-specific JSON
  → serialize              JSON → bytes for HTTP
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

### 6. Bash success pattern compression

Successful bash tool results matching known patterns are compressed to a
single summary line regardless of age:
- `cargo check/build` with no errors → the `Finished ...` line only
- `cargo test` with 0 failures → the `test result: ok. ...` line only
- Python `unittest` with OK → the `Ran N tests ...` line only
- `pytest` with no failures → the summary line only
- `make` with nothing to do → the `Nothing to be done` line

**Savings**: 100-2k tokens per successful build/test output.

### 7. Strip stale edit/write arguments

For tool calls in stale turns (before the `RECENT_TURNS` cutoff), edit and
write tool call arguments are reduced to just the `path` field. The `old_text`,
`new_text`, and `content` fields are stripped — the edit already happened and
the model doesn't need the full payload to understand what was changed.

**Savings**: 100-5k tokens per stale edit (depends on payload size).

## Tool-level optimizations

These are applied at tool execution time, not in `transform_context`.

### 8. Read tool: whole-file with mtime cache (`src/tools/read.rs`)

The read tool always returns the entire file (no offset/limit parameters).
For specific line ranges, the model uses `bash` + `sed -n '100,200p'`.
This eliminates the chunked-read problem where models read 1000-line files
in 120-line slices (10+ API calls).

An in-memory mtime cache tracks `path → (mtime, line_count)`. When the
model reads a file it already read and the mtime hasn't changed, the tool
returns `[unchanged since last read: path (N lines)]` instead of the full
content. Cache invalidates automatically when writes change the file.

Line number width adapts to file size (3/4/6 digits). Truncation at 3000
lines for truly massive files.

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

Uses tiktoken (cl100k_base BPE) for token estimation. Counts are
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
