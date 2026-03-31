#!/usr/bin/env python3
"""
Launch nerv interactively against an eval task's repo fixture.

Usage:
    python3 eval/run-interactive.py <task-dir> [--model <name>] [--binary <path>] [--plan]

Examples:
    python3 eval/run-interactive.py eval/interactive-tasks/plan-mode-todo-app
    python3 eval/run-interactive.py eval/interactive-tasks/plan-mode-todo-app --plan
    python3 eval/run-interactive.py eval/interactive-tasks/plan-mode-todo-app --model claude-sonnet-4-6

The task directory must contain:
    repo/       git repo fixture (cloned to a tmpdir)
    task.json   {"prompt": "...", "description": "..."}

--plan  prepends "Let's plan out" if not already present in the prompt, and
        passes --plan-mode to nerv so plan mode is active from the first turn.

The tmpdir is left on disk so you can inspect results; its path is printed.
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

NERV_BINARY = Path(__file__).parent.parent / "target" / "debug" / "nerv"


def die(msg: str) -> None:
    print(f"error: {msg}", file=sys.stderr)
    sys.exit(1)


def main() -> None:
    args = sys.argv[1:]
    if not args or args[0].startswith("-"):
        die("usage: run-interactive.py <task-dir> [--model M] [--binary PATH] [--plan]")

    task_dir = Path(args[0])
    args = args[1:]

    binary = NERV_BINARY
    model: str = "claude-sonnet-4-6"
    force_plan = False

    i = 0
    while i < len(args):
        if args[i] == "--binary" and i + 1 < len(args):
            binary = Path(args[i + 1])
            i += 2
        elif args[i] == "--model" and i + 1 < len(args):
            model = args[i + 1]
            i += 2
        elif args[i] == "--plan":
            force_plan = True
            i += 1
        else:
            die(f"unknown argument: {args[i]}")

    if not task_dir.is_dir():
        die(f"task directory not found: {task_dir}")

    task_json = task_dir / "task.json"
    if not task_json.exists():
        die(f"task.json not found in {task_dir}")

    repo_src = task_dir / "repo"
    if not repo_src.is_dir():
        die(f"repo/ not found in {task_dir}")

    if not binary.exists():
        die(f"nerv binary not found at {binary} — run `cargo build` first")

    task = json.loads(task_json.read_text())
    prompt: str = task["prompt"]
    description: str = task.get("description", "")

    if description:
        print(f"\n  {description}\n")

    # Copy the fixture into a fresh tmpdir and init a git repo so nerv has a proper root.
    tmpdir = Path(tempfile.mkdtemp(prefix="nerv-eval-"))
    repo_dst = tmpdir / "repo"
    shutil.copytree(repo_src, repo_dst)
    subprocess.run(["git", "init", "-q"], cwd=repo_dst, check=True)
    subprocess.run(["git", "add", "."], cwd=repo_dst, check=True)
    subprocess.run(
        ["git", "commit", "-q", "-m", "initial", "--no-verify"],
        cwd=repo_dst, check=True,
    )

    print(f"  repo:   {repo_dst}")
    print(f"  prompt: {prompt}\n")

    cmd = [str(binary), "--prompt", prompt, "--model", model]
    if force_plan:
        cmd += ["--plan-mode"]

    try:
        subprocess.run(cmd, cwd=repo_dst, check=False)
    except KeyboardInterrupt:
        pass

    print(f"\n  session left in: {tmpdir}")


if __name__ == "__main__":
    main()
