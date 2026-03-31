# Plan Mode Implementation

## Overview

Transform the current stub plan mode (tool lockout + one-line system prompt) into a
full structured planning flow: intent detection → research pass → plan file written to
disk → multi-choice interview TUI → iterative refinement → execute/follow-up/cancel.

---

## State machine

```
IDLE
  └─(heuristic hit, first msg)──auto-enter──► RESEARCH
  └─(heuristic hit, mid-session)──gate──y──► RESEARCH
                                         └─n──► IDLE (send as normal turn)

RESEARCH  (plan mode active, model researches codebase, writes plan file, emits questions JSON)
  └─(questions found)──────────────────────► INTERVIEW
  └─(questions: [])────────────────────────► PLAN_READY
  └─(no JSON block after retry)────────────► error status, stay RESEARCH

INTERVIEW  (alt-screen TUI, user answers N questions)
  └─(answers submitted)────────────────────► REFINE

REFINE  (answers injected as user turn, model updates plan file, emits questions JSON)
  └─(more questions)───────────────────────► INTERVIEW
  └─(questions: [])────────────────────────► PLAN_READY

PLAN_READY  (inline y/f/n prompt in status bar)
  └─y──► exit plan mode, send "Execute the plan at <path>" as next user turn
  └─f──► inject follow-up prompt, transition to REFINE
  └─n──► stay in plan mode, user can keep chatting
```

`PlanPhase` enum lives on `AgentSession` alongside `plan_mode: bool` and `plan_path: Option<PathBuf>`.

---

## Files to create

| File | Purpose |
|---|---|
| `src/interactive/plan_interview.rs` | Alt-screen multiple-choice interview TUI |

## Files to modify

| File | Changes |
|---|---|
| `src/core/agent_session.rs` | `PlanPhase` enum, `plan_path`, `plan_phase` fields; `set_plan_mode` updated; question extraction after each turn; correction-retry turn; `PlanQuestionsReady` / `PlanReady` events |
| `src/core/system_prompt.rs` | Structured plan-mode prompt block with `plan_path`, format instructions, question JSON schema |
| `src/core/permissions.rs` | `check_with_plan_path`: write/edit to `plan_path` allowed without gate in plan mode |
| `src/core/session_runner.rs` | Handle `SetPlanMode` already exists; add `ConfirmPlanMode` command for mid-session gate response |
| `src/interactive/event_loop.rs` | Intent heuristic in `prepare_prompt`; mid-session gate (reuses `pending_permission` channel); handle `PlanQuestionsReady` (launch TUI), `PlanReady` (y/f/n prompt), `PlanModeAutoEntered`; inject answer turns; `plan_phase` mirrored on `InteractiveState` |
| `src/interactive/footer.rs` | `PLAN` tag already rendered; add `INTERVIEW` phase indicator during TUI |
| `src/interactive/mod.rs` | Export `plan_interview` |
| `src/lib.rs` | Re-export if needed |

---

## Step-by-step implementation

### Step 1 — `PlanPhase` enum + fields on `AgentSession`

In `src/core/agent_session.rs`, add above the struct:

```rust
#[derive(Debug, Clone, PartialEq, Default)]
pub enum PlanPhase {
    #[default]
    Idle,
    Research,
    Interview,
    Refine,
    Ready,
}
```

Add to `AgentSession` struct:
```rust
pub plan_phase: PlanPhase,
pub plan_path: Option<PathBuf>,
```

Initialise both to `Default` in `AgentSession::new`.

---

### Step 2 — New `AgentSessionEvent` variants

In the `AgentSessionEvent` enum, add:

```rust
PlanModeAutoEntered,           // heuristic fired on first message, no gate needed
PlanQuestionsReady {
    questions: Vec<PlanQuestion>,
    plan_path: PathBuf,
    phase: PlanPhase,          // Interview or Refine (for footer label)
},
PlanReady {
    plan_path: PathBuf,
},
```

`PlanQuestion` is a new struct (same file or `agent/types.rs`):
```rust
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PlanQuestion {
    pub q: String,
    pub options: Vec<String>,  // 2–4 items; TUI always appends "✎ Custom…"
}
```

---

### Step 3 — `set_plan_mode` updated

```rust
pub fn set_plan_mode(&mut self, enabled: bool, cwd: &Path, session_id: &str,
                     event_tx: &Sender<AgentSessionEvent>) {
    self.plan_mode = enabled;
    if enabled {
        // Derive plan path: ~/.nerv/repos/<slug>/plan-<session-id>.md
        let slug = cwd.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase()
            .replace(' ', "-");
        let nerv_dir = crate::home_dir().unwrap().join(".nerv").join("repos").join(slug);
        let _ = std::fs::create_dir_all(&nerv_dir);
        self.plan_path = Some(nerv_dir.join(format!("plan-{}.md", session_id)));
        self.plan_phase = PlanPhase::Research;

        // Allow all read tools + write/edit (gated to plan_path in permissions)
        self.tool_registry.set_active(&[
            "read", "bash", "grep", "find", "ls", "symbols", "codemap",
            "write", "edit", "memory",
        ]);
    } else {
        self.plan_path = None;
        self.plan_phase = PlanPhase::Idle;
        self.tool_registry.set_active(&[]);
    }
    let _ = event_tx.send(AgentSessionEvent::PlanModeChanged { enabled });
}
```

---

### Step 4 — Question extraction after each agent turn

At the end of `AgentSession::prompt` (after `run_agent_prompt` returns, before compaction
check), when `self.plan_mode` is true, scan the last assistant message for the JSON block:

```rust
if self.plan_mode {
    self.handle_plan_turn(event_tx);
}
```

`handle_plan_turn`:

```rust
fn handle_plan_turn(&mut self, event_tx: &Sender<AgentSessionEvent>) {
    let last_text = self.last_assistant_text(); // helper: scan messages tail for Assistant text

    match extract_plan_questions(&last_text) {
        Some(questions) if questions.is_empty() => {
            // Model is satisfied
            self.plan_phase = PlanPhase::Ready;
            let _ = event_tx.send(AgentSessionEvent::PlanReady {
                plan_path: self.plan_path.clone().unwrap(),
            });
        }
        Some(questions) => {
            self.plan_phase = PlanPhase::Interview;
            let _ = event_tx.send(AgentSessionEvent::PlanQuestionsReady {
                questions,
                plan_path: self.plan_path.clone().unwrap(),
                phase: self.plan_phase.clone(),
            });
        }
        None => {
            // No JSON block found — inject correction turn and re-run once
            if self.plan_correction_attempts < 1 {
                self.plan_correction_attempts += 1;
                let correction = AgentMessage::User {
                    content: vec![ContentItem::Text {
                        text: "Your response did not include the required questions JSON block. \
                               Reply with ONLY the JSON, no prose: \
                               {\"questions\": [{\"q\": \"...\", \"options\": [\"...\"]}, ...]} \
                               Use an empty array if you have no further questions."
                            .to_string(),
                    }],
                    timestamp: now_millis(),
                };
                self.run_agent_prompt(vec![correction], event_tx);
                self.handle_plan_turn(event_tx); // recurse once
            } else {
                self.plan_correction_attempts = 0;
                self.status_error(event_tx, "Plan mode: model did not emit questions JSON");
            }
        }
    }
}
```

Add `plan_correction_attempts: u8` to `AgentSession` struct.

`extract_plan_questions(text: &str) -> Option<Vec<PlanQuestion>>`:
- Scan for last `{` in the text that starts a valid JSON object with a `"questions"` key
- Use `serde_json::from_str` on the candidate substring
- Return `None` if no valid block found, `Some(vec)` (possibly empty) if found
- Validate each question: `options` length 2–4, `q` non-empty

---

### Step 5 — Permissions: allow write/edit to `plan_path`

Add a thread-local or pass-through:

```rust
// In AgentSession::prepare_callbacks / run_agent_prompt, pass plan_path into the
// permission check closure already constructed there.
```

The permission closure in `agent.rs` calls `permissions::check_with_allowed_dirs`.
Add a new wrapper called from the same closure:

```rust
// src/core/permissions.rs
pub fn check_in_plan_mode(
    tool: &str,
    args: &serde_json::Value,
    plan_path: &Path,
) -> Permission {
    if tool == "write" || tool == "edit" {
        if let Some(p) = args["path"].as_str() {
            let candidate = std::fs::canonicalize(p)
                .unwrap_or_else(|_| PathBuf::from(p));
            if candidate == plan_path {
                return Permission::Allow;
            }
        }
        // All other writes blocked in plan mode
        return Permission::Deny("only the plan file may be written in plan mode".into());
    }
    // Reads always allowed in plan mode
    Permission::Allow
}
```

The permission callback in `agent_session.rs` is constructed in `prepare_callbacks`.
When `self.plan_mode`, replace the write path through `check_with_allowed_dirs` with
`check_in_plan_mode` first, then fall back.

---

### Step 6 — System prompt for plan mode

In `src/core/system_prompt.rs`, replace the current plan-mode paragraph with:

```
# Plan Mode

You are in plan mode. Follow this protocol exactly:

**Phase: Research & Draft**
1. Research the codebase using read, grep, find, ls, symbols, codemap, bash.
2. Write your initial plan to: `{plan_path}`
   Use clear markdown: ## Goal, ## Approach, ## Steps, ## Open Questions.
3. Identify 2–7 specific questions that would meaningfully improve the plan.
   Questions must have 2–4 concrete options each.

**Required: end every response with this JSON block — no exceptions:**
```json
{"questions": [{"q": "Question text", "options": ["Option A", "Option B", "Option C"]}, ...]}
```
Use an empty array when you have enough information to finalise the plan:
```json
{"questions": []}
```

**Constraints**
- Only write to `{plan_path}`. No other file mutations.
- Do not include an "Other" or "Custom" option — that is added automatically.
- Provide exactly the JSON block as the last thing in your response, after all prose.
```

---

### Step 7 — Intent heuristic in `event_loop.rs`

In `InteractiveState::prepare_prompt` (the method called before `cmd_tx.send(Prompt)`),
before the existing bare-"plan" check:

```rust
fn should_auto_plan(text: &str) -> bool {
    let t = text.trim().to_lowercase();
    // Must be a substantive message
    if t.len() < 20 { return false; }
    const TRIGGERS: &[&str] = &[
        "plan ", "design ", "architect ", "let's build", "lets build",
        "i want to build", "i want to implement", "i want to create",
        "help me build", "help me design", "help me implement",
        "how should i", "how do i", "how should we",
        "we need to", "i need to", "proposal", "spec for",
        "spec out", "plan out",
    ];
    TRIGGERS.iter().any(|t2| t.starts_with(t2) || t.contains(t2))
}
```

Usage in `prepare_prompt`:

```rust
if !self.plan_mode && should_auto_plan(&text) {
    let is_first_message = self.agent.state.messages.is_empty();
    if is_first_message {
        // Auto-enter, no gate
        self.plan_mode = true;
        let _ = self.cmd_tx.try_send(SessionCommand::SetPlanMode { enabled: true });
        let _ = event_tx.send(AgentSessionEvent::PlanModeAutoEntered);
    } else {
        // Mid-session: gate via existing permission channel
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.pending_permission = Some(tx);
        self.pending_permission_details = None;
        self.status_message = Some(
            "Switch to plan mode for this request?  y = yes, n = no (send as normal turn)"
                .into()
        );
        self.status_is_error = false;
        // Stash the text so the y/n handler can re-send it
        self.plan_pending_text = Some(text.clone());
        // The y/n handler in main.rs reads pending_permission; extend it:
        // y → SetPlanMode { enabled: true } then re-send stashed text
        // n → send text as normal turn
        return None; // don't send yet
    }
}
```

Add `plan_pending_text: Option<String>` to `InteractiveState`.

In `main.rs`, in the `pending_permission` handler, after `tx.send(approved)`:

```rust
if approved {
    if let Some(text) = interactive.plan_pending_text.take() {
        let _ = interactive.cmd_tx.try_send(SessionCommand::SetPlanMode { enabled: true });
        let _ = interactive.cmd_tx.try_send(SessionCommand::Prompt { text });
    }
} else {
    if let Some(text) = interactive.plan_pending_text.take() {
        let _ = interactive.cmd_tx.try_send(SessionCommand::Prompt { text });
    }
}
```

---

### Step 8 — `plan_interview.rs` — alt-screen multiple-choice TUI

New file: `src/interactive/plan_interview.rs`

Public API:
```rust
pub struct PlanAnswer {
    pub question: String,
    pub answer: String,      // chosen option text, or "[custom] <freeform>"
}

/// Enter alt screen, walk through questions, return answers.
/// Returns None if user cancelled (Escape / Ctrl-C).
pub fn run_plan_interview(
    questions: &[PlanQuestion],
    plan_path: &Path,
) -> Option<Vec<PlanAnswer>>
```

Internal state:
```rust
struct InterviewState {
    questions: Vec<PlanQuestion>,
    current: usize,
    answers: Vec<Option<PlanAnswer>>,    // None = unanswered / skipped
    cursor: usize,                       // focused option index (0..options.len()+1)
    freeform_mode: bool,
    freeform_buf: String,
    plan_path: PathBuf,
}
```

#### Layout (per question screen)

```
  Plan: ~/.nerv/repos/nerv/plan-a1b2c3.md

  Question 2 of 5 ───────────────────────────────────────────

  How should authentication tokens be stored?

    ● Keychain
    ○ Environment variable
    ○ Config file (~/.nerv/auth)
    ○ ✎ Custom answer...

  [↑/↓] move   [Enter] select   [←/Backspace] back   [Tab] skip   [Ctrl+D] done
```

When `✎ Custom answer...` is selected and Enter pressed, replace the option list with:

```
  Your answer:
  > _
  [Enter] confirm   [Escape] back to options
```

#### Key bindings

| Key | Action |
|---|---|
| `↑` / `↓` | Move cursor between options |
| `Enter` | Select option (or confirm freeform) |
| `←` / `Backspace` (not in freeform) | Go back to previous question |
| `Tab` | Skip question (answer = empty string) |
| `Ctrl+D` | Submit all answers so far and exit |
| `Escape` / `Ctrl+C` | Cancel entire interview (returns `None`) |

#### Rendering

Use the same alt-screen pattern as `fullscreen_picker.rs`:
- `\x1b[?1049h\x1b[?25l` on entry
- `\x1b[?2026h\x1b[H\x1b[2J` per frame (synchronized output)
- `\x1b[?25h\x1b[?1049l` on exit

Filled bullet `●` for focused option, empty `○` for others.
Use `theme::ACCENT_BOLD` for the focused option, `theme::DIM` for the others.

---

### Step 9 — Event handling in `event_loop.rs`

```rust
AgentSessionEvent::PlanModeAutoEntered => {
    self.plan_mode = true;
    layout.footer.set_plan_mode(true);
    self.status_message = Some("Plan mode — researching…".into());
}

AgentSessionEvent::PlanQuestionsReady { questions, plan_path, .. } => {
    // Pause stdin reader, launch interview TUI, resume
    // (same pattern as session_picker / model_picker)
    let answers = run_plan_interview(&questions, &plan_path);
    if let Some(answers) = answers {
        let body = format_answers(&answers);
        let _ = self.cmd_tx.try_send(SessionCommand::Prompt { text: body });
    }
    // else: user cancelled, stay in plan mode
}

AgentSessionEvent::PlanReady { plan_path } => {
    self.status_message = Some(format!(
        "Plan ready: {}  Execute plan? y = execute, f = dig deeper, n = keep editing",
        plan_path.display()
    ));
    self.plan_ready_path = Some(plan_path);
    self.pending_plan_ready = true;
}
```

Add `plan_ready_path: Option<PathBuf>` and `pending_plan_ready: bool` to `InteractiveState`.

In `main.rs` key handler, when `pending_plan_ready`:

```rust
if interactive.pending_plan_ready {
    if seq == b"y" || seq == b"Y" {
        interactive.pending_plan_ready = false;
        let path = interactive.plan_ready_path.take().unwrap();
        let _ = interactive.cmd_tx.try_send(SessionCommand::SetPlanMode { enabled: false });
        let _ = interactive.cmd_tx.try_send(SessionCommand::Prompt {
            text: format!("Execute the plan described in {}.", path.display()),
        });
    } else if seq == b"f" || seq == b"F" {
        interactive.pending_plan_ready = false;
        interactive.plan_ready_path = None;
        let _ = interactive.cmd_tx.try_send(SessionCommand::Prompt {
            text: "The plan needs more depth. Ask me targeted clarifying questions \
                   to sharpen it further.".into(),
        });
    } else if seq == b"n" || seq == b"N" {
        interactive.pending_plan_ready = false;
        interactive.plan_ready_path = None;
        interactive.status_message = None;
    }
    continue;
}
```

---

### Step 10 — Answer formatting

```rust
fn format_answers(answers: &[PlanAnswer]) -> String {
    let mut out = String::from(
        "Here are my answers to your questions. \
         Please update the plan file to incorporate them, then indicate \
         whether you need further clarification.\n\n"
    );
    for (i, a) in answers.iter().enumerate() {
        out.push_str(&format!("{}. {}\n   Answer: {}\n\n", i + 1, a.question, a.answer));
    }
    out
}
```

Skipped questions (empty answer): `Answer: (no preference — use your judgment)`

---

## New fields summary

### `AgentSession`
- `plan_phase: PlanPhase`
- `plan_path: Option<PathBuf>`
- `plan_correction_attempts: u8`

### `InteractiveState`
- `plan_pending_text: Option<String>` — stashed mid-session text while gate is open
- `plan_ready_path: Option<PathBuf>`
- `pending_plan_ready: bool`

---

## Testing plan

1. **Heuristic fires on first message**: type "I want to build a thing" → plan mode auto-entered, no gate
2. **Heuristic fires mid-session**: send a message after history exists → y/n gate appears
3. **Questions JSON extracted**: model emits block → interview TUI launches
4. **Missing JSON correction**: inject a message with no JSON block → correction turn fires, second attempt succeeds
5. **Multiple choice + freeform**: navigate options, select custom, type freeform → answer formatted correctly
6. **Empty questions (plan ready)**: model emits `[]` → y/f/n prompt appears
7. **y → execute**: plan mode exits, execute turn sent
8. **f → follow-up**: refine turn injected, model asks more questions
9. **Write gate**: model attempts to write to non-plan file → `Permission::Deny`
10. **Write to plan file**: model writes to correct path → `Permission::Allow`, no gate
