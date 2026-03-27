## General

- [ ] Image input — paste/drag screenshots
- [ ] MCP server support — external tool providers
- [ ] Session tree browser (`/tree`) — in progress
  - [ ] v2: node folding (collapse subtrees)
  - [ ] v2: filter modes (user-only, no-tools, labeled-only)
  - [ ] v2: search within tree
  - [ ] v2: label editing from tree view
  - [ ] v2: branch summary on fork (LLM-generated context for abandoned path)

## Shell Hooks

Executable scripts in `~/.nerv/hooks/`. Any language. Context via env vars + stdin JSON.

Slots: `before_tool_call`, `after_tool_call`, `before_prompt`, `after_response`, `on_start`, `on_exit`

- Non-zero exit from `before_*` cancels the action
- Hooks timeout after 10s
