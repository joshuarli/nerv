---
name: commit
description: Create a well-formed git commit from staged/unstaged changes
---

Create a commit efficiently:

1. Run `git status && git diff --cached --stat && git log --oneline -3` in ONE bash call to see state, staged changes, and recent style.
2. Stage files: `git add file1 file2 ...` (specific files, not `git add -A`).
3. Commit: `git commit -m "message"` — imperative mood, under 72 chars, summarizes the "why" not the "what".

That's 2-3 bash calls total. Do not run git diff --stat, git status, git log as separate calls.

Rules:
- Never use `--no-verify` or skip hooks.
- Never use `--amend` unless explicitly asked.
- Never push to remote unless explicitly asked.
- Do not commit .env, credentials, or secrets.
- If pre-commit hooks fail, fix the issue and create a new commit.
