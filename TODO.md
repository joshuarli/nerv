## General

## Memory (RSS reduction)

- [ ] **Undo stack cap** (`editor.rs`): `undo_stack: Vec<(Vec<String>, usize, usize)>` grows
      unboundedly — every keystroke pushes a full copy of all editor lines. Cap at 50 entries
      with `truncate`, same as kill ring.

- [ ] **Evict `source` from `FileEntry` after indexing** (`index/mod.rs`): each `FileEntry`
      holds `source: Option<Arc<String>>` (the full file text) to serve `codemap` calls.
      After a full index pass, call a `drop_sources()` sweep to release all file text;
      re-read on demand when `codemap` needs a body. On a large repo this can be many MB.

- [ ] **Drop `Block::Markdown` raw source after render** (`chat_writer.rs`): once a
      `Markdown` block has been rendered into `block_lines`, replace it with a
      `Block::Rendered(Vec<String>)` variant to free the original response body string.

- [ ] Skip `force_index_dir` in `symbols`/`codemap` if no mutating tools have
      run since the last index scan (track a `dirty` flag in `SymbolIndex`,
      set by `edit`/`write` via a shared atomic or by checking the mutation
      queue generation counter)
- [ ] Image input — paste/drag screenshots
- [ ] MCP server support — external tool providers
- [x] Session tree browser (`/tree`)
  - [x] ↑/↓ navigate, ←/→ page, Enter select, Esc cancel
  - [x] Ctrl+U user-only filter, Ctrl+O show-all filter
  - [x] Ctrl+←/Alt+← fold; Ctrl+→/Alt+→ unfold/jump
  - [x] ⊟/⊞ fold indicators, `← active` marker, `•` active-path dot
  - [x] User selection: leaf→parent, text placed in editor for re-submission
  - [x] Root user selection: leaf reset to null, text in editor
  - [x] Non-user selection: leaf set to node
  - [ ] v2: branch summary on fork (LLM-generated context for abandoned path)

## Shell Hooks

Executable scripts in `~/.nerv/hooks/`. Any language. Context via env vars + stdin JSON.

Slots: `before_tool_call`, `after_tool_call`, `before_prompt`, `after_response`, `on_start`, `on_exit`

- Non-zero exit from `before_*` cancels the action
- Hooks timeout after 10s
