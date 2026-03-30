---
name: commit
description: Create a commit
---

Create a commit efficiently:

**First assess:** `git status && git diff --cached --stat && git log --oneline -3`

**Then stage and commit:** `git add <specific files> && git commit -m` with a Conventional Commits subject (`feat:`, `fix:`, `ref:`, `chore:`, etc.), under 72 chars, imperative mood. Add a body when the change needs explanation — what changed and why, not a rehash of the diff.

Never use `--no-verify` or `--amend`.
Never `git add -A`.
