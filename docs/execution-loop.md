# Execution Loop

The agentic loop follows a state-machine pattern across two layers:
`src/agent/agent.rs` (the generic loop) and `src/core/agent_session.rs`
(the session-aware orchestrator). Everything runs on a single OS thread —
no async, no thread pool for tool execution.

## Data flow

```
                         AgentSession::prompt
                        ┌──────────────────────────────────────────────┐
                        │  prepare_system_prompt()                     │
                        │  prepare callbacks (permission, gate)        │
                        │                                              │
                        │  ┌─── Agent::prompt (the tool loop) ──────┐ │
                        │  │                                        │ │
  messages ─────────────┼──┼──→ transform_context ──→ convert_to_llm│ │
  (full history)        │  │        │                       │       │ │
                        │  │        │ 7 zero-cost           │       │ │
                        │  │        │ optimizations         ▼       │ │
                        │  │        │                CompletionRequest│ │
                        │  │        │                       │       │ │
                        │  │        ▼                       ▼       │ │
                        │  │   context gate ──────→ stream_completion│ │
                        │  │   (circuit breaker)        │   (SSE)  │ │
                        │  │                            ▼          │ │
                        │  │                    AssistantMessage    │ │
                        │  │                            │          │ │
                        │  │                    ┌───────┴───────┐  │ │
                        │  │               tool_calls?     no tools│ │
                        │  │                    │              │   │ │
                        │  │              execute_tools    break   │ │
                        │  │                    │                  │ │
                        │  │              ToolResult[]             │ │
                        │  │                    │                  │ │
                        │  │              push to state ───→ next  │ │
                        │  └────────────────────────────────────┘ │
                        │                                          │
                        │  run_agent_prompt: persist to SQLite      │
                        │  post-turn: compaction, session naming    │
                        └──────────────────────────────────────────┘
```

## `transform_context` — the key innovation

Every API call sends the full conversation history. Without intervention,
a 30-tool-call session with file reads easily hits 100k+ tokens. Rather
than truncating or summarizing (which loses information and costs an LLM
call), `transform_context` applies 7 deterministic, zero-cost optimizations
that preserve everything the model needs while discarding what it doesn't.

It runs before every API call. It never mutates the source messages — it
returns a new `Vec<AgentMessage>` derived from a clone. The stale/recent
boundary is frozen at the start of each prompt loop so the message prefix
is byte-stable across consecutive API calls within the same tool loop,
maximizing cache-read (Rc) hits.

The 7 optimizations, in application order:

| # | Optimization | What it does | Typical savings |
|---|---|---|---|
| 1 | Strip thinking | Remove `ContentBlock::Thinking` — never referenced by the model | 1k–10k+ per response |
| 2 | Strip denied args | Replace denied tool_use arguments with `{}` | up to 1.5k per denial |
| 3 | Remove orphans | Drop tool_use blocks with no matching tool_result (aborted streams) | prevents API 400 errors |
| 4 | Truncate stale results | Tool results before `RECENT_TURNS` (10) → 200-char preview | large reads → ~200 chars |
| 5 | Superseded reads | Earlier reads of the same file → `[superseded by later read]` | 200–2k+ per redundant read |
| 6 | Bash success compression | `cargo test ok` / `cargo check` success → single summary line | 100–2k per build |
| 7 | Strip stale edit args | Stale edit/write tool_use → keep only `path`, drop content | 100–5k per stale edit |

These compose: a stale, denied edit of a file that was later re-read gets
optimizations 2, 5, and 7 simultaneously. In a typical 20-tool session,
total savings are 30–60% of raw context size, with zero LLM calls and
zero information loss for the model's current task.

See [context.md](context.md) for the full pipeline including tool-level
optimizations (read mtime cache, grep context lines) and compaction.

## Invariants

These must hold across the entire loop. Violating any of them causes
either API errors, cache waste, or incorrect behavior.

1. **Every `tool_use` has a `tool_result`.** Anthropic's API rejects
   orphaned tool_use blocks. `transform_context` strips orphans as a
   safety net, but the loop itself should never produce them — the only
   case is mid-stream abort (^C during streaming).

2. **`transform_context` never mutates sent messages.** It clones the
   message vec and transforms the clone. If it mutated in place, the
   next API call's prefix would differ from the previous call's prefix,
   invalidating the cache. This was learned the hard way (12x cost
   regression).

3. **`stale_cutoff` is frozen per prompt loop.** Computed once at the
   start of `Agent::prompt` and passed to every `stream_response` call.
   If the cutoff shifted as new messages accumulated, earlier messages
   would flip between stale (truncated) and recent (full) representations
   between consecutive API calls, breaking cache prefix stability.

4. **Tool execution is sequential and single-threaded.** Tools run one
   at a time on the session thread. This is deliberate: tool execution
   is not the bottleneck (API latency dominates), and sequential execution
   keeps permission prompts, file mutation ordering, and cancel semantics
   trivial.

5. **Persistence is per-iteration via callback.** `Agent::prompt` accepts
   an optional `persist_fn: Option<&mut dyn FnMut(&AgentMessage)>` that
   is called for each message as it's produced (user, assistant, and tool
   results). `run_agent_prompt` passes a closure that writes to SQLite
   (WAL mode — cheap single-row inserts). A mid-turn crash recovers
   everything up to the last completed tool call. `Agent` has no knowledge
   of SQLite or sessions — persistence is injected, same pattern as
   `on_event`.

6. **Cancel is cooperative.** `cancel` (AtomicBool) is checked at two
   points: inside `stream_completion` (aborts the SSE read loop) and
   between tool rounds in `Agent::prompt` (skips the next API call).
   Tool execution itself is not interruptible — cancel takes effect
   after the current tool finishes.

## Call tree

```
session_task()                          ← OS thread; receives SessionCommand
  AgentSession::prompt()                ← one user turn
    prepare_system_prompt()             ← rebuild tools + system prompt
    prepare_callbacks()                 ← wire permission_fn + context_gate_fn
    run_agent_prompt()
      Agent::prompt(persist_fn)         ← the tool loop (agent.rs)
        loop {
          stream_response()
            transform_context()         ← 7 zero-cost optimizations
            context gate check          ← circuit breaker
            provider.stream_completion()← SSE stream → content blocks
          persist_fn(assistant)         ← write to SQLite immediately
          execute_tools()               ← permission → dispatch → result
          persist_fn(each tool_result)  ← write to SQLite immediately
        }
    post_turn()
      overflow compaction + retry
      threshold compaction
      session naming
```

## File ownership

| Concern | File | Key function |
|---|---|---|
| Tool loop | `src/agent/agent.rs` | `Agent::prompt` |
| API call + streaming | `src/agent/agent.rs` | `stream_response` |
| Tool dispatch | `src/agent/agent.rs` | `execute_tools` |
| Context optimization | `src/agent/convert.rs` | `transform_context` |
| Session orchestration | `src/core/agent_session.rs` | `AgentSession::prompt` |
| Callback wiring | `src/core/agent_session.rs` | `prepare_callbacks` |
| Per-iteration persistence | `src/core/agent_session.rs` | `run_agent_prompt` |
| Post-turn housekeeping | `src/core/agent_session.rs` | `post_turn` |
| Compaction | `src/compaction/mod.rs` | `find_cut_point`, `should_compact` |

## Retry logic

`stream_response` has an inner retry loop (up to 4 attempts) for transient
API errors (overloaded, rate-limited). The retry loop resets all SSE
accumulation state on each attempt and fires `AgentEvent::Retrying` so
the TUI can show a status message. Backoff: 5s → 30s → 60s → 60s, or
the `retry-after-ms` header value from Anthropic's 429 response.

Retries are invisible to `Agent::prompt` — from its perspective,
`stream_response` either returns an `AssistantMessage` or an error.

## Context gate (circuit breaker)

Before each API call, the estimated token count is compared to the previous
call. If **all** of these conditions hold, the user is prompted to confirm:

- At least 4 tool rounds completed (warmup — early reads cause expected growth)
- Absolute growth >20k tokens (roughly a 500-line file)
- Relative growth >30%

The gate blocks the session thread on a crossbeam channel. The user's
response arrives from the main thread; `false` aborts with
`StopReason::Aborted`. See [context.md](context.md) for the full picture.

## Where it could be cleaner

**`transform_context` is invisible at the loop level.** It runs inside
`stream_response`, not as a visible step in `Agent::prompt`. Lifting it
would make the loop match the data flow diagram exactly and allow
pre-computing the token estimate before entering the streaming function.

**`load_state` is split.** Message history and the system prompt are
rebuilt separately (`reload_agent_context()` and `prepare_system_prompt()`).
Both run before `run_agent_prompt` but aren't a single named step.
