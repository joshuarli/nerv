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

## Agent quality

### Tool schema (partially done)
- [x] Add `additionalProperties: false` to all tool schemas — prevents the model from
      inventing extra fields that get silently ignored.
- [x] Add `description` to bare properties that lacked one (`path` on read/write/find/ls/bash,
      `old_text`/`new_text` at the edit item level, `timeout` on bash).
- [x] Add `minItems: 1` on `edit.edits` array — rules out the empty-array no-op.
- [ ] **Guided error recovery in edit**: when `old_text` doesn't match, return the
      closest fuzzy candidate and its line number rather than just "not found". The model
      currently re-reads the whole file to see what changed; a hint eliminates that round trip.
- [ ] **Guided error recovery in bash**: for common failure modes (command not found,
      permission denied, missing file), prepend a structured tag (`[cmd-not-found]`,
      `[permission-denied]`) so the model can branch without parsing stderr prose.

### Context pipeline
- [ ] **Structured compaction summaries**: the current summarization prompt produces
      free-form prose. A JSON-structured summary (`{goal, progress, files_modified[],
      key_decisions[], next_steps, open_questions[]}`) would let post-compaction turns
      reference specific file states and decisions without re-reading.
- [ ] **Grep count-only mode**: add a `count_only` response mode (file + match count,
      no line content) for exploratory queries with hundreds of matches. Lets the model
      triage cost before deciding to read the full output.

### Parallelism
- [ ] **Mixed read+write parallelism**: when a tool batch contains reads *and* writes,
      the reads could run in parallel before the writes execute sequentially. Currently
      the entire batch goes sequential if any tool is mutating.

### Update callback plumbing
- [ ] **Wire `update_cb` through the agent loop**: `execute_tools` passes a no-op
      `update_cb` to `tool.execute`. The bash tool streams stdout via this callback but
      the chunks are black-holed. Route them through `AgentEvent::ToolProgress` so the
      TUI can show live bash output instead of waiting for the command to finish.

### Language coverage
- [ ] **C/C++ tree-sitter queries**: listed in the docs as supported but the actual
      tree-sitter queries are not implemented. Add `c` / `cpp` language support to
      `src/index/mod.rs`. Ruby, Java, Swift are also common enough to consider.

### Permission robustness
- [ ] **Adversarial bash path extraction tests**: `extract_path_tokens` in
      `permissions.rs` handles pipes, heredocs, backticks, redirects — add test cases
      for: heredoc with path in body, `$(cmd)` subshell, brace expansion `{a,b}/c`,
      multi-statement with `;` and `&&`, tee + redirect combos.

## Shell Hooks

Executable scripts in `~/.nerv/hooks/`. Any language. Context via env vars + stdin JSON.

Slots: `before_tool_call`, `after_tool_call`, `before_prompt`, `after_response`, `on_start`, `on_exit`

- Non-zero exit from `before_*` cancels the action
- Hooks timeout after 10s
