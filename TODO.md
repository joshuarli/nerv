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

Planned:
- [ ] Deduplicate reads — keep only latest result per file
- [ ] Collapse edit cycles — read+edit+verify → outcome summary
- [ ] Active file tracking — truncate non-recent file results
- [ ] Semantic compression — tool chains → natural language outcome
- [ ] Tool result caching — skip re-execution if file unchanged

## Agentic Session Observability

- [ ] Read tool: include context lines in grep output to reduce follow-up reads
- [ ] Batch edit heuristic: when grep shows many call sites, read all files upfront before editing
- [ ] Read tool: raise default limit for small files to avoid multi-chunk reading
- [ ] Deduplicate reads — track which files/ranges already read, skip re-reads of same content

## Shell Hooks

Executable scripts in `~/.nerv/hooks/`. Any language. Context via env vars + stdin JSON.

Slots: `before_tool_call`, `after_tool_call`, `before_prompt`, `after_response`, `on_start`, `on_exit`

- Non-zero exit from `before_*` cancels the action
- Hooks timeout after 10s
