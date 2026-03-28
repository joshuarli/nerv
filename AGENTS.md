# nerv

Token-efficient coding agent.

### Source layout

```
src/
├── main.rs                    # event loop, CLI, print mode, signal handling
├── lib.rs                     # module exports
├── http.rs                    # shared ureq agent (native-tls)
├── errors.rs                  # ProviderError, ToolError
├── agent/
│   ├── types.rs               # AgentMessage, Model, Usage, AgentEvent, ToolResultData
│   ├── convert.rs             # AgentMessage → LlmMessage (role mapping, merge)
│   ├── transform.rs           # transform_context: 12 optimizations, ContextConfig, pipeline
│   ├── provider.rs            # Provider trait, ProviderRegistry, CancelFlag
│   ├── anthropic.rs           # Anthropic Messages API + SSE + OAuth headers
│   ├── openai_compat.rs       # OpenAI-compatible (llama-server, Ollama)
│   └── agent.rs               # Agentic loop: stream → tool calls (parallel readonly) → context gate → loop
├── index/
│   ├── mod.rs                 # tree-sitter symbol index (Rust), incremental by mtime
│   └── codemap.rs             # codemap core: symbol search → source body assembly
├── tools/
│   ├── read.rs                # File read with line numbers, mtime cache + range dedup
│   ├── edit.rs                # Single + multi-edit, fuzzy match, BOM/CRLF
│   ├── write.rs               # File write with mkdir -p
│   ├── bash.rs                # /bin/bash -c, stderr on background thread
│   ├── grep.rs                # ripgrep wrapper (--context=3 for fewer follow-up reads)
│   ├── find.rs                # fd wrapper
│   ├── ls.rs                  # eza tree wrapper
│   ├── symbols.rs             # tree-sitter symbol lookup tool (definitions + references)
│   ├── codemap.rs             # codemap agent tool (thin wrapper over index/codemap)
│   ├── memory.rs              # Persistent memory read/add/remove
│   ├── diff.rs                # In-process Myers diff (replaced `similar` crate)
│   ├── file_mutation_queue.rs # Per-file mutex for concurrent writes
│   └── truncate.rs            # Output truncation (head/tail, bytes/lines)
├── session/
│   ├── types.rs               # SessionEntry variants, TokenInfo, SessionTreeNode
│   └── manager.rs             # SQLite backend, tree-aware branching, get_tree()
├── compaction/
│   ├── mod.rs                 # chars/4 token estimation, cut point selection
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

## Deep dives

- [Design](docs/design.md) — core principles and key design decisions
- [Tools](docs/tools.md) — tool design, content vs display, multi-edit algorithm, allocation tracking
- [Cancellation](docs/cancellation.md) — ^C flow, reader threads, should_quit flag
- [Permissions](docs/permissions.md) — classification, path resolution, cross-thread y/n prompt
- [Context optimization](docs/context.md) — transform_context, compaction, token savings
- [Authentication](docs/auth.md) — OAuth PKCE, Keychain, required headers
- [Local models](docs/local-models.md) — GGUF download, hardware detection, llama-server
- [Execution loop](docs/execution-loop.md) — loop structure, file ownership, where it diverges from the ideal, retry/circuit-breaker
- [Evals](eval/AGENTS.md) — eval harness, task design, oracle tests, report analysis
