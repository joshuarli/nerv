# Debugging

## Session exports

Every session can be exported as JSONL for offline analysis. Each line is a self-contained JSON object (a *session entry*). The file begins with a `session` header and ends with a `summary` line.

### Exporting

```
nerv --export-jsonl <session-id>          # writes to ~/.nerv/exports/<id>.jsonl
nerv --export-jsonl <session-id> out.jsonl  # custom path
```

From within an interactive session: `/export` writes JSONL + HTML to `~/.nerv/exports/`.

### Parsing

```
scripts/parse-jsonl-session.py ~/.nerv/exports/<id>.jsonl
```

Prints a unified timeline, tool-call trace, compaction details (including the full archived transcript), token metrics, and cache stats.

### Entry types

| `type` | Key fields | Notes |
|---|---|---|
| `session` | `id`, `version`, `cwd`, `timestamp`, `leaf_id` | First line |
| `message` | `message` (role + content), `tokens` | One per user/assistant/tool_result turn |
| `compaction` | `summary`, `tokens_before`, `tokens_after`, `archived_messages`, `model_id`, `cost_usd_before` | `archived_messages` is the complete pre-compaction transcript (including the verbatim window retained in the DB) |
| `system_prompt` | `prompt`, `token_count` | Recorded at turn start for reproducibility |
| `model_change` | `provider`, `model_id` | |
| `thinking_level_change` | `thinking_level` | `"off"` / `"low"` / `"medium"` / `"high"` |
| `permission_accept` | `tool`, `args` | |
| `label` | `label` | Freeform bookmark |
| `branch_summary` | `from_id`, `summary` | |
| `summary` | `api_calls`, `total_input`, `total_output`, `cache_read`, `cache_write`, `cache_hit_rate`, `max_context` | Last line |

### Message content blocks

Each `message.message.content` is an array of typed blocks:

| `type` | Used in | Fields |
|---|---|---|
| `text` | user, assistant | `text` |
| `thinking` | assistant (extended thinking) | `text` |
| `toolCall` | assistant | `id`, `name`, `arguments` (object) |
| `toolResult` | tool_result messages | `id`, `content` (array), `is_error`, `display` |

`display` on a `toolResult` is the TUI-facing compact summary. `content` is what the LLM sees. For debugging prefer `content`.

### Compaction and the archived transcript

When compaction runs, the oldest context is replaced by an LLM-generated summary. `archived_messages` on the `compaction` entry preserves the full pre-compaction transcript — both the deleted messages and the verbatim window that was retained — so the export is self-contained without querying the database.
