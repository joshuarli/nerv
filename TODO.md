### Tool schema
- [ ] **Guided error recovery in edit**: when `old_text` doesn't match, return the
      closest fuzzy candidate and its line number rather than just "not found". The model
      currently re-reads the whole file to see what changed; a hint eliminates that round trip.
- [ ] **Guided error recovery in bash**: for common failure modes (command not found,
      permission denied, missing file), prepend a structured tag (`[cmd-not-found]`,
      `[permission-denied]`) so the model can branch without parsing stderr prose.

### JSON navigation

The current output_filter schema-extracts large JSON blobs, but we should also
guide LLMs toward efficient JSON workflows at the *prompt* level:

- **Always curl to a file**: `curl -s URL > /tmp/out.json`, then explore with `jq`.
  `cat`-ing a raw JSON API response directly is almost always wrong — add this to
  the system prompt as an explicit rule.
- **jq for schema discovery**: recommend `jq 'keys'`, `jq '.[0] | keys'`,
  `jq 'to_entries | .[0]'` patterns before reading full values.
- **Make JSON grep-friendly**: `jq -c '.[]'` (one object per line) makes ripgrep
  useful on JSON arrays. The system prompt should suggest this for large arrays.
- **Ban `cat large.json`**: this should be a permission-gate / bash filter that
  intercepts `cat *.json` where the file exceeds ~10KB and either rejects it with
  an error message or auto-pipes through the JSON schema extractor.

### Bash command filtration / permission hardening

The current permission system gates on path safety and destructive ops, but many
commands waste tokens without being dangerous. Areas to improve:

- **Block naive file reads via bash**: `cat file`, `head file`, `tail file`,
  `sed '' file`, `awk '' file` on text/source files should be rejected with a
  message pointing to the `read` tool. Already partially done (display suppressed),
  but the LLM still sees the full content in context. A permission-layer rejection
  (exit-before-execute) would be cleaner and save the round-trip.
- **Block `cat large.json`**: as above — files >10KB matching `*.json` should
  route through the schema extractor or be rejected outright.
- **Warn on broad `find` / `ls` without path**: `find /` or `ls /` produce huge
  outputs; require a path argument or cap depth automatically.
- **Log suspicious expansions**: commands like `cat *`, `grep -r . /`, or
  `for f in $(find ...)` are common token-wasters and potential prompt-injection
  vectors; flag for user review.
- The nuance here is high — pipes, subshells, here-docs, and brace-expansion all
  interact. Any static analysis will have edge cases. Test coverage for
  `extract_path_tokens` is already tracked above; build on that foundation.

- [ ] **Structured compaction summaries**: the current summarization prompt produces
      free-form prose. A JSON-structured summary (`{goal, progress, files_modified[],
      key_decisions[], next_steps, open_questions[]}`) would let post-compaction turns
      reference specific file states and decisions without re-reading.
- [ ] **Grep count-only mode**: add a `count_only` response mode (file + match count,
      no line content) for exploratory queries with hundreds of matches. Lets the model
      triage cost before deciding to read the full output.

### Update callback plumbing
- [ ] **Wire `update_cb` through the agent loop**: `execute_tools` passes a no-op
      `update_cb` to `tool.execute`. The bash tool streams stdout via this callback but
      the chunks are black-holed. Route them through `AgentEvent::ToolProgress` so the
      TUI can show live bash output instead of waiting for the command to finish.

### Permission robustness
- [ ] **Adversarial bash path extraction tests**: `extract_path_tokens` in
      `permissions.rs` handles pipes, heredocs, backticks, redirects — add test cases
      for: heredoc with path in body, `$(cmd)` subshell, brace expansion `{a,b}/c`,
      multi-statement with `;` and `&&`, tee + redirect combos.

### Shell Hooks

Executable scripts in `~/.nerv/hooks/`. Any language. Context via env vars + stdin JSON.

Slots: `before_tool_call`, `after_tool_call`, `before_prompt`, `after_response`, `on_start`, `on_exit`

- Non-zero exit from `before_*` cancels the action
- Hooks timeout after 10s
