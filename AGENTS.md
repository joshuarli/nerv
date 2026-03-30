# nerv

Token-efficient coding agent.

### Source layout

```
src/
├── main.rs                    # TUI event loop, signal handling, should_quit flag
├── cli.rs                     # CLI parsing, arg handling, subcommand dispatch
├── bootstrap.rs               # Shared setup: agent, tools, session, model registry
├── lib.rs                     # module exports
├── http.rs                    # shared ureq agent (native-tls)
├── errors.rs                  # ProviderError, ToolError
├── export.rs                  # Session export (HTML, JSONL)
├── log.rs                     # File-based debug logging
├── worktree.rs                # Git worktree create/merge for isolated sessions
├── agent/
│   ├── types.rs               # AgentMessage, Model, Usage (Copy), AgentEvent, ToolDetails
│   ├── convert.rs             # AgentMessage → LlmMessage (role mapping, merge)
│   ├── transform.rs           # transform_context optimizations, ContextConfig, pipeline
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
│   ├── truncate.rs            # Output truncation (head/tail, bytes/lines)
│   └── output_filter/         # Bash output compression pipeline
│       ├── mod.rs             # filter_bash_output: ANSI strip → dedup → JSON → language
│       ├── ansi.rs            # ANSI escape sequence stripping
│       ├── dedup.rs           # Consecutive duplicate line collapsing
│       ├── json.rs            # Large JSON → key/type skeleton
│       ├── rust.rs            # cargo test/build/check/clippy output compression
│       ├── go.rs              # go test output compression
│       ├── python.rs          # pytest/unittest output compression
│       └── ts.rs              # jest/vitest output compression
├── session/
│   ├── types.rs               # SessionEntry variants, TokenInfo, SessionTreeNode
│   └── manager.rs             # SQLite backend, tree-aware branching, get_tree()
├── compaction/
│   ├── mod.rs                 # chars/4 token estimation, cut point selection
│   └── summarize.rs           # LLM-based summarization
├── core/
│   ├── agent_session.rs       # AgentSession: prompt orchestration, compaction
│   ├── session_runner.rs      # session_task loop + handle_login (runs on OS thread)
│   ├── compaction_controller.rs # CompactionController: threshold, triggered flag
│   ├── auth.rs                # Keychain storage, Anthropic OAuth (PKCE)
│   ├── config.rs              # NervConfig (JSONC), per-provider headers
│   ├── permissions.rs         # Tool call permission checks, SAFE_HOME_DIRS
│   ├── local_models.rs        # GGUF download, hardware detection, llama-server
│   ├── model_registry.rs      # Built-in + custom models, fuzzy matching
│   ├── resource_loader.rs     # AGENTS.md/CLAUDE.md walker, memory, skills
│   ├── system_prompt.rs       # Prompt assembly, per-model override
│   ├── skills.rs              # Skill loading from ~/.nerv/skills/
│   ├── tool_registry.rs       # ToolRegistry (Vec<Arc<dyn AgentTool>>)
│   ├── notifications.rs       # Shell command hooks on agent events
│   └── retry.rs               # Transient API error retry with backoff
├── interactive/
│   ├── event_loop.rs          # Slash commands, permission prompts, session picker
│   ├── chat_writer.rs         # Block-cached chat output (streaming, tools, status)
│   ├── layout.rs              # AppLayout: editor + statusbar + footer + chat
│   ├── footer.rs              # Hexagon context bar, model, cost, API call counter, plan mode
│   ├── statusbar.rs           # Spinner, per-turn token delta, tok/s, queue
│   ├── display.rs             # Display utilities for interactive mode
│   ├── session_picker.rs      # /resume session list
│   ├── tree_selector.rs       # /tree session branch navigator
│   ├── model_picker.rs        # /model full-screen model selector
│   ├── fullscreen_picker.rs   # Generic full-screen alt-screen picker
│   ├── btw_overlay.rs         # /btw full-screen ephemeral Q&A overlay
│   ├── btw_panel.rs           # /btw inline bordered panel
│   └── theme.rs               # ANSI color constants
└── tui/
    ├── tui.rs                 # Component trait, diff renderer (Vec<u8> buffer)
    ├── terminal.rs            # Raw libc terminal, DECSCUSR cursor, TCSAFLUSH
    ├── stdin_buffer.rs        # Raw stdin → StdinEvent
    ├── keys.rs                # VT100 + Kitty CSI-u + modifyOtherKeys parsing
    ├── highlight.rs           # Zero-dep syntax highlighter
    ├── utils.rs               # Unicode width/segmentation helpers
    └── components/            # Editor, Markdown, StyledText, Box, Loader, Select, Spacer, Text
```

## Coding rules

**No byte-index string slices.** `&s[..n]` panics if `n` splits a multi-byte char.
When you want to limit by character count, say so:
```rust
s.char_indices().nth(120).map_or(s, |(i, _)| &s[..i])
```
When you genuinely have a byte offset that needs snapping to a char boundary (e.g. from a parser), use `floor_char_boundary`:
```rust
&s[..s.floor_char_boundary(n)]
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
- [Debugging](docs/debugging.md) — session JSONL export format, parse-jsonl-session.py, entry types, compaction archive
- [Evals](eval/AGENTS.md) — eval harness, task design, oracle tests, report analysis
