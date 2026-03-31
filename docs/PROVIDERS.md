# Provider Differences

## Token Usage Reporting

### Anthropic

Usage is reported per-message via the `message_start` SSE event and updated in `message_delta`. Fields:

- `input_tokens` — prompt tokens charged at full price
- `output_tokens` — generated tokens
- `cache_creation_input_tokens` — tokens written to the prompt cache (billed at 25% premium)
- `cache_read_input_tokens` — tokens served from the prompt cache (billed at ~10% of full price)

Cache writes are a distinct, billable event because caching is **explicit**: you opt in by placing `cache_control: {"type": "ephemeral"}` breakpoints in your request. Both writes and reads are meaningful to report.

### OpenAI-compatible (OpenAI, llama.cpp, Ollama)

Usage arrives in the final stream chunk (requires `stream_options: {include_usage: true}`). Fields:

- `prompt_tokens` — total prompt tokens (includes cached tokens for OpenAI/Ollama)
- `completion_tokens` — generated tokens
- `prompt_tokens_details.cached_tokens` — prompt tokens served from cache (OpenAI, Ollama)

There is no cache write field. OpenAI's prompt caching is **automatic and transparent**: no markers, no opt-in, no explicit write event. The server decides when to cache a prompt prefix. From the client's perspective, `cached_tokens` is simply an observation that some prompt tokens were cheaper — not the result of a deliberate action.

Ollama follows the same convention: KV-cache reuse happens automatically, so only cache reads are surfaced.

#### llama.cpp specifics

llama.cpp reports cache stats differently from the OpenAI spec. Usage comes from the `timings` object in the response:

- `timings.prompt_n` — tokens the model actually had to process (i.e. not cached). Nerv uses this as `input_tokens` so the "In" counter reflects real compute cost, not context size.
- `timings.cache_n` — tokens served from the KV cache. Used as `cache_read`.

This means `In + Rc ≈ total context size` for llama.cpp. The Rc hit rate `Rc / (In + Rc)` shown in the footer reflects what fraction of the context was served from cache across the session. In a typical long session, this climbs toward 90%+ as the growing conversation prefix is reused each turn.

## Caching Summary

| Field         | Anthropic | OpenAI-compat |
|---------------|-----------|---------------|
| Cache reads   | ✓ `cache_read_input_tokens` | ✓ `prompt_tokens_details.cached_tokens` |
| Cache writes  | ✓ `cache_creation_input_tokens` | — (automatic, not reported) |
| Opt-in        | Yes (`cache_control` breakpoints) | No (server-managed) |
