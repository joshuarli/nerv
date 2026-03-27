# nerv

Rust coding agent for the terminal. See [README.md](README.md) for usage.

## Architecture

Sync — no tokio/async. OS threads + crossbeam channels.

```
┌──────────┐  stdin_tx   ┌──────────┐  cmd_tx   ┌──────────────────┐
│ stdin    │ ──────────> │ main     │ ────────> │ session thread   │
│ thread   │             │ loop     │ <──────── │ (Agent+Provider) │
└──────────┘             │ (select!)│ event_tx  └──────────────────┘
                         └──────────┘
```

Three threads: stdin reader, main event loop, session (agent + provider).

### Source layout

```
src/
├── main.rs                    # event loop, CLI, signal handling
├── lib.rs                     # module exports
├── http.rs                    # shared ureq agent (native-tls, no status-as-error)
├── errors.rs                  # ProviderError, ToolError
├── agent/
│   ├── types.rs               # AgentMessage, Model, Usage, AgentEvent
│   ├── convert.rs             # AgentMessage → LlmMessage, transform_context
│   ├── provider.rs            # Provider trait, ProviderRegistry, CancelFlag
│   ├── anthropic.rs           # Anthropic Messages API + SSE + OAuth headers
│   ├── openai_compat.rs       # OpenAI-compatible (llama-server, Ollama)
│   └── agent.rs               # Agentic loop: stream → tool calls → permissions → loop
├── tools/                     # 8 built-in tools (AgentTool trait)
├── session/
│   ├── types.rs               # SessionEntry variants, TokenInfo
│   └── manager.rs             # SQLite backend, WAL mode, 12µs listing
├── compaction/
│   ├── mod.rs                 # tiktoken counting, cut point selection
│   └── summarize.rs           # LLM-based summarization
├── core/
│   ├── agent_session.rs       # AgentSession: prompt orchestration, compaction, login
│   ├── auth.rs                # Keychain storage, Anthropic OAuth (PKCE)
│   ├── config.rs              # NervConfig (JSONC), per-provider headers
│   ├── permissions.rs         # Tool call permission checks
│   ├── local_models.rs        # GGUF download, hardware detection, llama-server args
│   ├── model_registry.rs      # Built-in + custom models, fuzzy matching
│   ├── resource_loader.rs     # AGENTS.md/CLAUDE.md walker, memory, skills
│   ├── system_prompt.rs       # Prompt assembly
│   ├── skills.rs              # Skill loading from ~/.nerv/skills/
│   └── tool_registry.rs       # ToolRegistry
├── interactive/
│   ├── chat_writer.rs         # Block-cached chat output (streaming, tools, status)
│   ├── event_loop.rs          # Slash commands, permission prompts, session picker
│   ├── layout.rs              # AppLayout: editor + statusbar + footer + chat
│   ├── footer.rs              # Hexagon context bar, model, cost, thinking level
│   ├── statusbar.rs           # Spinner, tok/s, queue display
│   └── theme.rs               # ANSI color constants
└── tui/
    ├── tui.rs                 # Component trait, diff renderer (Vec<u8> buffer)
    ├── terminal.rs            # Raw libc terminal, TCSAFLUSH cleanup
    ├── stdin_buffer.rs        # Raw stdin → StdinEvent
    ├── keys.rs                # VT100 + Kitty CSI-u parsing
    ├── highlight.rs           # Zero-dep syntax highlighter (8 languages)
    └── components/            # Editor, Markdown, StyledText
```

## Key design decisions

- **Callback streaming**: `on_event: &mut dyn FnMut(ProviderEvent)` — events flow to TUI in real time
- **Reader thread for cancellation**: SSE reads in background thread, main thread polls with 50ms timeout, drop receiver to cancel instantly
- **ChatWriter**: single persistent component for all chat output, block-level render caching
- **SQLite sessions**: WAL mode, entries table with parent_id chain, 12µs listing
- **Permission system**: auto-approve within repo, prompt for everything else
- **Context optimization**: strip thinking, denied args, orphans; truncate stale results
- **macOS Keychain**: credentials via `security` CLI, not on disk
- **JSONC config**: JSON with `//` comments, zero-dep parser

## Deep dives

- [Cancellation](docs/cancellation.md) — ^C flow, reader threads, should_quit flag
- [Permissions](docs/permissions.md) — classification, path resolution, cross-thread y/n prompt
- [Context optimization](docs/context.md) — transform_context, compaction, token savings
- [Authentication](docs/auth.md) — OAuth PKCE, Keychain, required headers
- [Local models](docs/local-models.md) — GGUF download, hardware detection, llama-server args
