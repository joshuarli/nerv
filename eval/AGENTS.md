# Eval System

Coding evals that measure nerv's ability to drive LLMs on realistic tasks.

**NEVER run evals automatically.** Each eval run costs real money (API calls).
Only a human should invoke `eval/run.py`. Do not run it in scripts, hooks,
CI, or as part of any automated workflow.

## Running evals

```bash
# All tasks with a specific model
python3 eval/run.py --model claude-sonnet-4-6

# Single task
python3 eval/run.py --model claude-haiku-4-5 --task fix-multiple-bugs

# Custom binary
python3 eval/run.py --binary ./target/release/nerv --model sonnet

# JSON output (for scripting)
python3 eval/run.py --model haiku --json
```

## Current tasks

### fix-multiple-bugs

A task scheduler with **4 independent bugs**: wrong return value in
`is_overdue()`, sort order in `next_task()`, missing counter increment in
`bulk_add()`, and a comparison issue. Tests each bug individually.

**What this tests**: Can the model find and fix multiple unrelated bugs
in one pass? The ideal solution uses a single multi-edit call with 4
replacements. Weaker models fix them one at a time (4 separate edit calls,
4 verify cycles). Measures multi-edit adoption and diagnostic breadth.

### extract-constants

An HTTP response handler with **14 magic number status codes** scattered
across function bodies, plus a magic retry delay cap. Tests assert that
named constants exist at module level AND that bare literals are gone from
functions.

**What this tests**: The model must add ~15 constant definitions at the
top of the file AND replace every occurrence in the function bodies — a
30+ replacement task. The ideal solution is one multi-edit call. This is
the hardest multi-edit stress test: many disjoint replacements that must
all be correct, plus new code that must be added (not just replaced).

### implement-from-tests

A dependency resolver with **4 method stubs** (all raise NotImplementedError)
and **20 tests** that define the complete spec. No documentation, no
comments — the tests are the only source of truth. Requires implementing:
version matching with constraints, transitive dependency resolution,
circular dependency detection, and topological install ordering.

**What this tests**: Raw problem-solving ability under ambiguity. The model
must reverse-engineer the required behavior from test assertions alone,
then implement graph algorithms (DFS, topological sort, cycle detection)
correctly. The ideal solution reads both files, implements the entire
class in one edit, and verifies — 4-5 turns. Weaker models iterate:
implement → test → fail → re-read tests → fix → test → ... burning
10+ turns. This is the hardest task in the suite.

## Task structure

```
eval/tasks/<name>/
  repo/           # Files copied to a tmpdir before the run
  repo/AGENTS.md  # Project context (test commands, file layout)
  task.json       # Task definition
```

### task.json format

```json
{
  "prompt": "The initial user message",
  "verify": "python3 test_foo.py",
  "max_turns": 15,
  "on_fail": "Optional follow-up hint sent if verify fails after first attempt",
  "expected_exit": 0
}
```

When `on_fail` is set and the first attempt fails verification, the harness
sends the hint as a second prompt to the same workdir (files persist). This
tests the model's ability to recover with a vague nudge — hints should be
realistic human messages, not detailed instructions.

## Report structure

Reports are written to `eval/reports/<timestamp>_<model>_<task>/`:

```
eval/reports/20260326-213915_claude-sonnet-4-6_2-tasks/
  results.json                    # Array of all task results
  fix-multiple-bugs/
    nerv_output.json              # Full nerv JSON with trace + metrics
    nerv_stderr.txt               # nerv stderr (model selection, warnings)
    verify_output.txt             # stdout/stderr from verify command
    summary.json                  # Pass/fail + all metrics
```

### nerv_output.json trace format

Each assistant message includes per-turn token usage:

```json
{
  "role": "assistant",
  "text": "optional response text",
  "tool_calls": [{"tool": "edit", "args": {...}}],
  "stop_reason": "ToolUse",
  "usage": {"input": 3845, "output": 331, "cache_read": 3845}
}
```

## Debugging a failing eval

```bash
# Run the task
python3 eval/run.py --model haiku --task fix-multiple-bugs

# Read the trace
cat eval/reports/*/fix-multiple-bugs/nerv_output.json | python3 -m json.tool

# Check what verify saw
cat eval/reports/*/fix-multiple-bugs/verify_output.txt

# Check nerv stderr (model selection, errors)
cat eval/reports/*/fix-multiple-bugs/nerv_stderr.txt

# Run nerv manually with logging to see the full system prompt
echo "fix the bug" | NERV_LOG=info ./target/debug/nerv --print --model haiku 2>/dev/null
# Then check ~/.nerv/debug.log for the full API request
```

## Key metrics

- **turns**: LLM round-trips. Fewer = more efficient.
- **tool_calls**: total tools invoked. Error calls are wasted.
- **tokens_out**: assistant output tokens. Measures narration waste.
- **tokens_in / cache_read**: input tokens. High cache_read = prompt caching working.
- **cost**: dollar cost from token pricing.
- **attempts**: 1 = solved first try, 2 = needed the on_fail hint.

## Design principles

- Tasks use only the Python standard library (no pip install).
- AGENTS.md in each repo tells the model how to run tests.
- The eval uses the real system prompt and real tool implementations.
- Prompts are realistic: "tests are failing, fix it" — not "change line 4".
- on_fail hints are vague like a real human: "still broken, run the tests" — not "change = to +=".
- Tasks are designed to stress specific nerv capabilities (multi-edit, tool efficiency) not just model intelligence.
