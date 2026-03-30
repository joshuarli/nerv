# Refactor Plan

Ordered by dependency — later tasks may touch files changed by earlier ones.
Each task is self-contained and can be verified independently.

---

## Task 1 — Replace `ToolResult::details: Option<serde_json::Value>` with `ToolDetails`

**Problem:** The details bag is untyped. Known keys — `display`, `diff`,
`filtered`, `exit_code` — are accessed with stringly-typed `.get("key")` calls
scattered across `agent.rs` (`run_one_tool`) and `transform.rs` (checks
`filtered:true` to skip re-processing).

**Changes:**
- Add `pub struct ToolDetails { display: Option<String>, diff: Option<String>, filtered: bool, exit_code: Option<i32> }` in `agent/types.rs` (or its own file).
- Replace `ToolResult::details: Option<serde_json::Value>` with `details: Option<ToolDetails>`.
- Replace `AgentMessage::ToolResult { details: Option<serde_json::Value>, … }` with `details: Option<ToolDetails>`.
- Update every tool that sets details: `edit.rs` (sets `display`, `diff`), `bash.rs` (sets `filtered`, `exit_code`), `write.rs` (sets `display`).
- Update `agent.rs` `run_one_tool`: reads `result.details.as_ref().and_then(|d| d.display.clone())` for display.
- Update `transform.rs`: reads `details.as_ref().map_or(false, |d| d.filtered)` instead of `.get("filtered")`.
- Update `anthropic.rs` / `openai_compat.rs` serialization if they inspect details.

**Files:** `src/agent/types.rs`, `src/agent/agent.rs`, `src/agent/transform.rs`,
`src/tools/edit.rs`, `src/tools/bash.rs`, `src/tools/write.rs`, provider files.

---

## Task 2 — `AgentTool::is_readonly()` replaces the hardcoded `READONLY_TOOLS` slice

**Problem:** `READONLY_TOOLS` in `agent.rs` is a magic string constant. Adding a
new readonly tool requires updating it separately from the tool's declaration.

**Changes:**
- Add `fn is_readonly(&self) -> bool { false }` default method to `AgentTool` trait.
- Override it to return `true` in `ReadTool`, `GrepTool`, `FindTool`, `LsTool`, `SymbolsTool`, `CodemapTool`, `MemoryTool` (memory reads only — but memory also writes; leave it false or split).
- Remove `const READONLY_TOOLS` from `agent.rs`.
- In `execute_tools`, replace `READONLY_TOOLS.contains(&name.as_str())` with `tool.is_readonly()` looked up from the tool object (already available in the loop).

**Files:** `src/agent/agent.rs`, `src/tools/read.rs`, `src/tools/grep.rs`,
`src/tools/find.rs`, `src/tools/ls.rs`, `src/tools/symbols.rs`,
`src/tools/codemap.rs`.

---

## Task 3 — Explicit imports replace glob imports in non-test code

**Problem:** `use crate::agent::types::*`, `use crate::core::*`,
`use crate::agent::provider::*`, `use crate::tools::*` make it impossible to
see where a name comes from.

**Files and the globs to replace:**
- `src/core/agent_session.rs`: `use crate::agent::types::*`
- `src/compaction/summarize.rs`: `use crate::agent::provider::*`
- `src/export.rs`: `use crate::agent::types::*`
- `src/bootstrap.rs`: `use crate::tools::*` (if present)

**Change:** Replace each glob with an explicit list of the names actually used.
Run `cargo check` to confirm nothing is missed.

---

## Task 4 — `NervConfig` loaded once at startup, passed where needed

**Problem:** `NervConfig::load(nerv_dir())` is called 6+ times inside
`agent_session.rs` — including inside the per-tool-call permission closure and
inside `run_compaction`. The config file is read and parsed from disk every
time.

**Design:** Load once at process start, store as an immutable value for the
whole run. Side-effect: the process is predictable (config changes mid-run are
ignored, which is the desired behaviour per the original issue).

**Changes:**
- In `src/core/config.rs`, confirm `NervConfig` is `Clone`.
- In `src/bootstrap.rs` (`bootstrap_session` / `bootstrap_agent`), load config
  once and pass it through.
- Add a `config: NervConfig` field to `AgentSession`.
- In `AgentSession::new`, accept `config: NervConfig`.
- Remove all inline `NervConfig::load(…)` calls inside `agent_session.rs`
  (permission closure, `run_compaction`, `handle_login`, `load_session`). Use
  `self.config` instead.
- Pass `&config` into `prepare_callbacks` so the permission-denied notification
  fires from the pre-loaded config, not a fresh disk read.
- In `main.rs` top-level `run_interactive` / `run_print_mode`, remove any
  inline `NervConfig::load` calls and use the single instance from bootstrap.

**Files:** `src/core/config.rs`, `src/core/agent_session.rs`, `src/bootstrap.rs`, `src/main.rs`.

---

## Task 5 — Extract `CompactionController` from `AgentSession`

**Problem:** Compaction logic (`run_compaction`, the auto-compact fields, the
`compaction_triggered` AtomicBool, mid-stream trigger detection, threshold
management) is mixed into `AgentSession` alongside prompt orchestration and
session persistence.

**Design:**
```
pub struct CompactionController {
    settings: CompactionSettings,
    /// 0–100; shared with UI for /compact threshold display.
    pub threshold_pct: Arc<AtomicU32>,
    /// Set by the UsageUpdate callback when mid-stream threshold is crossed.
    pub triggered: Arc<AtomicBool>,
    pub auto_compact: bool,
}
```

`CompactionController` gets a `run(&mut self, session_manager: &mut SessionManager, agent_messages: &[AgentMessage], config: &NervConfig) -> Result<Option<CompactionResult>, String>` method containing the current body of `run_compaction`.

`AgentSession` holds a `pub compaction: CompactionController` field. All current `self.auto_compact`, `self.compaction_triggered`, `self.compact_threshold_pct`, and `self.compaction_settings` references become `self.compaction.*`.

The mid-stream UsageUpdate check in `run_agent_prompt`'s `on_event` closure checks `self.compaction.triggered`.

**Files:** New `src/core/compaction_controller.rs`, `src/core/mod.rs`,
`src/core/agent_session.rs`.

---

## Task 6 — `stream_response` stops mutating `self.state.messages`

**Problem:** `stream_response` calls `self.state.messages.push(AgentMessage::Assistant(msg.clone()))` internally (line ~500 of agent.rs). All other pushes (`User` messages, `ToolResult` messages) happen in `prompt`. The asymmetry makes the state-mutation flow hard to follow and makes `stream_response` have a side effect inconsistent with its name.

**Change:**
- Remove the `self.state.messages.push(…)` call from `stream_response`.
- In `prompt`, after `let assistant = self.stream_response(…)`, push `AgentMessage::Assistant(assistant.clone())` to `self.state.messages` alongside the existing `new_messages.push`.
- All pushes to `self.state.messages` now live in one place: the `prompt` loop.

**Files:** `src/agent/agent.rs`.

---

## Task 7 — `Agent` encapsulates its mutable state (set_model, set_system_prompt, set_tools)

**Problem:** `AgentState` has all-public fields. External code directly
mutates `agent.state.model`, `agent.state.system_prompt`, `agent.state.tools`,
`agent.state.thinking_level`, `agent.state.effort_level`, and all four
function pointers. `Agent` has no invariant boundary.

**Changes:**
- Keep `AgentState` as a private implementation detail (remove `pub` from the
  struct and its fields, or at minimum stop accessing fields directly from
  outside `agent.rs`).
- Add methods to `Agent`:
  - `pub fn set_model(&mut self, model: Option<Model>)`  
    (also resets `prev_estimated_tokens` to 0 on model change)
  - `pub fn set_system_prompt(&mut self, prompt: String)`
  - `pub fn set_tools(&mut self, tools: Vec<Arc<dyn AgentTool>>)`
  - `pub fn set_thinking_level(&mut self, level: ThinkingLevel)`
  - `pub fn set_effort_level(&mut self, level: Option<EffortLevel>)`
  - `pub fn model(&self) -> Option<&Model>`
  - `pub fn messages(&self) -> &[AgentMessage]`
  - `pub fn set_messages(&mut self, messages: Vec<AgentMessage>)`  
    (used only by compaction/load paths)
  - `pub fn clear_messages(&mut self)`
  - `pub fn set_permission_fn(&mut self, f: Option<PermissionFn>)`
  - `pub fn set_context_gate_fn(&mut self, f: Option<ContextGateFn>)`
  - `pub fn set_post_tool_fn(&mut self, f: Option<PostToolFn>)`
  - `pub fn set_output_gate_fn(&mut self, f: Option<OutputGateFn>)`
  - `pub fn is_streaming(&self) -> bool`
- Update all call sites in `agent_session.rs`, `bootstrap.rs`, `main.rs` to use
  methods.

**Files:** `src/agent/agent.rs`, `src/core/agent_session.rs`, `src/bootstrap.rs`, `src/main.rs`.

---

## Task 8 — CLI parsing extracted to `src/cli.rs`

**Problem:** `main.rs` is 2,416 lines. ~530 lines are `parse_args` boilerplate.
~280 lines are `handle_subcommand`. The flags `--model`, `--thinking`, `--effort`,
`--prompt`, `--log-level` are parsed with near-identical code in three places.

**Changes:**
- Create `src/cli.rs` (module declared in `lib.rs` or `main.rs`).
- Define:
  ```rust
  pub struct CommonFlags {
      pub model: Option<String>,
      pub thinking: Option<ThinkingLevel>,
      pub effort: Option<EffortLevel>,
      pub log_level: Option<String>,
  }
  pub enum Subcommand {
      Interactive { prompt: Option<String>, flags: CommonFlags, … },
      Talk { prompt: Option<String>, flags: CommonFlags },
      Print { prompt: String, flags: CommonFlags, … },
      Worktree { … },
      … // other subcommands
  }
  pub fn parse_args() -> Subcommand
  ```
- Move `parse_args` and `handle_subcommand` bodies into `cli.rs`.
- `main.rs` calls `cli::parse_args()` → dispatches on `Subcommand`.
- `main.rs` retains: `main()`, `run_interactive()`, `run_print_mode()`, signal
  handler setup, event loop.

**Files:** New `src/cli.rs`; `src/main.rs`, `src/lib.rs`.

---

## Task 9 — `session_task` and `SessionCommand` dispatch extracted to own file

**Problem:** `session_task` (the OS thread that handles ~20 `SessionCommand`
variants) lives at the bottom of `agent_session.rs`, which already owns session
state, compaction, callbacks, and prompt orchestration. The 400-line command
dispatch loop is a separate concern.

**Changes:**
- Create `src/core/session_runner.rs`.
- Move `pub fn session_task(…)` and the `SessionCommand` match arm bodies into
  it. The file has one responsibility: receive a `SessionCommand`, call the
  appropriate method on `AgentSession`, send `AgentSessionEvent`s.
- `SessionCommand` and `AgentSessionEvent` enum definitions stay in
  `agent_session.rs` since they are the public protocol types.
- `handle_login` free function moves with `session_task` (it's only called
  from there).

**Files:** New `src/core/session_runner.rs`; `src/core/mod.rs`;
`src/core/agent_session.rs`.

---

## Task 10 — `UpdateCallback` removed or wired properly

**Problem:** `AgentTool::execute` takes `update: UpdateCallback` but every
call site passes `Arc::new(|_output: String| {})` — a no-op. The type exists
but carries no information.

**Decision:** Remove the parameter from the trait and all tool impls. If
live progress streaming for bash is wanted later, it should go through a
typed channel passed via a separate mechanism (e.g. an `on_event` reference
on the bash tool itself), not a generic stringly-typed callback on every tool.

**Files:** `src/agent/agent.rs`, all tool files that implement `AgentTool`.

---

## Task 11 — Unused `SessionError` removed from `errors.rs`

**Problem:** `errors.rs` defines `SessionError { Corrupt, MigrationFailed, Io }`
but `session/manager.rs` uses `anyhow::Result` throughout and never returns it.

**Change:** Delete the `SessionError` enum from `errors.rs`. If session errors
ever need structuring, add them then.

**Files:** `src/errors.rs`.

---

## Task 12 — `bootstrap.rs` index watcher uses `SOURCE_EXTENSIONS`

**Problem:** The `post_tool_fn` in `bootstrap.rs` only calls `index.index_file`
when the path extension is `"rs"`. `SOURCE_EXTENSIONS` in `src/index/mod.rs`
already lists `["rs", "go", "py", "ts", "tsx"]`.

**Change:** Import `SOURCE_EXTENSIONS` from `crate::index` and replace the
hardcoded `"rs"` check:
```rust
if path.extension().is_some_and(|e| SOURCE_EXTENSIONS.contains(&e.to_str().unwrap_or("")))
```

**Files:** `src/bootstrap.rs`, `src/index/mod.rs` (make `SOURCE_EXTENSIONS`
`pub` if not already).

---

## Task 13 — `allowed_dirs` gets a typed newtype handle

**Problem:** `AgentSession::allowed_dirs` is `pub Arc<Mutex<Vec<PathBuf>>>`,
directly manipulated by `main.rs` with `.lock().unwrap().push(dir)`. The type
leaks internal representation.

**Change:** Add:
```rust
#[derive(Clone, Default)]
pub struct AllowedDirs(Arc<Mutex<Vec<PathBuf>>>);

impl AllowedDirs {
    pub fn push(&self, dir: PathBuf) { … }
    pub fn snapshot(&self) -> Vec<PathBuf> { … }
}
```
Replace the raw `Arc<Mutex<Vec>>` field with `AllowedDirs`. Update `main.rs`
call site to use `.push(dir)`. Update `prepare_callbacks` to call `.snapshot()`.

**Files:** `src/core/agent_session.rs` (or `src/core/permissions.rs`), `src/main.rs`.

---

## Execution order

```
1  → ToolDetails struct              (self-contained types change)
2  → is_readonly() on trait          (depends on Task 1 types settling)
3  → kill glob imports               (mechanical, any order)
4  → NervConfig once at startup      (touches AgentSession struct)
5  → CompactionController            (depends on Task 4 config field)
6  → stream_response side-effect fix (self-contained agent.rs change)
7  → Agent encapsulation             (depends on Tasks 5+6 settling agent.rs)
8  → CLI to cli.rs                   (independent of agent changes)
9  → session_task to session_runner  (depends on Task 5+7)
10 → Remove UpdateCallback           (depends on Task 7 trait settling)
11 → Delete SessionError             (independent)
12 → SOURCE_EXTENSIONS in bootstrap  (independent)
13 → AllowedDirs newtype             (depends on Task 4/7 struct settling)
```
