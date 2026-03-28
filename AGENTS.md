# nerv

Token-efficient coding agent. See [README.md](README.md) for usage.

## Architecture

Sync — no tokio/async. OS threads + crossbeam channels.

```
┌──────────┐  stdin_tx   ┌──────────┐  cmd_tx   ┌──────────────────┐
│ stdin    │ ──────────> │ main     │ ────────> │ session thread   │
│ thread   │             │ loop     │ <──────── │ (Agent+Provider) │
└──────────┘             │ (select!)│ event_tx  └──────────────────┘
                         └──────────┘
```

Three threads: stdin reader (poll-based, pausable for $EDITOR), main event loop, session (agent + provider).

### Source layout

```
src/
├── main.rs                    # event loop, CLI, print mode, signal handling
├── lib.rs                     # module exports
├── http.rs                    # shared ureq agent (native-tls)
├── errors.rs                  # ProviderError, ToolError
├── agent/
│   ├── types.rs               # AgentMessage, Model, Usage, AgentEvent, ToolResultData
│   ├── convert.rs             # AgentMessage → LlmMessage, transform_context (9 optimizations)
│   ├── provider.rs            # Provider trait, ProviderRegistry, CancelFlag
│   ├── anthropic.rs           # Anthropic Messages API + SSE + OAuth headers
│   ├── openai_compat.rs       # OpenAI-compatible (llama-server, Ollama)
│   └── agent.rs               # Agentic loop: stream → tool calls → permissions → context gate → loop
├── tools/
│   ├── read.rs                # Whole-file read with line numbers + mtime cache
│   ├── edit.rs                # Single + multi-edit, fuzzy match, BOM/CRLF
│   ├── write.rs               # File write with mkdir -p
│   ├── bash.rs                # /bin/bash -c, stderr on background thread
│   ├── grep.rs                # ripgrep wrapper (--context=3 for fewer follow-up reads)
│   ├── find.rs                # fd wrapper
│   ├── ls.rs                  # eza tree wrapper
│   ├── memory.rs              # Persistent memory read/add/remove
│   ├── diff.rs                # In-process Myers diff (replaced `similar` crate)
│   ├── file_mutation_queue.rs # Per-file mutex for concurrent writes
│   └── truncate.rs            # Output truncation (head/tail, bytes/lines)
├── session/
│   ├── types.rs               # SessionEntry variants, TokenInfo, SessionTreeNode
│   └── manager.rs             # SQLite backend, tree-aware branching, get_tree()
├── compaction/
│   ├── mod.rs                 # tiktoken counting, cut point selection
│   └── summarize.rs           # LLM-based summarization
├── core/
│   ├── agent_session.rs       # AgentSession: prompt orchestration, compaction, login
│   ├── auth.rs                # Keychain storage, Anthropic OAuth (PKCE)
│   ├── config.rs              # NervConfig (JSONC), per-provider headers
│   ├── permissions.rs         # Tool call permission checks
│   ├── local_models.rs        # GGUF download, hardware detection, llama-server
│   ├── model_registry.rs      # Built-in + custom models, fuzzy matching
│   ├── resource_loader.rs     # AGENTS.md/CLAUDE.md walker, memory, skills
│   ├── system_prompt.rs       # Prompt assembly, per-model override
│   ├── skills.rs              # Skill loading from ~/.nerv/skills/
│   └── tool_registry.rs       # ToolRegistry
├── worktree.rs                    # Git worktree create/merge for isolated sessions
├── interactive/
│   ├── chat_writer.rs         # Block-cached chat output (streaming, tools, status)
│   ├── event_loop.rs          # Slash commands, permission prompts, session picker
│   ├── layout.rs              # AppLayout: editor + statusbar + footer + chat
│   ├── footer.rs              # Hexagon context bar, model, cost, API call counter, plan mode
│   ├── statusbar.rs           # Spinner, per-turn token delta, tok/s, queue
│   ├── session_picker.rs      # /resume session list
│   ├── tree_selector.rs       # /tree session branch navigator
│   └── theme.rs               # ANSI color constants
└── tui/
    ├── tui.rs                 # Component trait, diff renderer (Vec<u8> buffer)
    ├── terminal.rs            # Raw libc terminal, DECSCUSR cursor, TCSAFLUSH
    ├── stdin_buffer.rs        # Raw stdin → StdinEvent
    ├── keys.rs                # VT100 + Kitty CSI-u + modifyOtherKeys parsing
    ├── highlight.rs           # Zero-dep syntax highlighter (8 languages)
    └── components/            # Editor, Markdown, StyledText
```

## Key design decisions

- **Content vs display**: tool results have `content` (terse, sent to LLM) and `details.display` (rich, TUI only). Edit returns "Edited foo.rs" to the model, full diff to the user. See [docs/tools.md](docs/tools.md).
- **Per-model system prompts**: `~/.nerv/prompts/{model_id}.md` → global override → compiled default. Smaller models get numbered rules; larger models get nuanced guidelines.
- **Multi-edit**: `edits` array in one tool call, matched against original file, uniqueness-enforced, overlap-detected, applied in reverse position order.
- **Callback streaming**: `on_event: &dyn Fn(AgentEvent)` — events flow to TUI in real time
- **Poll-based stdin**: reader uses `libc::poll()` with 100ms timeout + atomic pause flag so $EDITOR gets exclusive terminal access
- **Session tree**: parent_id chain in SQLite, tree-aware branch walking, `/tree` TUI selector
- **In-process diff**: Myers algorithm, ~170 lines, replaces `similar` crate (51KB binary savings)
- **Per-turn token deltas**: statusbar shows marginal cost (↑800 ↓110), footer shows cumulative context
- **SQLite sessions**: WAL mode, entries table with parent_id chain, 12µs listing
- **Context optimization**: 12 zero-LLM-cost transforms in `transform_context` (strip thinking, denied args, orphans; truncate stale results; superseded result dedup for read/grep/ls/find/edit; bash success compression; stale edit arg stripping; compact diff in edit results; read result folding around referenced lines; adaptive stale cutoff; tool description pruning after 4 turns) plus tool-level optimizations (read mtime cache, auto-size small files, grep context lines) and a circuit breaker for unexpected context growth. See [docs/context.md](docs/context.md).
- **Per-API-call token tracking**: each `AssistantMessage` carries its own `Usage` from its API call. Footer shows cumulative API usage `(N calls, Mk tok)` when multiple calls occur in one turn.
- **Context circuit breaker**: `ContextGateFn` callback in `stream_response` — prompts user to confirm when context grows >20k tokens AND >30% between consecutive API calls (skips first 4 rounds for warmup)
- **Plan mode**: `/plan`, Shift+Tab, or bare "plan" toggles read-only research mode. Removes edit/write from the tool set and injects a planning-focused system prompt section. `ToolRegistry::set_active` handles the filtering.
- **Git worktrees**: `/wt <branch>` creates an isolated worktree for the session; `/wt merge` merges back and cleans up. Session DB tracks worktree path for `/resume` restoration.
- **macOS Keychain**: credentials via `security` CLI, not on disk
- **Execution loop**: `Agent::prompt` (agent.rs) is the clean inner loop (compress → model → execute → persist → update); `AgentSession::prompt` (agent_session.rs) orchestrates via `prepare_callbacks` (permission + context gate), `run_agent_prompt` (per-iteration SQLite persistence via `persist_fn` callback), and `post_turn` (compaction + session naming). See [docs/execution-loop.md](docs/execution-loop.md).

## Deep dives

- [Design](docs/design.md) — core principles, eval-driven insights, what actually saves tokens
- [Tools](docs/tools.md) — tool design, content vs display, multi-edit algorithm, allocation tracking
- [Cancellation](docs/cancellation.md) — ^C flow, reader threads, should_quit flag
- [Permissions](docs/permissions.md) — classification, path resolution, cross-thread y/n prompt
- [Context optimization](docs/context.md) — transform_context, compaction, token savings
- [Authentication](docs/auth.md) — OAuth PKCE, Keychain, required headers
- [Local models](docs/local-models.md) — GGUF download, hardware detection, llama-server
- [Execution loop](docs/execution-loop.md) — loop structure, file ownership, where it diverges from the ideal, retry/circuit-breaker
- [Evals](eval/AGENTS.md) — eval harness, task design, oracle tests, report analysis
