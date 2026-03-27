---
name: commit
description: Create a well-formed git commit from staged/unstaged changes
---

Review the current git state and create a commit:

1. Run `git status` to see all changes (staged and unstaged).
2. Run `git diff` to see unstaged changes and `git diff --cached` to see staged changes.
3. Run `git log --oneline -5` to understand the recent commit style.
4. Stage appropriate files with `git add` (prefer specific files over `git add -A`).
5. Write a concise commit message:
   - First line: imperative mood, under 72 chars, summarizes the "why"
   - If needed, blank line then details
   - Do not include file lists — the diff speaks for itself
6. Commit with `git commit -m "message"`.
7. Run `git status` to verify the commit succeeded.

Rules:
- Never use `--no-verify` or skip hooks.
- Never use `--amend` unless explicitly asked.
- Never push to remote unless explicitly asked.
- Do not commit .env, credentials, or secrets.
- If pre-commit hooks fail, fix the issue and create a new commit.
