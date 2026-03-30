# Compaction and Cache Efficiency

## How Anthropic prompt caching works

Every API request sends the full conversation history. Anthropic caches prefixes of the
prompt and charges two token types:

- **Wc** (`cache_creation_input_tokens`): tokens written to cache — priced at 125% of
  base input cost (a one-time investment per prefix)
- **Rc** (`cache_read_input_tokens`): tokens served from cache — priced at ~10% of base
  input cost

Cache hits require **exactly the same byte prefix**. If anything upstream shifts even one
byte, everything downstream misses. Cache TTL is 5 minutes (ephemeral) or 1 hour (the
`CacheRetention::Long` default nerv uses).

---

## Cache breakpoints nerv places

Anthropic allows up to 4 `cache_control` breakpoints per request. Nerv uses three,
placed on the most byte-stable parts of the prompt:

```
1. System prompt         [stable for entire session]   → Rc after turn 1
2. Tool definitions      [stable for entire session]   → Rc after turn 1
3. Last user message     [advances each turn]          → Wc this turn, Rc next turn
```

**System prompt** (`src/agent/anthropic.rs`): the system prompt is rebuilt identically
by `prepare_system_prompt` on every turn, so it hits the cache every time after the
first request. A breakpoint is placed on its last content block.

**Tool definitions** (`src/agent/anthropic.rs`): tool schemas never change within a
session. A breakpoint on the last tool object pins the entire tools array as Rc after the
first request. This is particularly valuable post-compaction (see below) — tool
definitions are cache-warm even when the conversation messages are cold.

**Last user message** (`messages_to_wire`): the cache breakpoint on the newest user
message advances one position each turn. It Wc on the turn it's placed, then Rc on the
next turn (when a new message takes the breakpoint role). This is the standard "pay once,
read once" pattern for rolling context.

---

## The compaction / cache tension

Without compaction, a long session accumulates context linearly. The oldest turns become
ever-larger Wc writes each call (since the breakpoint only covers the tail). Compaction
resets the conversation prefix to a small summary, dramatically reducing context size —
but at the cost of making the conversation prefix cache-cold.

```
Before compaction — long session, high Rc on old turns:
  [system Rc] [tools Rc] [msg1 Rc] [msg2 Rc] ... [msgN Rc] [new-msg Wc]

Immediately after compaction — conversation is cache-cold:
  [system Rc] [tools Rc] [summary Wc] [verbatim-window Rc] [new-msg Wc]

Two turns later — warm again:
  [system Rc] [tools Rc] [summary Rc] [verbatim-window Rc] [msg Rc] [new-msg Wc]
```

The key observation: system prompt and tool definitions survive compaction as Rc because
they are byte-identical before and after. Only the conversation portion cold-starts.

---

## How nerv compacts

Compaction is handled by `run_compaction` in `src/core/agent_session.rs` and
`find_cut_point` in `src/compaction/mod.rs`.

### Trigger conditions

- **Proactive** (threshold): after each turn, if total context tokens exceed
  `threshold_pct` of the model's context window, compaction fires before
  the next user turn
- **Reactive** (overflow): if the API returns a context-too-long error, compact and retry
- **Manual**: `/compact` slash command

### The three-region split

`find_cut_point` divides the session history into three regions:

```
[0 .. first_kept_entry_index)                  → deleted from DB, replaced by summary
[first_kept_entry_index .. verbatim_start_index)  → summarized by LLM call
[verbatim_start_index .. end)                  → kept verbatim (cache-warm post-compaction)
```

**Deleted region**: entries too old to be useful even in summary form. Removed from the
session DB entirely.

**Summarized region**: older history within the kept window. Sent to a utility model
(default: Haiku) with a summarization prompt. The resulting summary is stored as a
`CompactionSummary` session entry and prepended to the kept messages.

**Verbatim window**: the newest `verbatim_window_tokens` (default: 5 000) worth of
entries before the compaction boundary. These are left byte-for-byte in the DB. Because
they appeared verbatim in the pre-compaction requests, they are already in Anthropic's
cache — keeping them byte-identical means they recover as Rc hits on the very first
post-compaction API call. Only the new summary prefix is cache-cold (Wc) after
compaction.

Setting `verbatim_window_tokens = 0` disables the verbatim window and summarizes the
entire kept range (simpler, slightly smaller context, but pays more Wc on the first
post-compaction call).

### Token budget

- `keep_recent_tokens` (default: 20 000): total token budget for everything after the
  deleted region (summary + verbatim window + any kept messages)
- `verbatim_window_tokens` (default: 5 000): carved out of `keep_recent_tokens` for the
  verbatim tail; the remaining ~15 000 tokens hold the summary and older kept messages

### Utility model

`resolve_utility_provider` selects the summarization model (`src/core/agent_session.rs`):

1. `config.compaction_model` in `~/.nerv/config.json` (explicit override)
2. Same provider as the active model, using `DEFAULT_UTILITY_MODEL` (Haiku)
3. Any available provider

Haiku is fast and cheap for summarization — the summary content matters more than the
model size.

---

## Cost profile of a compaction event

| Call | Wc | Rc | Notes |
|------|----|----|-------|
| Pre-compaction (typical) | last-user-msg only | everything else | High Rc, growing slowly |
| Compaction LLM call | full old context | system + tools | One-time summarization cost |
| First post-compaction | summary | system + tools + verbatim window | Summary is new; verbatim window stays Rc |
| Second post-compaction | last-user-msg only | everything | Fully warm again |

The verbatim window shortens the cold-start period from two turns to one for the
conversation portion. System prompt and tool definitions never go cold.

---

## Configuration reference

```jsonc
// ~/.nerv/config.json
{
  "compaction": {
    "enabled": true,
    "threshold_pct": 0.80,        // compact at this % of context window
    "keep_recent_tokens": 20000,  // token budget for post-compaction context
    "verbatim_window_tokens": 5000 // tail of kept window to preserve verbatim
  }
}
```

---

## Source map

| Concern | File |
|---------|------|
| Cache breakpoints (system, tools, last-user) | `src/agent/anthropic.rs` |
| Compaction trigger/threshold | `src/core/compaction_controller.rs` |
| `CompactionSettings`, `find_cut_point`, token estimation | `src/compaction/mod.rs` |
| `run_compaction`, trigger logic | `src/core/agent_session.rs` |
| Summarization prompt and LLM call | `src/compaction/summarize.rs` |
| Session DB: `append_compaction`, branch walking | `src/session/manager.rs` |
