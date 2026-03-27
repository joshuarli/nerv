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
    timeout: int = 120,
    model: str | None = None,
) -> tuple[dict, str, str]:
    """Run nerv in print mode. Returns (parsed_json, stdout, stderr)."""
    cmd = [str(binary), "--print", "--max-turns", str(max_turns)]
    if model:
        cmd.extend(["--model", model])
    result = subprocess.run(
        cmd,
        input=prompt,
        capture_output=True,
        text=True,
        cwd=work_dir,
        timeout=timeout,
    )

    stdout = result.stdout
    stderr = result.stderr

    if result.returncode != 0 and not stdout.strip():
        return {"error": stderr.strip() or f"exit code {result.returncode}"}, stdout, stderr

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
                 nerv_output: dict, nerv_stdout: str, nerv_stderr: str,
                 passed: bool, verify_output: str, result: EvalResult):
    """Write detailed task report."""
    task_dir = report_dir / task_name
    task_dir.mkdir(parents=True, exist_ok=True)

    # Full nerv JSON output
    with open(task_dir / "nerv_output.json", "w") as f:
        json.dump(nerv_output, f, indent=2)

    # Raw stdout/stderr
    if nerv_stderr.strip():
        with open(task_dir / "nerv_stderr.txt", "w") as f:
            f.write(nerv_stderr)

    # Verification output
    with open(task_dir / "verify_output.txt", "w") as f:
        f.write(verify_output)

    # Summary
    summary = {
        "task": task_name,
        "passed": passed,
        "prompt": config["prompt"],
        "verify_command": config["verify"],
        **asdict(result),
    }
    with open(task_dir / "summary.json", "w") as f:
        json.dump(summary, f, indent=2)


def run_task(task_dir: Path, binary: Path, report_dir: Path,
             model: str | None = None) -> EvalResult:
    task_name = task_dir.name
    config = load_task(task_dir)

    with tempfile.TemporaryDirectory(prefix="nerv-eval-") as tmpdir:
        work_dir = Path(tmpdir)
        setup_workdir(task_dir, work_dir)

        start = time.monotonic()
        nerv_output, nerv_stdout, nerv_stderr = run_nerv(
            binary,
            config["prompt"],
            work_dir,
            max_turns=config.get("max_turns", 20),
            model=model,
        )
        wall_time = time.monotonic() - start

        if "error" in nerv_output:
            result = EvalResult(
                task=task_name,
                passed=False,
                wall_time_s=round(wall_time, 2),
                error=nerv_output["error"],
            )
            write_report(report_dir, task_name, config,
                         nerv_output, nerv_stdout, nerv_stderr,
                         False, "", result)
            return result

        # Extract metrics
        metrics = nerv_output.get("metrics", {})
        tool_calls = [
            ToolCall(name=tc["name"], is_error=tc.get("is_error", False))
            for tc in metrics.get("tool_calls", [])
        ]

        passed, verify_output = verify(
            config["verify"],
            work_dir,
            config.get("expected_exit", 0),
        )

        result = EvalResult(
            task=task_name,
            passed=passed,
            turns=metrics.get("turns", 0),
            tool_calls=tool_calls,
            total_tool_calls=len(tool_calls),
            tokens_in=metrics.get("tokens_in", 0),
            tokens_out=metrics.get("tokens_out", 0),
            tokens_cache_read=metrics.get("tokens_cache_read", 0),
            cost=metrics.get("cost", 0.0),
            wall_time_s=round(wall_time, 2),
        )

        write_report(report_dir, task_name, config,
                     nerv_output, nerv_stdout, nerv_stderr,
                     passed, verify_output, result)

        return result


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
    report_dir = REPORT_DIR / f"{timestamp}_{model_tag}"
    report_dir.mkdir(parents=True, exist_ok=True)

    results: list[EvalResult] = []

    for task_dir in task_dirs:
        if not json_output:
            print(f"  {task_dir.name} ... ", end="", flush=True, file=sys.stderr)

        result = run_task(task_dir, binary, report_dir, model=model)

        if not json_output:
            if result.passed:
                print(
                    f"PASS  ({result.turns} turns, {result.total_tool_calls} tools, "
                    f"{result.tokens_in}+{result.tokens_out} tok, "
                    f"{result.wall_time_s}s)",
                    file=sys.stderr,
                )
            else:
                err = result.error or "verification failed"
                print(f"FAIL  {err}", file=sys.stderr)

        results.append(result)

    # Write overall results
    with open(report_dir / "results.json", "w") as f:
        json.dump([asdict(r) for r in results], f, indent=2)

    if json_output:
        print(json.dumps([asdict(r) for r in results], indent=2))
    else:
        passed = sum(1 for r in results if r.passed)
        total = len(results)
        total_tokens = sum(r.tokens_in + r.tokens_out for r in results)
        total_cost = sum(r.cost for r in results)
        total_tools = sum(r.total_tool_calls for r in results)
        print(file=sys.stderr)
        print(
            f"  {passed}/{total} passed | {total_tools} tool calls | "
            f"{total_tokens} tokens | ${total_cost:.4f}",
            file=sys.stderr,
        )
        print(f"  report: {report_dir}", file=sys.stderr)


if __name__ == "__main__":
    main()
