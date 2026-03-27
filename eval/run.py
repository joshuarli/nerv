#!/usr/bin/env python3
"""
Eval harness — drives nerv headlessly against coding tasks.

Usage:
    python3 eval/run.py [--task <name>] [--all] [--json] [--binary <path>]

Tasks live in eval/tasks/<name>/ with:
    repo/       git repo fixture (copied to tmpdir)
    setup.sh    optional setup script (run in tmpdir before prompt)
    task.json   {"prompt": "...", "verify": "cargo test", "max_turns": 20}
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from dataclasses import asdict, dataclass, field
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


def run_nerv(binary: Path, prompt: str, work_dir: Path, max_turns: int = 20, timeout: int = 120) -> dict:
    """Run nerv in print mode, return parsed JSON output."""
    result = subprocess.run(
        [str(binary), "--print", "--max-turns", str(max_turns), "--json"],
        input=prompt,
        capture_output=True,
        text=True,
        cwd=work_dir,
        timeout=timeout,
    )

    if result.returncode != 0 and not result.stdout.strip():
        return {"error": result.stderr.strip() or f"exit code {result.returncode}"}

    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError:
        return {"error": f"invalid JSON output: {result.stdout[:200]}"}


def verify(command: str, work_dir: Path, expected_exit: int = 0) -> bool:
    """Run verification command and check exit code."""
    try:
        result = subprocess.run(
            ["/bin/bash", "-c", command],
            cwd=work_dir,
            capture_output=True,
            timeout=30,
        )
        return result.returncode == expected_exit
    except subprocess.TimeoutExpired:
        return False


def run_task(task_dir: Path, binary: Path) -> EvalResult:
    task_name = task_dir.name
    config = load_task(task_dir)

    with tempfile.TemporaryDirectory(prefix="nerv-eval-") as tmpdir:
        work_dir = Path(tmpdir)
        setup_workdir(task_dir, work_dir)

        start = time.monotonic()
        output = run_nerv(
            binary,
            config["prompt"],
            work_dir,
            max_turns=config.get("max_turns", 20),
        )
        wall_time = time.monotonic() - start

        if "error" in output:
            return EvalResult(
                task=task_name,
                passed=False,
                wall_time_s=wall_time,
                error=output["error"],
            )

        # Extract metrics from nerv JSON output
        metrics = output.get("metrics", {})
        tool_calls = [
            ToolCall(name=tc["name"], is_error=tc.get("is_error", False))
            for tc in metrics.get("tool_calls", [])
        ]

        passed = verify(
            config["verify"],
            work_dir,
            config.get("expected_exit", 0),
        )

        return EvalResult(
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


def main():
    args = sys.argv[1:]
    json_output = "--json" in args
    binary = NERV_BINARY

    if "--binary" in args:
        idx = args.index("--binary")
        binary = Path(args[idx + 1])

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

    results: list[EvalResult] = []

    for task_dir in task_dirs:
        if not json_output:
            print(f"  {task_dir.name} ... ", end="", flush=True, file=sys.stderr)

        result = run_task(task_dir, binary)

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


if __name__ == "__main__":
    main()
