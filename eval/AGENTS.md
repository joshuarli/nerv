# Eval System

Coding evals that measure nerv's ability to drive LLMs on realistic tasks.

## Running evals

```bash
# All tasks with a specific model
python3 eval/run.py --model claude-sonnet-4-6

# Single task
python3 eval/run.py --model claude-haiku-4-5 --task fix-off-by-one

# Custom binary
python3 eval/run.py --binary ./target/release/nerv --model sonnet

# JSON output (for scripting)
python3 eval/run.py --model haiku --json
```

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
eval/reports/20260326-213915_claude-sonnet-4-6_6-tasks/
  results.json                    # Array of all task results
  fix-off-by-one/
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
python3 eval/run.py --model haiku --task fix-off-by-one

# Read the trace
cat eval/reports/*/fix-off-by-one/nerv_output.json | python3 -m json.tool

# Check what verify saw
cat eval/reports/*/fix-off-by-one/verify_output.txt

# Check nerv stderr (model selection, errors)
cat eval/reports/*/fix-off-by-one/nerv_stderr.txt

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
