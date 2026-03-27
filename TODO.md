## General

- [x] SQLite session storage
- [x] Cancel propagation to local servers
- [x] Local model management (nerv add/load/models)
- [x] Memory tool
- [ ] Permission system — per-tool approval (allow/deny/always-allow)
- [x] Print mode — `echo "fix" | nerv --print` headless JSON output
- [ ] Image input — paste/drag screenshots
- [ ] MCP server support — external tool providers
- [ ] Session tree browser (`/tree`) — in progress
  - [ ] v2: node folding (collapse subtrees)
  - [ ] v2: filter modes (user-only, no-tools, labeled-only)
  - [ ] v2: search within tree
  - [ ] v2: label editing from tree view
  - [ ] v2: branch summary on fork (LLM-generated context for abandoned path)
- [ ] Session naming (`/name`)
- [ ] Session search (`/search <query>`)

## Optimizations

Done:
- [x] tiktoken token counting (replaces chars/4)
- [x] Token-budget-aware compaction cut point
- [x] Stale tool result truncation (30-50% context savings)
- [x] 379 → ~100 transitive crates (no tokio, reqwest, crossterm, tracing)
- [x] Zero-dep syntax highlighter (302 lines, 8 languages)
- [x] SQLite sessions (12µs listing, was 1.15s)
- [x] Vec<u8> render buffer with SGR tracking + intra-line diffing
- [x] ANSI-safe prefix splitting
- [x] Event-per-render (no batching, ChatWriter makes it cheap)

Planned (implementation order):
- [x] 1. Superseded read dedup — replace earlier read results when same file read again
- [x] 2. Grep context lines — add -C3 to rg so model doesn't need follow-up reads
- [x] 3. Success-pattern bash truncation — cargo check ok → 1 line, cargo test summary only
- [x] 4. Read auto-size — return whole file if < 300 lines, skip chunked reading
- [x] 5. Collapse edit cycles — strip bulky edit/write args (old_text, new_text) from stale turns
- [x] 6. System prompt batch guidance — "read all files first, then edit, then verify"
- [x] 7. Tool result caching — skip re-read if file mtime unchanged since last read
- [x] 8. Context budget injection — append [Context: ~Nk/Mk tokens, T turns] to system prompt

## Shell Hooks

Executable scripts in `~/.nerv/hooks/`. Any language. Context via env vars + stdin JSON.

Slots: `before_tool_call`, `after_tool_call`, `before_prompt`, `after_response`, `on_start`, `on_exit`

- Non-zero exit from `before_*` cancels the action
- Hooks timeout after 10s
