# nerv

Token-efficient coding agent. Every tool result, system prompt, and display output is designed to minimize context consumption while maximizing the model's ability to act.

## What's different

- **Tool results split into LLM content vs TUI display.** The edit tool returns "Edited foo.rs" to the model (3 tokens) but shows the full unified diff to the user. Read, grep, find, ls all show compact summaries in the TUI while sending full content to the model.
- **Per-model system prompts.** `~/.nerv/prompts/{model_id}.md` lets you tune behavior per model — terse numbered rules for small models, nuanced guidelines for large ones.
- **Multi-edit tool.** Multiple disjoint replacements in one call, matched against the original file. One tool call instead of five.
- **Per-turn token deltas.** The statusbar shows "↑800 ↓110" — what this turn *added*, not the cumulative 32k context.
- **Session tree branching.** Fork conversations at any point, navigate branches with `/tree`.
- **Headless mode with structured output.** `echo "fix the bug" | nerv --print --model sonnet` outputs JSON with full message trace, per-turn token usage, and cost.
- **Eval harness.** `eval/run.py` drives nerv against coding tasks, measures turns/tools/tokens/cost, checks efficiency goals, supports on_fail hints.

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
| Shift+Enter, Ctrl+Enter | Newline |
| Ctrl+C | Interrupt stream / quit (double-tap to force) |
| Esc, Ctrl+D | Quit |
| Ctrl+G | Open message in $EDITOR |
| Shift+Tab | Toggle plan mode (read-only research) |
| Ctrl+T | Cycle thinking level |
| Ctrl+Z | Suspend |
| Up/Down | Browse history (idle) / navigate queue (streaming) |
| Tab | Autocomplete slash commands |

## Commands

| Command | Description |
|---|---|
| `/model [name]` | List or switch models |
| `/model add local` | Connect to local OpenAI-compatible server |
| `/think [level]` | Set thinking: off, low, medium, high, xhigh |
| `/login [provider]` | OAuth login (default: anthropic) |
| `/logout [provider]` | Remove stored credentials |
| `/compact` | Compact conversation context |
| `/plan` | Toggle plan mode (read-only, no edit/write) |
| `/resume [id]` | Browse or load previous sessions |
| `/tree` | Browse and switch session branches |
| `/wt <branch>` | Create git worktree for isolated work |
| `/wt merge` | Merge worktree back and clean up |
| `/new` | Start new session |
| `/export <path>` | Export to .jsonl or .html |
| `/session` | Show session info |
| `/commit` | Create a git commit (skill) |
| `/help` | Show all commands |

## CLI

```
nerv                              # interactive TUI
nerv --resume [id]                # resume session
nerv --model <name>               # select model on startup
nerv --wt <branch>                # create worktree on startup
nerv --print                      # headless: stdin prompt → JSON stdout
nerv --print --model sonnet       # headless with specific model
nerv --print --max-turns 10       # cap agent turns
nerv --list-models                # show all available models
nerv models                       # list all models (API + local)
nerv add <hf-repo> <quant>        # download GGUF from HuggingFace
nerv load [alias]                 # start llama-server
```

## Config

```
~/.nerv/
├── config.json          # providers, models, headers (JSONC)
├── models.json          # local GGUF models + llama-server args
├── sessions.db          # SQLite session storage
├── memory.md            # agent-writable persistent memory
├── skills/              # skill markdown files
├── prompts/             # per-model system prompts
│   └── claude-haiku-4-5.md
├── system-prompt.md     # global system prompt override
└── debug.log            # NERV_LOG=debug for verbose
```

Credentials stored in macOS Keychain (not on disk).

## Documentation

- [Design](docs/design.md) — core principles, eval-driven insights, what actually saves tokens
- [Tools](docs/tools.md) — built-in tool design, content vs display, multi-edit algorithm
- [Permissions](docs/permissions.md) — auto-approve in repo, prompt outside
- [Context](docs/context.md) — transform_context, compaction, token savings
- [Cancellation](docs/cancellation.md) — ^C flow, reader threads
- [Authentication](docs/auth.md) — OAuth PKCE, Keychain
- [Local models](docs/local-models.md) — GGUF download, llama-server
- [Evals](eval/AGENTS.md) — eval harness, task design, report analysis

## Environment

| Variable | Description |
|---|---|
| `ANTHROPIC_API_KEY` | API key (bypasses OAuth) |
| `NERV_LOG` | Log level (default: warn) |
