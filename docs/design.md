# Design Principles

Hard-won lessons from building and evaluating nerv. These aren't aspirational
— each one is backed by eval data showing measurable impact.

## The LLM doesn't need what the human needs

This is the single most impactful principle. Every tool result, every status
display, every piece of feedback has two audiences with opposite needs:

- **The model** wants minimal, structured confirmation. It wrote the edit —
  it doesn't need the diff back. It asked to read a file — it needs the
  content, not a pretty summary.
- **The human** wants rich, visual feedback. Diffs, syntax highlighting,
  progress bars, token counters.

Conflating these wastes tokens. The edit tool used to return the full unified
diff as the tool result (500+ tokens). Now it returns `"Edited foo.rs"` (3
tokens) and puts the diff in `details` for the TUI. Over a 10-turn session,
this saves 3000+ tokens of context that the model never references.

The three-channel model:
- `content` → LLM (terse, minimal)
- `details.display` → TUI (compact summary)
- `details` → metadata (diffs, exit codes, for rendering)

## System prompts are the highest-leverage optimization

Eval data consistently shows that prompt wording changes have more impact
than code optimizations:

| Change | Token savings |
|---|---|
| "Don't narrate" | -30% output tokens per session |
| "Read files directly, don't find/ls first" | -2 turns per task |
| "Use python3 not python" | -1 wasted turn per Python task |
| "All tools run from project root, never cd" | -50 tokens per bash call |
| "Use the read tool's offset/limit, not bash+sed" | -1 subprocess per read |

Per-model prompts matter. Haiku ignores nuanced guidelines but follows
numbered rules. Sonnet follows nuance. `~/.nerv/prompts/{model_id}.md`
lets you tune per model.

## Measure before optimizing

The eval harness (`eval/run.py`) revealed that:
- Models waste 40% of turns on exploration when the prompt could say "read
  directly"
- The `python` vs `python3` distinction causes a wasted turn on every task
- Narration costs more output tokens than tool execution
- Diff output sent to the LLM was the single largest waste

Without per-turn token data, we'd have optimized allocations (microseconds)
instead of prompts (thousands of tokens).

Key metrics to track: turns (fewer = more efficient), tool call errors
(wasted round-trips), output tokens (narration waste), input token delta
per turn (marginal cost, not cumulative context).

## Structure compounds

`bootstrap.rs` (106 lines) eliminated tool setup duplication between
interactive and headless modes. This one extraction made it trivial to
build: the eval harness, the print mode, and the `--model` flag. It also
means adding a new tool is a 1-line change, not a 2-file edit.

The `ToolResult::ok()` / `::error()` constructors eliminated 50 manual
struct constructions. Each one was a potential `is_error: false` on an
error path. The constructors make the intent unambiguous.

Moving HTML export (257 lines of CSS/JS templates) out of `agent_session.rs`
reduced it from 1117 to 860 lines. The session module now only does session
orchestration. The export module can evolve independently.

## Tests as specification

Shared test helpers (`tests/helpers/mod.rs`) serve double duty:
- **Test infrastructure**: `MockProvider`, `EchoTool`, `mock_session()`
  make it easy to write new tests.
- **Interface specification**: reading the helpers tells you exactly how
  to construct an agent, what a provider looks like, and what response
  events to expect.

The allocation tracking tests (`tests/tool_allocs.rs`) use a thread-local
counting allocator that catches regressions quantitatively. The LF-vs-CRLF
test confirmed that `Cow<str>` optimization actually works — not by
reasoning about the code, but by measuring.

## Eval task design

Tests that reveal the algorithm in their assertions are too easy. The
oracle-grid eval (`.pyc` oracle with hidden rules) forced Haiku to do
actual science: hypothesize Game of Life → implement → test → fail →
probe oracle → discover non-standard birth rule → fix → pass. 10 turns
of genuine reasoning.

on_fail hints should be vague like a real human: "still broken, run the
tests" — not "change `=` to `+=` on line 14". The model should do the
work of diagnosis, not the hint.

Efficiency goals (max_edit_calls, max_turns, min_multi_edits) per task
let you compare models on *how* they solve, not just *if* they solve.
Sonnet hit 6/6 goals. Haiku passed all tasks but missed turns/edit goals.
