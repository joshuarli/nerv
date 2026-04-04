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

- [ ] **Language-aware AST references are still generic and heuristic-based.**
  - Current AST matching walks all identifier-like nodes and excludes likely definition nodes by parent kind list.
  - Risk: false positives/negatives for language-specific constructs (imports, member accesses, destructuring aliases, decorators, macro contexts).
  - Files: `src/index/references.rs` (`is_identifier_node`, `is_definition_name_node`, AST walker).

- [ ] **Comment/string exclusion in fallback is line-local and shallow.**
  - Current fallback filter (`line_has_identifier_outside_comment_or_string`) does not maintain multi-line comment/string state.
  - Risk: matches can leak from block comments / multiline strings.
  - File: `src/index/references.rs`.

- [ ] **Fallback definition exclusion is line-only and may under/over-exclude edge cases.**
  - Current rule excludes any hit on a line that equals an indexed definition line for the query.
  - Risk: same-line definition + usage collisions.
  - File: `src/index/references.rs`.

## P1 (determinism / UX)

- [ ] **No user-visible provenance when fallback is used.**
  - Intentional in v1, but limits debuggability when precision is questioned.
  - Consider optional debug marker behind config/flag.

- [ ] **`codemap` CLI does not expose `match` / `from`.**
  - Tool supports these fields, CLI subcommand currently hardcodes substring behavior.
  - File: `src/cli.rs`.

- [ ] **Exact ambiguity output could include stronger narrowing hints.**
  - Today: textual hint for `file/kind/from`.
  - Could emit a more parseable structure or suggested ready-to-run argument snippets.
  - Files: `src/index/codemap.rs`, `src/tools/codemap.rs`.

## P2 (performance / scale)

- [ ] **Reference scan can be expensive on large repos with broad filters.**
  - Candidate file expansion may traverse many files when `file` points to large directories.
  - No scan budget/circuit breaker in references path.
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
- [ ] Add unknown-argument detection for all tool JSON inputs used by agentic loop.
  - Implemented for `symbols`, `codemap`, and `grep`; still pending repo-wide rollout.
- [ ] Include "effective normalized args" in debug logging to aid postmortems.

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
- [ ] Add per-turn duplicate-read detection in tool orchestration:
  - same `(path, offset/limit or equivalent range)` should be dropped or downgraded.
- [ ] Surface a compact advisory message:
  - "range already present; use grep for localization."
- [ ] Keep override path for intentional rereads after edit mutation events.

#### Candidate files
- `src/agent/agent.rs`
- `src/tools/read.rs`

#### Acceptance criteria
- Tests confirm repeated identical reads in a turn are blocked/deduped.
- No regression for valid reread after file mutation.

### WS9: Evaluation harness additions for first-turn efficiency (P1/P2)

Goal: catch exploration regressions automatically.

#### Tasks
- [ ] Add eval metrics:
  - early miss rate (first N calls),
  - redundant read count,
  - broad grep count before first bounded target hit.
- [ ] Add oracle cases for "known-file bug hunt" where best path is symbols -> codemap -> bounded grep.
- [ ] Fail/flag when anti-pattern thresholds are exceeded.

#### Candidate files
- `eval/` harness and report scripts.

#### Acceptance criteria
- CI/local eval report includes new efficiency counters.
- Reproduced trace class (`a44f5a92`) scores better after guardrails land.

## Workstreams

## WS1: Strengthen AST references semantics per language (P0)

Goal: replace broad identifier walk with language-aware queries/rules.

### Tasks
- [ ] Add per-language references extraction module or strategy table:
  - Rust, Go, Python, TypeScript, TSX.
- [ ] Encode language-specific "usage vs definition" rules explicitly.
- [ ] Keep current per-file fallback behavior when AST extraction fails.
- [ ] Preserve output contract (`path:line:context`, line-only public output).

### Suggested file plan
- `src/index/references.rs`
- optional split: `src/index/references/{mod.rs,rust.rs,go.rs,python.rs,ts.rs}`

### Acceptance criteria
- For each AST language fixture:
  - finds real usages,
  - excludes definitions,
  - excludes comments/strings,
  - stable deterministic ordering.

## WS2: Make fallback filtering multiline-aware (P0)

Goal: avoid comment/string leakage in fallback.

### Tasks
- [ ] Introduce lightweight scanner state for block comments/strings where feasible.
- [ ] Handle escaped delimiters and language-specific comment starts conservatively.
- [ ] Keep failure mode safe (prefer dropping uncertain matches over adding false positives).

### Acceptance criteria
- Add fixtures with multiline comments and multiline strings containing target symbol.
- Ensure zero hits from those regions in fallback-only scenarios.

## WS3: Improve definition exclusion precision in fallback (P0/P1)

Goal: reduce same-line collision issues.

### Tasks
- [ ] Track definition byte/column spans where available from index to refine exclusion.
- [ ] In fallback mode, if same line contains both def + usage, attempt token-position disambiguation.

### Acceptance criteria
- Add cases where definition and usage share a line; retain usage, drop definition token match.

## WS4: Expose codemap exact controls in CLI (P1)

Goal: parity between CLI and tool API.

### Tasks
- [ ] Extend `nerv codemap` flags:
  - `--match substring|exact`
  - `--from <file>`
- [ ] Reuse existing validation semantics from tool where possible.
- [ ] Update CLI help text and docs examples.

### Files
- `src/cli.rs`
- `docs/tools.md`

### Acceptance criteria
- `nerv codemap foo --match exact --from src/lib.rs` works end-to-end.
- Invalid `--from` path errors clearly and exits non-zero.

## WS5: Add scale guards for references (P2)

Goal: preserve responsiveness on very large repos.

### Tasks
- [ ] Add optional max-files and/or max-runtime guardrails for references scan.
- [ ] Return partial results with explicit summary when capped.
- [ ] Keep default behavior backward-compatible unless guard is tripped.

### Acceptance criteria
- Deterministic partial-output behavior in stress tests.
- No hard abort from single-file parser failures.

## Test expansion backlog

## Unit tests (`src/index/references.rs`)
- [ ] language-specific fixtures for imports/aliases/member access.
- [ ] multiline comment/string exclusion in fallback.
- [ ] same-line definition+usage collision.
- [ ] symlink/canonical path dedupe edge cases.
- [ ] file filter behavior for file and directory filters in large trees.

## Tool tests (`src/tools/symbols.rs`, `src/tools/codemap.rs`)
- [ ] `symbols` invalid references query (empty/whitespace) error shape.
- [ ] `symbols` references output ordering stability across runs.
- [ ] `codemap` exact ambiguity guidance text regression tests.
- [ ] `codemap` invalid `from` error wording stability tests.

## Integration tests (`tests/tools.rs` / `tests/integration.rs`)
- [ ] end-to-end scenario: edit -> reindex -> references and codemap exact reflect updates.
- [ ] CLI codemap exact/from once WS4 ships.

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
