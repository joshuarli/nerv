# nerv

Rust coding agent for the terminal.

## Setup

**With Anthropic (Claude):**
```
nerv
/login                   # opens browser for OAuth
/model sonnet            # or opus, haiku
```

Or set `ANTHROPIC_API_KEY` for API key auth.

**With local models:**
```
nerv add unsloth/Qwen3.5-27B-GGUF Q4_K_XL
nerv load qwen3.5-27b
nerv
```

## Keybindings

| Key | Action |
|---|---|
| Enter | Send message |
| Shift+Enter | Newline |
| Ctrl+C | Interrupt stream / quit (double-tap to force) |
| Esc, Ctrl+D | Quit |
| Ctrl+G | Open message in $EDITOR |
| Ctrl+T | Cycle thinking level |
| Ctrl+Z | Suspend |
| Up/Down | Browse history (idle) / navigate queue (streaming) |
| Tab | Autocomplete slash commands |

## Commands

| Command | Description |
|---|---|
| `/model [name]` | List or switch models (`/model sonnet`) |
| `/model add local` | Connect to local OpenAI-compatible server |
| `/think [level]` | Set thinking: off, low, medium, high, xhigh |
| `/login [provider]` | OAuth login (default: anthropic) |
| `/logout [provider]` | Remove stored credentials |
| `/compact` | Compact conversation context |
| `/resume [id]` | Browse or load previous sessions |
| `/new` | Start new session |
| `/export <path>` | Export to .jsonl or .html |
| `/session` | Show session info |
| `/help` | Show all commands |

## Steering

Type while the agent is streaming — messages queue up. On Ctrl+C the
queued message fires immediately against the partial response.

## Permissions

Tool calls within the git repo root are auto-approved. Operations outside
the repo prompt for `y`/`n` confirmation. [Details](docs/permissions.md)

## Context management

- Auto-compacts when approaching context limit
- Emergency compact + retry on overflow errors
- Thinking blocks stripped from context (never referenced by model)
- Denied tool call args stripped to save tokens
- Stale tool results truncated after 10 turns

## Memory

The agent has a `memory` tool that writes to `~/.nerv/memory.md`.
Persistent across sessions, loaded into every system prompt.

## Skills

Markdown files in `~/.nerv/skills/` with YAML frontmatter:

```markdown
---
name: review
description: Code review
---
Review the code changes for correctness, style, and performance.
```

Invoke with `/review` or `/review <context>`.

## Config

```
~/.nerv/
├── config.json      # providers, models, headers (JSONC)
├── models.json      # local GGUF models + llama-server args (JSONC)
├── sessions.db      # SQLite session storage
├── memory.md        # agent-writable persistent memory
├── skills/          # skill markdown files
├── system-prompt.md # custom system prompt (optional)
└── debug.log        # NERV_LOG=debug for verbose
```

Credentials stored in macOS Keychain (not on disk).

## CLI

```
nerv                         # interactive TUI
nerv --resume [id]           # resume session
nerv --log-level <level>     # debug/info/warn/error
nerv add <hf-repo> <quant>   # download GGUF from HuggingFace
nerv load [alias]             # start llama-server
nerv models                   # list configured models
```

## Environment

| Variable | Description |
|---|---|
| `ANTHROPIC_API_KEY` | API key (bypasses OAuth) |
| `NERV_LOG` | Log level (default: warn) |
