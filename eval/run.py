#!/usr/bin/env python3
"""
Eval harness — drives nerv headlessly against coding tasks.

Usage:
    python3 eval/run.py [--task <name>] [--model <name>] [--binary <path>] [--json]

Tasks live in eval/tasks/<name>/ with:
    repo/       git repo fixture (copied to tmpdir)
    setup.sh    optional setup script (run in tmpdir before prompt)
    task.json   {"prompt": "...", "verify": "cargo test", "max_turns": 20}

Reports are written to eval/reports/<timestamp>/.
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from dataclasses import asdict, dataclass, field
from datetime import datetime
from pathlib import Path


@dataclass
class ToolCall:
    name: str
    is_error: bool


@dataclass
class EvalResult:
    task: str
    passed: bool
    turns: int = 0
    tool_calls: list[ToolCall] = field(default_factory=list)
    total_tool_calls: int = 0
    tokens_in: int = 0
    tokens_out: int = 0
    tokens_cache_read: int = 0
    cost: float = 0.0
    wall_time_s: float = 0.0
    error: str | None = None
    goal_results: list[str] = field(default_factory=list)


EVAL_DIR = Path(__file__).parent / "tasks"
REPORT_DIR = Path(__file__).parent / "reports"
NERV_BINARY = Path(__file__).parent.parent / "target" / "debug" / "nerv"


def load_task(task_dir: Path) -> dict:
    with open(task_dir / "task.json") as f:
        return json.load(f)


def setup_workdir(task_dir: Path, work_dir: Path):
    """Copy repo/ into work_dir and run setup.sh if present."""
    repo = task_dir / "repo"
    if repo.exists():
        shutil.copytree(repo, work_dir, dirs_exist_ok=True)

    setup = task_dir / "setup.sh"
    if setup.exists():
        subprocess.run(
            ["/bin/bash", str(setup)],
            cwd=work_dir,
            capture_output=True,
        )


def run_nerv(
    binary: Path,
    prompt: str,
    work_dir: Path,
    max_turns: int = 20,
    timeout: int = 300,
    model: str | None = None,
    stream: bool = False,
) -> tuple[dict, str, str]:
    """Run nerv in print mode. Returns (parsed_json, stdout, stderr).

    nerv handles SIGINT gracefully (sets cancel flag, finishes current turn,
    flushes JSON). On ^C we wait up to 30s for it to finish. Second ^C kills.
    """
    cmd = [str(binary), "--print", "--max-turns", str(max_turns)]
    if model:
        cmd.extend(["--model", model])
    if stream:
        cmd.append("--verbose")

    proc = subprocess.Popen(
        cmd,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=None if stream else subprocess.PIPE,
        text=True,
        cwd=work_dir,
    )

    try:
        stdout, stderr = proc.communicate(input=prompt, timeout=timeout)
    except subprocess.TimeoutExpired:
        proc.kill()
        stdout, stderr = proc.communicate()
        return {"error": f"timeout after {timeout}s"}, stdout or "", stderr or ""
    except KeyboardInterrupt:
        # First ^C: nerv got SIGINT too and is flushing. Wait for it.
        try:
            stdout, stderr = proc.communicate(timeout=30)
        except (subprocess.TimeoutExpired, KeyboardInterrupt):
            # Second ^C or too slow: force kill.
            proc.kill()
            stdout, stderr = proc.communicate()
            return {"error": "interrupted"}, stdout or "", stderr or ""

    stderr = stderr or ""

    if proc.returncode != 0 and not stdout.strip():
        return {"error": stderr.strip() or f"exit code {proc.returncode}"}, stdout, stderr

    try:
        return json.loads(stdout), stdout, stderr
    except json.JSONDecodeError:
        return {"error": f"invalid JSON output: {stdout[:200]}"}, stdout, stderr


def verify(command: str, work_dir: Path, expected_exit: int = 0) -> tuple[bool, str]:
    """Run verification command. Returns (passed, output)."""
    try:
        result = subprocess.run(
            ["/bin/bash", "-c", command],
            cwd=work_dir,
            capture_output=True,
            text=True,
            timeout=30,
        )
        output = result.stdout
        if result.stderr:
            output += "\n[stderr]\n" + result.stderr
        return result.returncode == expected_exit, output
    except subprocess.TimeoutExpired:
        return False, "verification timed out"


def write_report(report_dir: Path, task_name: str, config: dict,
                 nerv_output: dict, nerv_stderr: str, _nerv_stdout: str,
                 passed: bool, verify_output: str, result: EvalResult):
    """Write detailed task report."""
    task_dir = report_dir / task_name
    task_dir.mkdir(parents=True, exist_ok=True)

    with open(task_dir / "nerv_output.json", "w") as f:
        json.dump(nerv_output, f, indent=2)

    if nerv_stderr.strip():
        with open(task_dir / "nerv_stderr.txt", "w") as f:
            f.write(nerv_stderr)

    if verify_output:
        with open(task_dir / "verify_output.txt", "w") as f:
            f.write(verify_output)

    summary = {
        "task": task_name,
        "passed": passed,
        "prompt": config["prompt"],
        "verify_command": config["verify"],
        **asdict(result),
    }
    with open(task_dir / "summary.json", "w") as f:
        json.dump(summary, f, indent=2)


def extract_metrics(nerv_output: dict) -> tuple[list[ToolCall], dict]:
    """Extract tool calls and raw metrics dict from nerv output."""
    metrics = nerv_output.get("metrics", {})
    tool_calls = [
        ToolCall(name=tc["name"], is_error=tc.get("is_error", False))
        for tc in metrics.get("tool_calls", [])
    ]
    return tool_calls, metrics


def run_task(task_dir: Path, binary: Path, report_dir: Path,
             model: str | None = None, stream: bool = False) -> EvalResult:
    task_name = task_dir.name
    config = load_task(task_dir)

    with tempfile.TemporaryDirectory(prefix="nerv-eval-") as tmpdir:
        work_dir = Path(tmpdir)
        setup_workdir(task_dir, work_dir)

        start = time.monotonic()
        error_msg = None

        try:
            nerv_output, nerv_stdout, nerv_stderr = run_nerv(
                binary,
                config["prompt"],
                work_dir,
                max_turns=config.get("max_turns", 20),
                model=model,
                stream=stream,
            )
        except KeyboardInterrupt:
            nerv_output = {"error": "interrupted"}
            nerv_stderr = ""

        if "error" in nerv_output:
            error_msg = nerv_output["error"]
            tool_calls, metrics = [], {}
        else:
            tool_calls, metrics = extract_metrics(nerv_output)

        if error_msg:
            passed, verify_output = False, ""
        else:
            passed, verify_output = verify(
                config["verify"],
                work_dir,
                config.get("expected_exit", 0),
            )

        wall_time = time.monotonic() - start

        result = EvalResult(
            task=task_name,
            passed=passed,
            turns=metrics.get("turns", 0),
            tool_calls=tool_calls,
            total_tool_calls=len(tool_calls),
            tokens_in=metrics.get("tokens_in", 0),
            tokens_out=metrics.get("tokens_out", 0),
            tokens_cache_read=metrics.get("tokens_cache_read", 0),
            cost=round(metrics.get("cost", 0.0), 6),
            wall_time_s=round(wall_time, 2),
            error=error_msg,
        )

        goals = config.get("goals")
        if goals and passed:
            result.goal_results = check_goals(goals, nerv_output, result)

        write_report(report_dir, task_name, config,
                     nerv_output, nerv_stderr or "", "",
                     passed, verify_output, result)

        return result


def extract_tool_sequence(output: dict) -> list[dict]:
    """Extract ordered list of {tool, args} from trace messages."""
    sequence = []
    for msg in output.get("trace", []):
        if msg.get("role") != "assistant":
            continue
        for tc in msg.get("tool_calls", []):
            sequence.append({"tool": tc.get("tool", ""), "args": tc.get("args", {})})
    return sequence


def check_goals(goals: dict, output: dict, result: EvalResult) -> list[str]:
    """Check efficiency goals against the trace. Returns list of pass/fail strings."""
    results = []
    tool_seq = extract_tool_sequence(output)

    # Count edit calls and multi-edits
    edit_calls = sum(1 for t in tool_seq if t["tool"] == "edit")
    multi_edits = sum(1 for t in tool_seq if t["tool"] == "edit" and "edits" in t["args"])

    if "max_edit_calls" in goals:
        limit = goals["max_edit_calls"]
        ok = edit_calls <= limit
        results.append(f"{'GOAL' if ok else 'MISS'} edit calls: {edit_calls} (goal: <={limit})")

    if "min_multi_edits" in goals:
        minimum = goals["min_multi_edits"]
        ok = multi_edits >= minimum
        results.append(f"{'GOAL' if ok else 'MISS'} multi-edits: {multi_edits} (goal: >={minimum})")

    if "max_turns" in goals:
        limit = goals["max_turns"]
        ok = result.turns <= limit
        results.append(f"{'GOAL' if ok else 'MISS'} turns: {result.turns} (goal: <={limit})")

    # Tool ordering: require certain tools before any `read` call
    if "require_before_read" in goals:
        required = set(goals["require_before_read"])
        first_read = next((i for i, t in enumerate(tool_seq) if t["tool"] == "read"), None)
        first_required = next(
            (i for i, t in enumerate(tool_seq) if t["tool"] in required), None
        )
        if first_required is None:
            tools_used = [t["tool"] for t in tool_seq]
            results.append(f"MISS require_before_read: never used {required} (tools: {tools_used})")
        elif first_read is not None and first_required > first_read:
            results.append(
                f"MISS require_before_read: {tool_seq[first_required]['tool']} at position "
                f"{first_required}, but read at {first_read}"
            )
        else:
            results.append(f"GOAL require_before_read: {tool_seq[first_required]['tool']} at position {first_required}")

    # Forbid bash for searching (grep, rg, find, fd, cat, head, tail)
    if "forbid_bash_search" in goals and goals["forbid_bash_search"]:
        search_cmds = {"grep", "rg", "find", "fd", "cat", "head", "tail", "awk", "sed"}
        violations = []
        for t in tool_seq:
            if t["tool"] != "bash":
                continue
            cmd = t["args"].get("command", "")
            words = cmd.replace("|", " ").replace(";", " ").replace("&&", " ").split()
            for w in words:
                base = w.split("/")[-1]
                if base in search_cmds:
                    violations.append(cmd.strip()[:80])
                    break
        if violations:
            results.append(f"MISS forbid_bash_search: {len(violations)} violation(s)")
            for v in violations[:3]:
                results.append(f"  bash: {v}")
        else:
            results.append("GOAL forbid_bash_search: no bash search commands")

    # Require specific tools were used at least once
    if "require_tools" in goals:
        for tool_name in goals["require_tools"]:
            used = any(t["tool"] == tool_name for t in tool_seq)
            results.append(
                f"{'GOAL' if used else 'MISS'} require_tools: {tool_name} {'used' if used else 'never used'}"
            )

    return results


def main():
    args = sys.argv[1:]
    json_output = "--json" in args
    binary = NERV_BINARY

    model = None
    if "--binary" in args:
        idx = args.index("--binary")
        binary = Path(args[idx + 1])
    if "--model" in args:
        idx = args.index("--model")
        model = args[idx + 1]
    stream = "--stream" in args

    if not binary.exists():
        print(f"nerv binary not found at {binary}", file=sys.stderr)
        print("Run: cargo build", file=sys.stderr)
        sys.exit(1)

    # Collect tasks
    if not EVAL_DIR.exists():
        print(f"No eval tasks at {EVAL_DIR}", file=sys.stderr)
        sys.exit(1)

    task_dirs = sorted(
        p for p in EVAL_DIR.iterdir()
        if p.is_dir() and (p / "task.json").exists()
    )

    if "--task" in args:
        idx = args.index("--task")
        name = args[idx + 1]
        task_dirs = [p for p in task_dirs if name in p.name]

    if not task_dirs:
        print("No matching tasks found.", file=sys.stderr)
        sys.exit(1)

    # Create report directory
    timestamp = datetime.now().strftime("%Y%m%d-%H%M%S")
    model_tag = model or "default"
    task_tag = task_dirs[0].name if len(task_dirs) == 1 else f"{len(task_dirs)}-tasks"
    report_dir = REPORT_DIR / f"{timestamp}_{model_tag}_{task_tag}"
    report_dir.mkdir(parents=True, exist_ok=True)

    results: list[EvalResult] = []

    def flush_results():
        """Write whatever results we have so far."""
        with open(report_dir / "results.json", "w") as f:
            json.dump([asdict(r) for r in results], f, indent=2)

    try:
        for i, task_dir in enumerate(task_dirs):
            # Brief pause between tasks to avoid hammering the API when running
            # multiple tasks in sequence (the agent handles per-request retries,
            # but inter-task bursts can still trigger 529 overloaded responses).
            if i > 0:
                time.sleep(3)

            if not json_output:
                print(f"  {task_dir.name} ... ", end="", flush=True, file=sys.stderr)

            result = run_task(task_dir, binary, report_dir, model=model, stream=stream)

            if not json_output:
                if result.passed:
                    ctx = result.tokens_in + result.tokens_cache_read
                    print(
                        f"PASS  ({result.turns} turns, {result.total_tool_calls} tools, "
                        f"{ctx}+{result.tokens_out} tok, "
                        f"${result.cost:.4f}, {result.wall_time_s}s)",
                        file=sys.stderr,
                    )
                    for g in result.goal_results:
                        print(f"    {g}", file=sys.stderr)
                else:
                    err = result.error or "verification failed"
                    print(f"FAIL  {err}", file=sys.stderr)

            results.append(result)
            flush_results()

            # Stop running more tasks if this one was interrupted
            if result.error == "interrupted":
                break

    except KeyboardInterrupt:
        if not json_output:
            print("\n  interrupted", file=sys.stderr)
        flush_results()

    if json_output:
        print(json.dumps([asdict(r) for r in results], indent=2))
    else:
        # Summary
        passed = sum(1 for r in results if r.passed)
        total = len(results)
        total_tokens = sum(r.tokens_in + r.tokens_cache_read + r.tokens_out for r in results)
        total_cost = sum(r.cost for r in results)
        total_turns = sum(r.turns for r in results)
        total_tools = sum(r.total_tool_calls for r in results)
        goals_met = sum(1 for r in results for g in r.goal_results if g.startswith("GOAL"))
        goals_total = sum(len(r.goal_results) for r in results)

        print(file=sys.stderr)
        print(f"  {'─' * 60}", file=sys.stderr)
        print(f"  {passed}/{total} passed | {total_turns} turns | {total_tools} tools | "
              f"{total_tokens} tok | ${total_cost:.4f}",
              file=sys.stderr)
        if goals_total > 0:
            print(f"  goals: {goals_met}/{goals_total}", file=sys.stderr)
        print(f"  report: {report_dir}", file=sys.stderr)


if __name__ == "__main__":
    main()
