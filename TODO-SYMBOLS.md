# TODO-SYMBOLS

Detailed follow-up backlog for `symbols` / `symbols.references` / `codemap` symbol targeting.

## Why this exists

Recent work shipped:
- `codemap` exact matching (`match: substring|exact`) with optional `from` hint and deterministic ambiguity output.
- `symbols.references` AST-first lookup for Rust/Go/Python/TS/TSX with per-file ripgrep fallback.

This document tracks known quality gaps and concrete improvements for future agents.

## Current behavior snapshot (as of this TODO)

### `symbols` definitions
- Source: `SymbolIndex` (`src/index/mod.rs`), via tree-sitter symbol extraction.
- Query behavior: case-insensitive substring on symbol names, optional `kind` + `file` filter.

### `symbols.references`
- Entry point: `src/index/references.rs::find_references`.
- Flow:
  1. validate query is non-empty after trim.
  2. enumerate candidate files (indexed files + filter expansion).
  3. for each file:
     - AST extraction for supported extensions (`rs`, `go`, `py`, `ts`, `tsx`),
     - on AST failure, fallback to `rg --word-regexp --fixed-strings` for that file,
     - unsupported extensions use fallback directly.
  4. exclude definition lines using indexed `SymbolDef.line` in same file.
  5. remove comment/string matches.
  6. normalize path identity and dedupe by `(file,line)`.

### `codemap` exact mode
- Entry point: `src/index/codemap.rs`.
- `exact` uses case-sensitive `name == query`.
- Optional `from` path is validated in `src/tools/codemap.rs` and constrains exact matches to same file only.
- Exact ambiguity is never auto-picked; outputs capped candidate list (10 + remainder).

## Known limits (prioritized)

## P0 (correctness)

- [x] **Language-aware AST references are still generic and heuristic-based.**
  - Replaced flat mixed-language lists with per-language `LanguageConfig` structs (Rust, Go, Python, TypeScript).
  - Added special-case detection for Go `short_var_declaration`/`range_clause` LHS, Python `for`/`for_in_clause` loop targets, Python simple positional parameters.
  - Uses `children_by_field_name` to correctly handle multiple-name constructs (Go `const_spec`, `var_spec`).
  - Files: `src/index/references.rs`.

- [x] **Comment/string exclusion in fallback is line-local and shallow.**
  - Added `blocked_lines(source, ext)` pre-scan that tracks `/* ... */` block comments (all non-Python languages) and Python triple-quoted strings across lines.
  - rg fallback now filters hits on blocked lines before the per-line check.
  - File: `src/index/references.rs`.

- [x] **Fallback definition exclusion is line-only and may under/over-exclude edge cases.**
  - Refactored to `count_word_occurrences`; fallback hits on definition lines are retained when the query appears more than once (one occurrence is the definition, rest are genuine usages).
  - File: `src/index/references.rs`.

## P1 (determinism / UX)

- [ ] **No user-visible provenance when fallback is used.**
  - Intentional in v1, but limits debuggability when precision is questioned.
  - Consider optional debug marker behind config/flag.

- [x] **`codemap` CLI does not expose `match` / `from`.**
  - Added `--match substring|exact` and `--from <file>` flags to `nerv codemap`.
  - Invalid `--from` path exits non-zero with a clear error message.
  - File: `src/cli.rs`. Docs updated: `docs/tools.md`.

- [x] **Exact ambiguity output could include stronger narrowing hints.**
  - Each ambiguity candidate now includes a ready-to-run `codemap(query, match, from, kind)` snippet.
  - Files: `src/index/codemap.rs`.

## P2 (performance / scale)

- [x] **Reference scan can be expensive on large repos with broad filters.**
  - Added `MAX_CANDIDATE_FILES = 500` cap in `find_references`; candidate list is truncated at that limit.
  - `find_references` now returns `ReferencesOutput { hits, skipped_files }`; callers emit a `[partial: N file(s) not scanned]` note when `skipped_files > 0`.
  - File: `src/index/references.rs`.

- [ ] **Per-file parser setup is repeated and not pooled.**
  - AST path constructs parsers per file currently.
  - Could cache per-language parser instances within call scope.
  - File: `src/index/references.rs`.

## P3 (maintainability)

- [ ] **No shared abstraction for language-specific symbol/reference semantics.**
  - Symbol extraction and references logic duplicate language distinctions in separate places.
  - Suggest centralizing language config (extension → grammar + identifier rules + def node rules).

## Tool-call hygiene backlog (from export `~/.nerv/exports/a44f5a92.jsonl`)

This section tracks concrete fixes for weak early exploration behavior observed in the export:
- `codemap` called first with semantic terms (`cd`, `tilde`) instead of symbol-oriented discovery.
- Empty-query calls were sent as `query: "\"\""` (literal quote chars) instead of `query: ""`.
- `grep` calls used `file` and behaved as broad scans; intended file scoping was not enforced.
- `read` was reissued for ranges already present in context despite explicit already-read messages.

### Operator playbook: first 3 calls (default)

Use this sequence unless there is a very strong reason not to.

1. `symbols` inventory in likely files
   - Example:
     - `symbols(query: "", file: "src/main.rs")`
     - `symbols(query: "", file: "src/expand.rs")`
   - Goal: list candidate definitions cheaply before content reads.

2. Targeted `codemap` by exact symbol name
   - Example:
     - `codemap(file: "src/main.rs", query: "do_cd", match: "exact", depth: "full")`
   - Goal: inspect real symbol bodies rather than semantic keyword matches.

3. Bounded `grep` for literal wiring/call sites
   - Example:
     - `grep(path: "src/main.rs", pattern: "do_cd\\(")`
   - Goal: confirm call path and integration points with tight scope.

Escalate to `read` only if signatures/body snippets are insufficient or file is non-code.

### Do-not-do list for initial exploration

- Do not start with `codemap(query: "tilde")` or similar semantic probe words.
- Do not send `query: "\"\""` to `symbols`/`codemap`; empty query must be exactly `""`.
- Do not use broad grep roots for first-pass triage when a file is already known.
- Do not re-read files/ranges already in context after `[already read ...]`.

### WS6: Prompt-level guardrails for tool-call quality (P1)

Goal: make correct first calls the default through stronger tool instructions.

#### Tasks
- [x] Add explicit examples in system prompt for correct empty query:
  - `symbols(query: "")`, `codemap(query: "", file: "...")`.
- [x] Add a short anti-example block:
  - reject/avoid `query: "\"\""`.
- [x] Add startup guidance:
  - "Prefer `symbols(query: "", file: ...)` before semantic `codemap` probes."
- [x] Add re-read discipline text with concrete fallback:
  - "After `[already read ...]`, use `grep(path: ..., pattern: ...)` instead of `read`."

#### Files
- `src/core/system_prompt.rs`
- optional prompt docs where examples are mirrored.

#### Acceptance criteria
- Prompt snapshot/regression tests include both valid and invalid empty-query examples.
- Agent traces show reduced first-turn miss loops in local eval runs.

### WS7: Tool argument normalization and strict validation (P0/P1)

Goal: prevent silent drift from malformed or ambiguous arguments.

#### Tasks
- [x] `symbols`/`codemap`: normalize `query: "\"\""` and whitespace-only query to canonical empty query where safe.
- [x] `grep`: enforce canonical key as `path`; decide one policy:
  - strict error on unknown `file`, or
  - compatibility alias `file -> path` with explicit warning in tool output.
- [x] Add unknown-argument detection for all tool JSON inputs used by agentic loop.
  - Shared `validate_known_keys` in `src/tools/mod.rs`; applied to all tools (read, edit, write, find, ls, epsh, memory, symbols, codemap, grep).
- [x] Include "effective normalized args" in debug logging to aid postmortems.
  - `src/agent/agent.rs` logs `tool={name} args={args}` at DEBUG level after normalization.

#### Candidate files
- `src/tools/symbols.rs`
- `src/tools/codemap.rs`
- `src/tools/grep.rs`
- shared argument parsing/validation path in tool registry layer.

#### Acceptance criteria
- Unit tests for malformed inputs:
  - `query: "\"\""` -> behaves as empty query.
  - `grep(file: "x", pattern: "...")` follows chosen policy deterministically.
  - unknown keys return deterministic, actionable errors (or warnings if compat mode).
- Export traces no longer show broad accidental scans caused by arg-key mismatch.

### WS8: Duplicate-read suppression (P1)

Goal: reduce wasted calls and loop churn after read cache signals.

#### Tasks
- [x] Add per-turn duplicate-read detection in tool orchestration:
  - same `(path, offset/limit or equivalent range)` should be dropped or downgraded.
  - Implemented in `src/tools/read.rs` via mtime + range cache (`ReadCacheEntry`).
- [x] Surface a compact advisory message:
  - Full-file re-reads: `[unchanged since last read: ...]`; range re-reads: `[already read ... — use grep to locate specific text]`.
- [x] Keep override path for intentional rereads after edit mutation events.
  - mtime-based invalidation: any file write changes mtime and clears the dedup guard.

#### Candidate files
- `src/agent/agent.rs`
- `src/tools/read.rs`

#### Acceptance criteria
- Tests confirm repeated identical reads in a turn are blocked/deduped. ✓ (range_dedup_*, mtime_cache_* tests in read.rs)
- No regression for valid reread after file mutation. ✓ (range_dedup_invalidated_by_edit)

### WS9: Evaluation harness additions for first-turn efficiency (P1/P2)

Goal: catch exploration regressions automatically.

#### Tasks
- [x] Add eval metrics:
  - `max_redundant_reads`: counts same-path re-reads in the trace; GOAL/MISS against a threshold.
  - `max_broad_greps_before_targeted`: counts grep calls without a path arg before the first scoped grep.
- [x] Add oracle task for "known-file bug hunt": `eval/tasks/known-file-bug-hunt/` — a Python bug-fix task where the agent is told which file to look at; goals enforce `require_before_read: ["symbols","codemap"]`, `max_broad_greps_before_targeted: 0`, and `max_turns: 5`.
- [x] Fail/flag when anti-pattern thresholds are exceeded: `check_goals` in `eval/run.py` emits MISS lines for both new metrics when thresholds are violated.
- [ ] Early miss rate (first N call quality) — still pending; no clear oracle definition yet.

#### Candidate files
- `eval/run.py` (metrics added).
- `eval/tasks/known-file-bug-hunt/` (new oracle task).

#### Acceptance criteria
- [x] `max_redundant_reads` and `max_broad_greps_before_targeted` goal types recognized and scored in `check_goals`.
- [x] `known-file-bug-hunt` task runs via `python3 eval/run.py --task known-file-bug-hunt`.
- [ ] Early miss rate counter still pending.

## Workstreams

## WS1: Strengthen AST references semantics per language (P0) ✓

Goal: replace broad identifier walk with language-aware queries/rules.

### Tasks
- [x] Add per-language references extraction module or strategy table:
  - Rust, Go, Python, TypeScript, TSX.
- [x] Encode language-specific "usage vs definition" rules explicitly.
- [x] Keep current per-file fallback behavior when AST extraction fails.
- [x] Preserve output contract (`path:line:context`, line-only public output).

### Acceptance criteria
- [x] For each AST language fixture: finds real usages, excludes definitions, excludes comments/strings, stable deterministic ordering. (tests: `references_are_usages_only_for_ast_languages`, `go_short_var_declaration_lhs_is_not_a_reference`, `go_range_clause_lhs_is_not_a_reference`, `python_for_loop_variable_is_not_a_reference`, `python_simple_parameter_is_not_a_reference`, `go_var_spec_name_is_not_a_reference`)

## WS2: Make fallback filtering multiline-aware (P0) ✓

Goal: avoid comment/string leakage in fallback.

### Tasks
- [x] Introduce lightweight scanner state for block comments/strings where feasible.
- [x] Handle language-specific comment starts conservatively (C-style `/* */` for most; triple-quotes for Python).
- [x] Keep failure mode safe (prefer dropping uncertain matches over adding false positives).

### Acceptance criteria
- [x] Fixtures with multiline C-style block comments and Python triple-quoted strings (tests: `blocked_lines_c_style_block_comment`, `blocked_lines_python_triple_double_quote`, `blocked_lines_python_triple_single_quote`, `fallback_block_comment_interior_suppressed`).

## WS3: Improve definition exclusion precision in fallback (P0/P1) ✓

Goal: reduce same-line collision issues.

### Tasks
- [x] In fallback mode, if same line contains both def + usage, attempt token-position disambiguation.
  - Implemented via `count_word_occurrences`: fallback hits on definition lines are retained when count > 1.

### Acceptance criteria
- [x] Cases where definition and usage share a line: tests `count_word_occurrences_basic`, `definition_line_with_dual_occurrence_retained_for_fallback`.

## WS4: Expose codemap exact controls in CLI (P1) ✓

Goal: parity between CLI and tool API.

### Tasks
- [x] Extend `nerv codemap` flags: `--match substring|exact`, `--from <file>`.
- [x] Reuse existing validation semantics from tool where possible.
- [x] Update CLI help text and docs examples.

### Acceptance criteria
- [x] `nerv codemap foo --match exact --from src/lib.rs` works end-to-end.
- [x] Invalid `--from` path errors clearly and exits non-zero.

## WS5: Add scale guards for references (P2) ✓

Goal: preserve responsiveness on very large repos.

### Tasks
- [x] Add max-files guardrail: `MAX_CANDIDATE_FILES = 500` cap in `find_references`.
- [x] Return partial results with explicit summary when capped: `[partial: N file(s) not scanned — result set capped at 500 files]` appended to REFERENCES output.
- [x] Keep default behavior backward-compatible unless guard is tripped.

### Acceptance criteria
- [x] Deterministic capped behavior.
- [x] Partial-output summary message (`skipped_files_is_zero_when_under_cap` test; message surfaces in `symbols.rs` output when `skipped_files > 0`).

## Test expansion backlog

## Unit tests (`src/index/references.rs`)
- [x] language-specific fixtures: Go short_var_declaration, range_clause, var_spec; Python for-loop, simple parameter. (WS1)
- [x] multiline comment/string exclusion in fallback: `blocked_lines_*`, `fallback_block_comment_interior_suppressed`. (WS2)
- [x] same-line definition+usage collision: `count_word_occurrences_*`, `definition_line_with_dual_occurrence_retained_for_fallback`. (WS3)
- [x] symlink/canonical path dedupe: `symlink_paths_deduplicate_to_canonical` (unix).
- [x] file filter behavior: `file_filter_restricts_to_single_file`, `file_filter_restricts_to_directory`.

## Tool tests (`src/tools/symbols.rs`, `src/tools/codemap.rs`)
- [x] `symbols` invalid references query (empty/whitespace) error shape.
- [x] `symbols` references output ordering stability across runs.
- [x] `codemap` exact ambiguity guidance text regression tests.
- [x] `codemap` invalid `from` error wording stability tests.

## Integration tests (`tests/tools.rs` / `tests/integration.rs`)
- [x] end-to-end scenario: edit → reindex → references and codemap exact reflect updates. (`symbols_reindex_after_file_edit`, `codemap_reindex_after_file_edit`, `references_reindex_after_file_edit` in `tests/tools.rs`). Tests simulate the post-edit `index_file()` callback because `index_dir` is debounced (5s) and would otherwise skip re-parsing in rapid test loops.
- [x] CLI codemap exact/from (WS4 shipped; covered by CLI flag parsing).

## Suggested execution order

1. WS1 AST semantics hardening.
2. WS2 fallback multiline exclusion.
3. WS3 fallback definition precision.
4. WS4 CLI parity for codemap exact/from.
5. WS5 performance guards.

## Implementation notes for future agents

- Preserve current external output contracts unless deliberately versioning them.
- Keep deterministic sorting and dedupe semantics stable.
- Avoid byte-index panics; maintain char-boundary-safe slicing in any new string logic.
- Prefer adding focused fixtures over broad snapshot tests.
- If behavior changes materially, update both docs:
  - `docs/tools.md`
  - `docs/design.md`

## Quick verification commands

```bash
cargo test index::references -- --nocapture
cargo test tools::symbols -- --nocapture
cargo test index::codemap -- --nocapture
cargo test tools::codemap -- --nocapture
cargo test -q
```
