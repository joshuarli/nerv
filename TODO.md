## General

## Memory (RSS reduction)

- [x] **Undo stack cap** (`editor.rs`): `undo_stack: Vec<(Vec<String>, usize, usize)>` grows
      unboundedly — every keystroke pushes a full copy of all editor lines. Cap at 50 entries
      with `truncate`, same as kill ring.

- [x] **Evict `source` from `FileEntry` after indexing** (`index/mod.rs`): each `FileEntry`
      holds `source: Option<Arc<String>>` (the full file text) to serve `codemap` calls.
      Source is now dropped immediately after `parse_symbols`; `render()` re-reads on demand
      via the parallel `fs::read_to_string` fallback already in `codemap::render`.

- [x] **Drop `Block::Markdown` raw source after render** (`chat_writer.rs`): once a
      `Markdown` block has been rendered into `block_lines`, it is replaced with
      `Block::Rendered(Vec<String>)` to free the original response body string.

- [x] Skip `force_index_dir` in `symbols`/`codemap` if no mutating tools have
      run since the last index scan — implemented via `is_fresh()` + `index_dir` debounce;
      `mark_dirty()` / `index_file()` handle post-tool invalidation.
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
