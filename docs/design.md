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
| "Read tool returns full file; use sed for ranges" | eliminated chunked-read storm |

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

## The read tool simplification (a war story)

The read tool originally had `offset` and `limit` parameters so the model
could request specific line ranges. This caused a cascade of problems that
took multiple rounds of optimization to diagnose — and the fix was to delete
the feature entirely.

**The problem.** Sonnet developed a habit of reading files in 120-line chunks.
A 1180-line file became 10 sequential reads, each a separate API call. Each
call resent the growing conversation (~20k+ tokens). A single file read that
should have cost one API call cost ten, ballooning total input tokens from
~20k to ~200k. Session 99f63ffb showed 55 API calls and $3.67 for what
should have been a $0.30 task.

**The optimization spiral.** We tried to fix this with increasingly complex
heuristics:
1. *Auto-size* (300-line threshold): return the full file if it's small
   enough, ignoring the model's limit. But 300 was too low for most source
   files, so we raised it to 2000.
2. *Auto-size with offset override*: ignore offset too for files under the
   threshold. But this broke targeted reads — the model asked for 68 lines
   and got 1200.
3. *Mtime cache with auto-size interaction*: cache hits needed to work for
   offset reads of auto-sized files. Edge cases multiplied.
4. *System prompt guidance*: "Read whole files by default, don't pass
   offset or limit unless the file is 2000+ lines." The model still passed
   `limit=120` out of habit.

Each fix introduced new edge cases. Auto-size at 2000 lines meant a
targeted `sed -n '828,895p'` through bash was more efficient than using
the read tool — the tool would return 1200 lines when the model wanted 68.
We were about to add bash command rejection (detect sed/cat and force the
model through the read tool) when we realized that would cause *worse*
context bloat.

**The insight.** The offset/limit API was solving a problem that bash
already solves elegantly. `sed -n '100,200p' file` is natural, precise,
and self-documenting. The model already knows sed. There's nothing to
learn, no threshold to tune, no interaction bugs.

The read tool's actual value was never line-range selection — it was the
mtime cache (`[unchanged since last read]` saves 10k+ tokens on redundant
re-reads) and consistent line-number formatting. Those features don't need
offset or limit.

**The fix.** Remove offset and limit entirely. The read tool does one thing:
return the entire file with line numbers, cached by mtime. For specific
ranges, the model uses bash + sed. The tool went from 421 lines to 215.
The system prompt went from a paragraph of read-tool guidance to one line:
"The read tool returns the entire file. For specific line ranges, use
bash: `sed -n '100,200p' file`."

**The lesson.** When you're building heuristics to compensate for an API
the model doesn't want to use correctly, consider whether the API is wrong.
The model's preference for bash wasn't a compliance problem to fix with
prompt engineering — it was signal that the tool's abstraction didn't match
how the model thinks about file reading.
