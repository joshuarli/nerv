use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::sync::atomic::Ordering;

/// Phase of the plan-mode state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanPhase {
    /// Model is researching + writing the initial plan draft.
    Research,
    /// Waiting for the user to answer the model's interview questions.
    Interview,
    /// Model is refining the plan based on user answers.
    Refine,
    /// Plan is complete — waiting for user to choose execute / follow-up / cancel.
    Ready,
}

/// A single interview question emitted by the model.
#[derive(Debug, Clone)]
pub struct PlanOption {
    pub label: String,
    /// One-line tradeoff note shown as subtext under the choice.
    pub subtext: String,
    /// Whether the model recommends this option.
    pub recommended: bool,
}

#[derive(Debug, Clone)]
pub struct PlanQuestion {
    pub q: String,
    pub options: Vec<PlanOption>,
}

/// Typed wrapper around the shared allowed-directories list.
///
/// Directories added here are granted full tool access without per-call prompts.
/// The Arc makes it cheap to clone into closures.
#[derive(Clone, Default)]
pub struct AllowedDirs(Arc<std::sync::Mutex<Vec<PathBuf>>>);

impl AllowedDirs {
    pub fn push(&self, dir: PathBuf) {
        self.0.lock().unwrap().push(dir);
    }
    pub fn snapshot(&self) -> Vec<PathBuf> {
        self.0.lock().unwrap().clone()
    }
}


use crossbeam_channel::Sender;

use super::model_registry::ModelRegistry;
use super::resource_loader::LoadedResources;
use super::system_prompt::build_system_prompt_for_model;
use super::tool_registry::ToolRegistry;
use crate::agent::agent::Agent;
use crate::agent::provider::Provider;
use crate::agent::types::{
    AgentEvent, AgentMessage, AssistantMessage, ContentItem, Cost, EffortLevel, Model,
    ModelPricing, StopReason, ThinkingLevel,
};
use crate::now_millis;
use crate::compaction::summarize::{generate_session_name, generate_summary};
use crate::compaction::{self, CompactionResult};
use super::compaction_controller::CompactionController;
use crate::core::config::NervConfig;
use crate::session::SessionManager;

#[derive(Debug, Clone, Copy)]
pub enum CompactionReason {
    Overflow,
    Threshold,
    Manual,
}

#[derive(Debug)]
pub enum AgentSessionEvent {
    Agent(AgentEvent),
    AutoCompactionStart {
        reason: CompactionReason,
    },
    AutoCompactionEnd {
        summary: Option<String>,
        /// Parsed structured summary, if JSON parse succeeded. `None` when
        /// compaction failed/skipped or the LLM returned prose.
        structured: Option<crate::compaction::summarize::StructuredSummary>,
        will_retry: bool,
        /// Post-compaction messages (for UI rebuild). Empty when compaction
        /// failed/skipped.
        messages: Vec<AgentMessage>,
    },
    RetryStart {
        attempt: u32,
        delay_ms: u64,
    },
    RetryEnd {
        success: bool,
    },
    ModelChanged {
        model: Model,
    },
    ThinkingLevelChanged {
        level: ThinkingLevel,
    },
    EffortLevelChanged {
        level: Option<EffortLevel>,
    },
    ExportDone {
        result: Result<String, String>,
    },
    Status {
        message: String,
        is_error: bool,
    },
    SessionList {
        sessions: Vec<crate::session::manager::SessionSummary>,
    },
    TreeData {
        tree: Vec<crate::session::types::SessionTreeNode>,
        current_leaf: Option<String>,
    },
    /// A session is now active (created or loaded).
    SessionStarted {
        id: String,
        /// Existing name, if any (populated on resume; None for new sessions).
        name: Option<String>,
    },
    /// Session loaded — clear UI and display history.
    SessionLoaded {
        messages: Vec<AgentMessage>,
        /// Accumulated cost in USD for this session (restored from DB on load).
        cost_usd: f64,
        /// Total input tokens sent across all API calls (restored from DB).
        total_input: u64,
        /// Total output tokens received across all API calls (restored from
        /// DB).
        total_output: u64,
        /// Number of API calls made in this session (restored from DB).
        api_calls: u32,
        /// All user-typed prompts ever submitted in this session (for up-arrow
        /// recall).
        input_history: Vec<String>,
    },
    /// A worktree was created (via /wt). UI should update cwd display.
    WorktreeCreated {
        path: PathBuf,
    },
    /// Worktree was merged and removed. UI should update cwd back.
    WorktreeMerged {
        original_path: PathBuf,
        message: String,
    },
    /// Provider health check result (from background thread on startup).
    ProviderHealth {
        provider: String,
        online: bool,
    },
    PlanModeChanged {
        enabled: bool,
    },
    /// Model produced interview questions — UI should launch the interview TUI.
    PlanQuestionsReady {
        questions: Vec<PlanQuestion>,
        plan_path: PathBuf,
    },
    /// Plan phase advanced (Research → Interview → Refine → Ready).
    PlanPhaseChanged {
        phase: PlanPhase,
    },
    /// Session title was generated after the first completed turn.
    SessionNamed {
        name: String,
    },
    /// Auto-compact threshold changed for this session (0–100).
    CompactThresholdChanged {
        pct: u8,
    },
    PermissionRequest {
        tool: String,
        args: serde_json::Value,
        reason: String,
        response_tx: crossbeam_channel::Sender<bool>,
    },
    /// Context gate — agent blocks until user confirms or denies the large
    /// request.
    ContextGateRequest {
        estimated_tokens: usize,
        prev_tokens: usize,
        context_window: u32,
        response_tx: crossbeam_channel::Sender<bool>,
    },
    /// Output gate — bash result exceeded OUTPUT_GATE_THRESHOLD_BYTES after
    /// filtering. Agent blocks until user allows or denies adding the
    /// result to context.
    OutputGateRequest {
        command: String,
        line_count: usize,
        estimated_tokens: usize,
        response_tx: crossbeam_channel::Sender<bool>,
    },
}

pub enum SessionCommand {
    Prompt {
        text: String,
    },
    Abort,
    NewSession,
    LoadSession {
        id: String,
    },
    SetModel {
        provider: String,
        model_id: String,
    },
    SetThinkingLevel {
        level: ThinkingLevel,
    },
    SetEffortLevel {
        level: Option<EffortLevel>,
    },
    Compact {
        custom_instructions: Option<String>,
    },
    SetCompactThreshold {
        pct: u8,
    },
    SetAutoCompact {
        enabled: bool,
    },
    Export,
    Login {
        provider: String,
    },
    Logout {
        provider: String,
    },
    ListSessions {
        repo_root: Option<String>,
        repo_id: Option<String>,
    },
    GetTree,
    SwitchBranch {
        entry_id: String,
        /// If true, set leaf to the *parent* of entry_id instead (user message
        /// re-submission).
        use_parent: bool,
        /// If true, reset leaf to None (root user message selected).
        reset_leaf: bool,
    },
    CreateWorktree {
        branch_name: String,
        nerv_dir: PathBuf,
    },
    MergeWorktree,
    SetPlanMode {
        enabled: bool,
    },
    /// Submit answers from the interview TUI back to the agent.
    PlanAnswers {
        answers: Vec<(String, String)>,
    },
    /// User chose to dig deeper (f) — ask the model for more questions.
    PlanFollowUp,
    /// Exit plan mode and immediately prompt the agent to implement the plan.
    ExecutePlan,
    ForkSession,
    /// Persist the full input history for the current session.
    SaveInputHistory {
        history: Vec<String>,
    },
    /// Record a completed /btw call to the session history.
    RecordBtw {
        note: String,
        response: String,
        model_id: String,
    },
}

pub struct AgentSession {
    pub agent: Agent,
    pub session_manager: SessionManager,
    pub tool_registry: ToolRegistry,
    /// Config loaded once at startup. Immutable for the lifetime of the session.
    pub config: NervConfig,
    /// Compaction state: settings, threshold, auto-compact flag, and the
    /// mid-stream trigger flag. Grouped to make hand-off to closures obvious.
    pub compaction: CompactionController,
    pub(crate) model_registry: Arc<ModelRegistry>,
    pub(crate) resources: LoadedResources,
    pub(crate) cwd: PathBuf,
    pub(crate) session_cost: Cost,
    last_input_tokens: u32,
    pub permissions_enabled: bool,
    /// Cache of accepted permissions: (tool, args_json) keyed by args hash
    /// Shared arc for use in permission_fn closure
    permission_cache: Arc<std::sync::Mutex<HashSet<String>>>,
    /// Directories the user has granted full access to via the "allow dir"
    /// prompt response. Shared with the main thread so new entries can be
    /// pushed from the UI.
    pub allowed_dirs: AllowedDirs,
    /// Worktree path tied to this session (set via --wt or /wt).
    pub(crate) worktree: Option<PathBuf>,
    /// Plan mode: restrict tools to read-only, steer model toward planning.
    plan_mode: bool,
    /// Path of the plan file for the current plan-mode session.
    pub plan_path: Option<PathBuf>,
    /// Current phase of the plan-mode state machine.
    pub plan_phase: PlanPhase,
    /// Talk mode: no tools, no project context, pure conversational assistant.
    pub talk_mode: bool,
    /// True once the session has been given an auto-generated name, to avoid
    /// re-naming.
    pub(crate) session_named: bool,
    /// Shared slot for mid-turn message injection. The event loop writes a
    /// queued message here at TurnEnd; the agent loop picks it up before the
    /// next API call. Cloned from agent.midturn_inject on construction.
    pub midturn_inject: Arc<Mutex<Option<String>>>,
}

impl AgentSession {
    pub fn new(
        agent: Agent,
        session_manager: SessionManager,
        tool_registry: ToolRegistry,
        model_registry: Arc<ModelRegistry>,
        resources: LoadedResources,
        cwd: PathBuf,
        config: NervConfig,
    ) -> Self {
        let midturn_inject = agent.midturn_inject.clone();
        Self {
            agent,
            session_manager,
            tool_registry,
            config,
            compaction: CompactionController::default(),
            model_registry,
            resources,
            cwd,
            session_cost: Cost::default(),
            last_input_tokens: 0,
            permissions_enabled: false,
            permission_cache: Arc::new(std::sync::Mutex::new(HashSet::new())),
            allowed_dirs: AllowedDirs::default(),
            worktree: None,
            plan_mode: false,
            plan_path: None,
            plan_phase: PlanPhase::Research,
            talk_mode: false,
            session_named: false,
            midturn_inject,
        }
    }

    /// Resolve (and cache) the plan file path. Called lazily so the session ID
    /// exists by the time we need it.
    fn resolve_plan_path(&mut self) -> PathBuf {
        if let Some(ref p) = self.plan_path {
            return p.clone();
        }
        let session_id = self.session_manager.session_id().to_string();
        let repo_dir = crate::repo_data_dir(&self.cwd);
        let _ = std::fs::create_dir_all(&repo_dir);
        let path = repo_dir.join(format!("plan-{}.md", session_id));
        self.plan_path = Some(path.clone());
        path
    }

    pub fn set_plan_mode(&mut self, enabled: bool, event_tx: &Sender<AgentSessionEvent>) {
        self.plan_mode = enabled;
        if enabled {
            // plan_path is resolved lazily (session ID may not exist yet).
            self.plan_phase = PlanPhase::Research;
            // Read-only tools + write + edit (write/edit are gated to plan_path only
            // via permissions).
            self.tool_registry.set_active(&[
                "read", "bash", "grep", "find", "ls", "symbols", "codemap", "memory",
                "write", "edit",
            ]);
        } else {
            self.plan_path = None;
            self.plan_phase = PlanPhase::Research;
            self.tool_registry.set_active(&[]);
        }
        let _ = event_tx.send(AgentSessionEvent::PlanModeChanged { enabled });
    }

    /// Extract a `{"questions":[...]}` JSON block from the tail of an assistant
    /// response. Returns `Some(vec)` (possibly empty) when the block is present,
    /// `None` when absent.
    fn extract_questions(text: &str) -> Option<Vec<PlanQuestion>> {
        // Find the last `{` that starts a JSON object containing "questions".
        let start = text.rfind("{\"questions\"")?;
        let fragment = &text[start..];
        // Find the matching closing brace (simple depth counter).
        let mut depth = 0usize;
        let mut end = None;
        for (i, ch) in fragment.char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        end = Some(i + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        let json_str = &fragment[..end?];
        let val: serde_json::Value = serde_json::from_str(json_str).ok()?;
        let arr = val.get("questions")?.as_array()?;
        let questions = arr
            .iter()
            .filter_map(|item| {
                let obj = item.as_object()?;
                let q = obj.get("q")?.as_str()?.to_string();
                let options = obj
                    .get("options")
                    .and_then(|o| o.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| {
                                if let Some(s) = v.as_str() {
                                    // Backward-compat: plain string option.
                                    Some(PlanOption {
                                        label: s.to_string(),
                                        subtext: String::new(),
                                        recommended: false,
                                    })
                                } else if let Some(o) = v.as_object() {
                                    let label = o.get("label")?.as_str()?.to_string();
                                    let subtext = o.get("subtext")
                                        .and_then(|s| s.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    let recommended = o.get("recommended")
                                        .and_then(|r| r.as_bool())
                                        .unwrap_or(false);
                                    Some(PlanOption { label, subtext, recommended })
                                } else {
                                    None
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Some(PlanQuestion { q, options })
            })
            .collect();
        Some(questions)
    }

    /// Build and send the answer injection prompt after the interview.
    pub fn inject_plan_answers(
        &mut self,
        answers: Vec<(String, String)>,
        event_tx: &Sender<AgentSessionEvent>,
    ) {
        let plan_path = self.resolve_plan_path().display().to_string();
        let mut text = String::from("Here are my answers:\n\n");
        for (i, (_question, answer)) in answers.iter().enumerate() {
            if answer.is_empty() {
                text.push_str(&format!("{}. (no preference — use your judgment)\n", i + 1));
            } else {
                text.push_str(&format!("{}. {}\n", i + 1, answer));
            }
        }
        text.push('\n');
        text.push_str(&format!(
            "Please update the plan file at `{}` with a refined version incorporating these answers. \
             Then either output a new questions JSON block if you need more clarification, or \
             output `{{\"questions\":[]}}` when the plan is complete.",
            plan_path
        ));
        self.plan_phase = PlanPhase::Refine;
        let _ = event_tx.send(AgentSessionEvent::PlanPhaseChanged { phase: PlanPhase::Refine });
        self.prompt(text, event_tx);
    }

    /// Inject a follow-up prompt asking the model to dig deeper.
    pub fn inject_plan_followup(&mut self, event_tx: &Sender<AgentSessionEvent>) {
        let text =
            "The plan needs more depth. Please ask me more targeted clarifying questions \
             to sharpen it further. Use the same questions JSON format."
                .to_string();
        self.plan_phase = PlanPhase::Refine;
        let _ = event_tx.send(AgentSessionEvent::PlanPhaseChanged { phase: PlanPhase::Refine });
        self.prompt(text, event_tx);
    }

    pub fn set_worktree(&mut self, path: PathBuf) {
        self.cwd = path.clone();
        self.worktree = Some(path);
    }

    pub fn cost(&self) -> &Cost {
        &self.session_cost
    }

    pub fn prompt(&mut self, text: String, event_tx: &Sender<AgentSessionEvent>) {
        // Lazily create session on first prompt (not on startup)
        if !self.session_manager.has_session() {
            let _ = self.session_manager.new_session(&self.cwd, self.worktree.as_deref());
            let _ = event_tx.send(AgentSessionEvent::SessionStarted {
                id: self.session_manager.session_id().to_string(),
                name: None,
            });
        }

        let user_msg = AgentMessage::User {
            content: vec![ContentItem::Text { text: text.clone() }],
            timestamp: now_millis(),
        };

        self.prepare_system_prompt();

        // Record system prompt in session for reproducibility
        let prompt_tokens = crate::compaction::count_tokens(&self.agent.state.system_prompt) as u32;
        let _ =
            self.session_manager.append_entry(crate::session::types::SessionEntry::SystemPrompt(
                crate::session::types::SystemPromptEntry {
                    id: crate::session::types::gen_entry_id(),
                    parent_id: self.session_manager.leaf_id().map(|s| s.to_string()),
                    timestamp: crate::session::types::now_iso(),
                    prompt: self.agent.state.system_prompt.clone(),
                    token_count: prompt_tokens,
                },
            ));

        self.prepare_callbacks(event_tx);

        // Reset in case a previous prompt left it set (e.g. abort during compaction).
        self.compaction.reset_triggered();
        let new_messages = self.run_agent_prompt(vec![user_msg], event_tx);

        // Mid-stream auto-compaction: the on_event callback detected the context
        // exceeded the threshold and cancelled the stream. Compact now and retry.
        if self.compaction.check_and_clear_triggered() {
            crate::log::info("mid-stream auto-compact: running compaction");
            let _ = event_tx.send(AgentSessionEvent::AutoCompactionStart {
                reason: CompactionReason::Threshold,
            });

            match self.run_compaction(None) {
                Ok(compaction::CompactionOutcome::Full(result)) => {
                    self.reload_agent_context();
                    let _ = event_tx.send(AgentSessionEvent::AutoCompactionEnd {
                        summary: Some(result.summary),
                        structured: result.structured,
                        will_retry: true,
                        messages: self.agent.state.messages.clone(),
                    });

                    // Retry: re-send the user prompt so the model picks up
                    // where it left off with the compacted context.
                    // Temporarily disable auto-compact for the retry to avoid
                    // infinite loops if compaction didn't shrink enough.
                    let retry_msg = AgentMessage::User {
                        content: vec![ContentItem::Text { text: text.to_string() }],
                        timestamp: now_millis(),
                    };
                    self.prepare_system_prompt();
                    self.prepare_callbacks(event_tx);
                    let saved_auto_compact = self.compaction.auto_compact;
                    self.compaction.auto_compact = false;
                    let retry_messages = self.run_agent_prompt(vec![retry_msg], event_tx);
                    self.compaction.auto_compact = saved_auto_compact;
                    self.post_turn(retry_messages, &text, event_tx);
                }
                Ok(compaction::CompactionOutcome::LiteCompact { zeroed }) => {
                    // Messages already mutated in place — no reload needed.
                    let _ = event_tx.send(AgentSessionEvent::AutoCompactionEnd {
                        summary: Some(format!("Lite-compact: {zeroed} stale outputs cleared")),
                        structured: None,
                        will_retry: true,
                        messages: self.agent.state.messages.clone(),
                    });
                    let retry_msg = AgentMessage::User {
                        content: vec![ContentItem::Text { text: text.to_string() }],
                        timestamp: now_millis(),
                    };
                    self.prepare_system_prompt();
                    self.prepare_callbacks(event_tx);
                    let saved_auto_compact = self.compaction.auto_compact;
                    self.compaction.auto_compact = false;
                    let retry_messages = self.run_agent_prompt(vec![retry_msg], event_tx);
                    self.compaction.auto_compact = saved_auto_compact;
                    self.post_turn(retry_messages, &text, event_tx);
                }
                Ok(compaction::CompactionOutcome::None) => {
                    let _ = event_tx.send(AgentSessionEvent::AutoCompactionEnd {
                        summary: None,
                        structured: None,
                        will_retry: false,
                        messages: vec![],
                    });
                    let _ = event_tx.send(AgentSessionEvent::Status {
                        message: "Auto-compact triggered but nothing to compact.".into(),
                        is_error: false,
                    });
                    // Fall through to normal post_turn with the original messages.
                    self.post_turn(new_messages, &text, event_tx);
                }
                Err(e) => {
                    let _ = event_tx.send(AgentSessionEvent::AutoCompactionEnd {
                        summary: None,
                        structured: None,
                        will_retry: false,
                        messages: vec![],
                    });
                    let _ = event_tx.send(AgentSessionEvent::Status {
                        message: format!("Auto-compact failed: {e}"),
                        is_error: true,
                    });
                    self.post_turn(new_messages, &text, event_tx);
                }
            }
            return;
        }

        self.post_turn(new_messages, &text, event_tx);
    }

    /// Wire up permission_fn and context_gate_fn on the agent before a prompt.
    fn prepare_callbacks(&mut self, event_tx: &Sender<AgentSessionEvent>) {
        // Permission checking
        if self.permissions_enabled {
            let repo_root = crate::find_repo_root(&self.cwd);
            let perm_tx = event_tx.clone();
            let cache = self.permission_cache.clone();
            let allowed_dirs = self.allowed_dirs.clone();
            let notifications = self.config.notifications.clone();
            // Write/edit to the plan file are always allowed without a gate.
            let plan_path = Some(self.resolve_plan_path());

            self.agent.set_permission_fn(
                Some(std::sync::Arc::new(move |tool: &str, args: &serde_json::Value| {
                    // In plan mode, allow write/edit unconditionally to the plan file.
                    if (tool == "write" || tool == "edit") && let Some(ref pp) = plan_path {
                        let target = args["path"].as_str().unwrap_or("");
                        let target_path = std::path::Path::new(target);
                        if target_path == pp.as_path() {
                            return true;
                        }
                        // Absolute comparison via canonicalize fallback
                        let canon_target = std::fs::canonicalize(target_path)
                            .unwrap_or_else(|_| target_path.to_path_buf());
                        let canon_plan = std::fs::canonicalize(pp)
                            .unwrap_or_else(|_| pp.clone());
                        if canon_target == canon_plan {
                            return true;
                        }
                    }

                    let args_json = serde_json::to_string(args).unwrap_or_default();
                    let key = permission_key(tool, &args_json);
                    if cache.lock().unwrap().contains(&key) {
                        return true;
                    }

                    let dirs = allowed_dirs.snapshot();
                    let perm = super::permissions::check_with_allowed_dirs(
                        tool,
                        args,
                        repo_root.as_deref(),
                        &dirs,
                    );
                    match perm {
                        super::permissions::Permission::Allow => true,
                        super::permissions::Permission::Ask(reason) => {
                            let reason_key = format!("reason:{}", reason);
                            if cache.lock().unwrap().contains(&reason_key) {
                                return true;
                            }

                            let (resp_tx, resp_rx) = crossbeam_channel::bounded(1);
                            let _ = perm_tx.send(AgentSessionEvent::PermissionRequest {
                                tool: tool.to_string(),
                                args: args.clone(),
                                reason: reason.clone(),
                                response_tx: resp_tx,
                            });
                            let approved = resp_rx.recv().unwrap_or(false);
                            if approved {
                                let mut c = cache.lock().unwrap();
                                c.insert(key.clone());
                                c.insert(reason_key);
                            } else {
                                // Fire onPermissionDenied hooks (fire-and-forget).
                                super::notifications::fire(
                                    super::notifications::NotificationMatcher::OnPermissionDenied,
                                    &notifications,
                                );
                            }
                            approved
                        }
                    }
                })));
        }

        // Output gate: fires after bash executes when filtered output exceeds
        // threshold. Agent thread blocks on the channel waiting for a y/n from
        // the TUI.
        {
            let output_tx = event_tx.clone();
            self.agent.set_output_gate_fn(
                Some(std::sync::Arc::new(move |info: crate::agent::agent::OutputGateInfo| {
                    let (resp_tx, resp_rx) = crossbeam_channel::bounded(1);
                    let _ = output_tx.send(AgentSessionEvent::OutputGateRequest {
                        command: info.command.clone(),
                        line_count: info.line_count,
                        estimated_tokens: info.estimated_tokens,
                        response_tx: resp_tx,
                    });
                    if resp_rx.recv().unwrap_or(false) {
                        crate::agent::agent::OutputGateDecision::Allow
                    } else {
                        crate::agent::agent::OutputGateDecision::Deny
                    }
                })));
        }

        // Context gate (circuit breaker for context growth)
        let gate_tx = event_tx.clone();
        self.agent.set_context_gate_fn(
            Some(std::sync::Arc::new(move |info: crate::agent::agent::ContextGateInfo| {
                if info.tool_rounds < 4 || info.prev_tokens == 0 {
                    return true;
                }
                let delta = info.estimated_tokens.saturating_sub(info.prev_tokens);
                if delta < 20_000 {
                    return true;
                }
                let pct = (delta as f64 / info.prev_tokens as f64) * 100.0;
                if pct <= 30.0 {
                    return true;
                }
                let (resp_tx, resp_rx) = crossbeam_channel::bounded(1);
                let _ = gate_tx.send(AgentSessionEvent::ContextGateRequest {
                    estimated_tokens: info.estimated_tokens,
                    prev_tokens: info.prev_tokens,
                    context_window: info.context_window,
                    response_tx: resp_tx,
                });
                resp_rx.recv().unwrap_or(false)
            })));
    }

    /// Handle overflow compaction and session naming after a completed prompt.
    /// Threshold-based auto-compaction is handled mid-stream (in
    /// run_agent_prompt's on_event callback), not here.
    fn post_turn(
        &mut self,
        new_messages: Vec<AgentMessage>,
        user_text: &str,
        event_tx: &Sender<AgentSessionEvent>,
    ) {
        // Context overflow → auto-compact → retry
        if let Some(last) = last_assistant(&new_messages)
            && last.stop_reason.is_context_overflow()
        {
            crate::log::info("context overflow detected, attempting auto-compact + retry");
            let _ = event_tx.send(AgentSessionEvent::AutoCompactionStart {
                reason: CompactionReason::Overflow,
            });

            match self.run_compaction(None) {
                Ok(compaction::CompactionOutcome::Full(result)) => {
                    self.reload_agent_context();
                    let _ = event_tx.send(AgentSessionEvent::AutoCompactionEnd {
                        summary: Some(result.summary),
                        structured: result.structured,
                        will_retry: true,
                        messages: self.agent.state.messages.clone(),
                    });

                    let retry_msg = AgentMessage::User {
                        content: vec![ContentItem::Text { text: user_text.to_string() }],
                        timestamp: now_millis(),
                    };
                    self.prepare_system_prompt();
                    let _retry_messages = self.run_agent_prompt(vec![retry_msg], event_tx);
                }
                Ok(compaction::CompactionOutcome::LiteCompact { zeroed }) => {
                    let _ = event_tx.send(AgentSessionEvent::AutoCompactionEnd {
                        summary: Some(format!("Lite-compact: {zeroed} stale outputs cleared")),
                        structured: None,
                        will_retry: true,
                        messages: self.agent.state.messages.clone(),
                    });

                    let retry_msg = AgentMessage::User {
                        content: vec![ContentItem::Text { text: user_text.to_string() }],
                        timestamp: now_millis(),
                    };
                    self.prepare_system_prompt();
                    let _retry_messages = self.run_agent_prompt(vec![retry_msg], event_tx);
                }
                Ok(compaction::CompactionOutcome::None) => {
                    let _ = event_tx.send(AgentSessionEvent::AutoCompactionEnd {
                        summary: None,
                        structured: None,
                        will_retry: false,
                        messages: vec![],
                    });
                    let _ = event_tx.send(AgentSessionEvent::Status {
                        message: "Context overflow: nothing to compact. Try /new to start a fresh session.".into(),
                        is_error: true,
                    });
                }
                Err(e) => {
                    let _ = event_tx.send(AgentSessionEvent::AutoCompactionEnd {
                        summary: None,
                        structured: None,
                        will_retry: false,
                        messages: vec![],
                    });
                    let _ = event_tx.send(AgentSessionEvent::Status {
                        message: format!("Context overflow: {e}"),
                        is_error: true,
                    });
                }
            }
            return;
        }

        // Session naming after first completed turn — deterministic, no model call.
        if !self.session_named && self.session_manager.name().is_none() && !user_text.is_empty() {
            let name = generate_session_name(user_text);
            self.session_manager.set_name(&name);
            self.session_named = true;
            let _ = event_tx.send(AgentSessionEvent::SessionNamed { name });
        }

        // Plan mode: extract questions from the last assistant message.
        // Only fire during Research/Refine — not once we've reached Interview or Ready.
        if self.plan_mode
            && matches!(self.plan_phase, PlanPhase::Research | PlanPhase::Refine)
        {
            self.handle_plan_turn(new_messages, event_tx);
        }
    }

    /// After a plan-mode turn, parse the questions JSON block and emit the
    /// appropriate event. If the block is missing, inject a correction prompt
    /// once. If `questions` is empty, the plan is ready.
    fn handle_plan_turn(
        &mut self,
        new_messages: Vec<AgentMessage>,
        event_tx: &Sender<AgentSessionEvent>,
    ) {
        let last_text = new_messages
            .iter()
            .rev()
            .find_map(|m| {
                if let AgentMessage::Assistant(a) = m {
                    let t: String = a
                        .content
                        .iter()
                        .filter_map(|b| {
                            if let crate::agent::types::ContentBlock::Text { text } = b {
                                Some(text.as_str())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("");
                    if t.is_empty() { None } else { Some(t) }
                } else {
                    None
                }
            });

        let Some(text) = last_text else { return };

        match Self::extract_questions(&text) {
            Some(questions) if questions.is_empty() => {
                // Model signals it has enough information — plan is ready.
                self.plan_phase = PlanPhase::Ready;
                let _ =
                    event_tx.send(AgentSessionEvent::PlanPhaseChanged { phase: PlanPhase::Ready });
            }
            Some(questions) => {
                // Questions found — move to Interview phase.
                self.plan_phase = PlanPhase::Interview;
                let plan_path = self.resolve_plan_path();
                let _ = event_tx.send(AgentSessionEvent::PlanPhaseChanged {
                    phase: PlanPhase::Interview,
                });
                let _ = event_tx.send(AgentSessionEvent::PlanQuestionsReady {
                    questions,
                    plan_path,
                });
            }
            None => {
                // No JSON block — inject a one-shot correction prompt.
                let correction =
                    "Your response didn't include the required questions JSON block. \
                     Reply with ONLY the JSON block — for example:\n\
                     {\"questions\":[{\"q\":\"How should X work?\",\"options\":[{\"label\":\"A\",\"subtext\":\"Simpler but less flexible.\",\"recommended\":true},{\"label\":\"B\",\"subtext\":\"More powerful but adds complexity.\",\"recommended\":false}]}]}\n\
                     Each option must have label, subtext, and recommended fields. \
                     Use an empty array if you are satisfied: {\"questions\":[]}"
                        .to_string();
                crate::log::info("plan mode: missing questions block — injecting correction prompt");
                self.prompt(correction, event_tx);
            }
        }
    }

    fn prepare_system_prompt(&mut self) {
        // In talk mode: use a minimal conversational prompt with no tools,
        // no project context, and no memory.
        if self.talk_mode {
            self.agent.set_tools(Vec::new());
            self.agent.set_system_prompt(
                "You are a helpful assistant. Answer clearly and concisely.".to_string());
            return;
        }

        // Reload memory in case it was updated by a tool call
        self.resources.memory = std::fs::read_to_string(crate::nerv_dir().join("memory.md")).ok();

        let tools = self.tool_registry.active_tools();
        let tool_names: Vec<String> = tools.iter().map(|t| t.name().to_string()).collect();
        self.agent.set_tools(tools);
        let snippets = self.tool_registry.prompt_snippets();
        let guidelines = self.tool_registry.prompt_guidelines();
        let tool_name_refs: Vec<&str> = tool_names.iter().map(String::as_str).collect();
        let model_id = self.agent.model().map(|m| m.id.as_str());
        let mut prompt = build_system_prompt_for_model(
            &self.cwd,
            &self.resources,
            &tool_name_refs,
            &snippets,
            &guidelines,
            model_id,
        );

        if let Some(ref wt) = self.worktree {
            prompt.push_str(&format!(
                "\n\nYou are working in a git worktree at {}. \
                 All file paths and commands run from this directory, not the original repo. \
                 Do not cd to other directories.",
                wt.display()
            ));
        }

        if self.plan_mode {
            let plan_path_str = self.resolve_plan_path().display().to_string();
            prompt.push_str(&format!(
                "\n\n# Plan Mode\n\n\
                 You are in plan mode. Your job is to produce a thorough implementation plan.\n\n\
                 ## Workflow\n\n\
                 1. Research the codebase using read, grep, find, ls, symbols, and codemap.\n\
                 2. Write your initial plan to: `{plan_path}`\n\
                    Use the write tool for this — it is the only file you may write to.\n\
                 3. Identify 2–4 specific, targeted questions that would meaningfully improve \
                    the plan. For each question provide 2–4 concrete options. Each option must \
                    include a non-empty `subtext` (tradeoffs + reason why if recommended) and a `recommended` flag.\
                 4. **Your response MUST end with a raw JSON object on its own line, no fences:**\n\n\
                 {{\"questions\":[{{\"q\":\"<question>\",\"options\":[{{\"label\":\"<opt>\",\"subtext\":\"<tradeoff>\",\"recommended\":false}}, ...]}}]}}\n\n\
                 Rules:\n\
                 - Output the JSON as the **very last line** of your response — raw, no markdown fences, no trailing text.\n\
                 - Each option must have `label` (short choice text), `subtext` (one or two sentences explaining the tradeoffs — and if this option is recommended, explicitly why you recommend it), and `recommended` (true for exactly one option per question, false for the rest). Every option, including the recommended one, must have a non-empty `subtext`.\
                 - Provide 2–4 options per question. Do NOT include an 'other' option — that is \
                    added automatically.\n\
                 - When you have enough information to finalize the plan (after a round of \
                    answers), output {{\"questions\":[]}} as the last line to signal completion.\n\
                 - Only write to `{plan_path}`. Do not modify any other files.\n\
                 - Do not make any other file mutations.",
                plan_path = plan_path_str,
            ));
        }

        self.agent.set_system_prompt(prompt);
    }

    /// Run agent.prompt() with event forwarding and per-iteration persistence.
    /// Each message is written to SQLite as it's produced (WAL mode makes this
    /// cheap), so a mid-turn crash recovers everything up to the last completed
    /// tool call. Returns the new messages produced during this prompt.
    fn run_agent_prompt(
        &mut self,
        prompt_messages: Vec<AgentMessage>,
        event_tx: &Sender<AgentSessionEvent>,
    ) -> Vec<AgentMessage> {
        let tx = event_tx.clone();

        // Capture compaction state before the destructure so the on_event
        // closure can check the threshold on every UsageUpdate and cancel
        // the stream immediately when the context exceeds it.
        let auto_compact = self.compaction.auto_compact;
        let compaction_enabled = self.compaction.settings.enabled;
        let threshold_pct = self.compaction.settings.threshold_pct;
        let cancel_flag = self.agent.cancel.clone();
        let compaction_triggered = self.compaction.triggered.clone();
        let compact_threshold_pct = self.compaction.threshold_pct.clone();

        // Destructure self so the borrow checker sees agent, session_manager,
        // and session_cost as independent borrows (needed because the persist
        // closure captures session_manager/session_cost while agent.prompt()
        // borrows agent).
        let AgentSession {
            ref mut agent,
            ref mut session_manager,
            ref mut session_cost,
            ref mut last_input_tokens,
            ..
        } = *self;

        let context_window = agent.state.model.as_ref().map(|m| m.context_window).unwrap_or(0);
        let model_pricing = agent.state.model.as_ref().map(|m| m.pricing.clone());

        let mut last_input = 0u32;
        let mut persist = |msg: &AgentMessage| {
            let tokens = if let AgentMessage::Assistant(a) = msg {
                let input = a.usage.as_ref().map(|u| u.input).unwrap_or(0);
                let output = a.usage.as_ref().map(|u| u.output).unwrap_or(0);
                let cache_read = a.usage.as_ref().map(|u| u.cache_read).unwrap_or(0);
                let cache_write = a.usage.as_ref().map(|u| u.cache_write).unwrap_or(0);
                last_input = input;
                // Compute cost at record time so we can restore it on session load.
                let cost_usd = if let Some(ref pricing) = model_pricing {
                    let uncached = input.saturating_sub(cache_read + cache_write);
                    (pricing.input / 1_000_000.0) * uncached as f64
                        + (pricing.output / 1_000_000.0) * output as f64
                        + (pricing.cache_read / 1_000_000.0) * cache_read as f64
                        + (pricing.cache_write / 1_000_000.0) * cache_write as f64
                } else {
                    0.0
                };
                Some(crate::session::types::TokenInfo {
                    input,
                    output,
                    cache_read,
                    cache_write,
                    context_used: input + output,
                    context_window,
                    cost_usd,
                })
            } else {
                None
            };
            let _ = session_manager.append_message(msg, tokens);

            if let AgentMessage::Assistant(assistant) = msg
                && let Some(ref usage) = assistant.usage
                && let Some(ref pricing) = model_pricing
            {
                session_cost.add_usage(usage, pricing);
            }
        };

        let new_messages = agent.prompt(
            prompt_messages,
            &|event: AgentEvent| {
                // Check for auto-compaction on UsageUpdate (API-reported, at
                // response start) and ContextEstimate (heuristic, after tool
                // results). Both paths use the same threshold logic.
                if auto_compact && compaction_enabled && context_window > 0 {
                    let context_tokens = match &event {
                        AgentEvent::UsageUpdate { usage } => {
                            Some((usage.input + usage.output + usage.cache_read) as usize)
                        }
                        AgentEvent::ContextEstimate { estimated_tokens } => {
                            Some(*estimated_tokens)
                        }
                        _ => None,
                    };
                    if let Some(context_tokens) = context_tokens {
                        let live_pct = compact_threshold_pct.load(Ordering::Relaxed);
                        let pct = if live_pct > 0 { live_pct as f64 / 100.0 } else { threshold_pct };
                        let threshold = (context_window as f64 * pct) as usize;
                        if context_tokens > threshold {
                            crate::log::info(&format!(
                                "auto-compact triggered ({context_tokens} tokens > {threshold} threshold)"
                            ));
                            compaction_triggered.store(true, Ordering::Relaxed);
                            cancel_flag.store(true, Ordering::Relaxed);
                        }
                    }
                }
                let _ = tx.send(AgentSessionEvent::Agent(event));
            },
            Some(&mut persist),
        );

        *last_input_tokens = last_input;

        // Surface non-overflow errors
        if let Some(last) = last_assistant(&new_messages)
            && let StopReason::Error { ref message } = last.stop_reason
            && !last.stop_reason.is_context_overflow()
        {
            let _ = event_tx
                .send(AgentSessionEvent::Status { message: message.clone(), is_error: true });
        }

        // Fire onResponseComplete for successful, non-error turns.
        // Don't fire when compaction cancelled the stream — we're about to retry.
        if !compaction_triggered.load(Ordering::Relaxed)
            && let Some(last) = last_assistant(&new_messages)
            && !last.stop_reason.is_error()
            && !last.stop_reason.is_context_overflow()
        {
            super::notifications::fire(
                super::notifications::NotificationMatcher::OnResponseComplete,
                &self.config.notifications,
            );
        }

        new_messages
    }

    /// Rebuild agent message history from the current session entries.
    pub(crate) fn reload_agent_context(&mut self) {
        let entries = self.session_manager.entries();
        self.agent.set_messages(entries
            .iter()
            .filter_map(|e| {
                if let crate::session::types::SessionEntry::Message(me) = e {
                    Some(me.message.clone())
                } else {
                    None
                }
            })
            .collect());
    }

    /// Resolve the provider and model id to use for background utility tasks
    /// (compaction). Resolution order:
    ///   1. The `model_override` from config (fuzzy-matched via ModelRegistry).
    ///   2. DEFAULT_COMPACTION_MODEL on the anthropic provider (if registered).
    ///   3. The active session model as fallback.
    fn resolve_utility_provider(
        &self,
        model_override: Option<&str>,
    ) -> Option<(Arc<dyn Provider>, String)> {
        let registry = self.agent.provider_registry.read().ok()?;

        // 1. Config override
        if let Some(override_id) = model_override
            && let Some(model) = self.model_registry.find_model(override_id)
            && let Some(provider) = registry.get(&model.provider_name)
        {
            return Some((provider, model.id.clone()));
        }

        // 2. Default utility model (haiku) on anthropic
        if let Some(provider) = registry.get("anthropic") {
            return Some((provider, crate::core::model_registry::DEFAULT_COMPACTION_MODEL.to_string()));
        }

        // 3. Fall back to the current session model
        let model = self.agent.state.model.as_ref()?;
        let provider = registry.get(&model.provider_name)?;
        Some((provider, model.id.clone()))
    }

    pub fn run_compaction(
        &mut self,
        _custom_instructions: Option<String>,
    ) -> Result<compaction::CompactionOutcome, String> {
        // Lite-compact: cheaply zero stale bulk outputs before trying LLM
        // summarization. If this drops us below threshold, skip the LLM call.
        // Snapshot first so full compaction can use original content if needed.
        let age = self
            .config
            .lite_compact_age
            .unwrap_or(crate::agent::transform::LITE_COMPACT_AGE_THRESHOLD);
        let tokens_before_lite: usize = self
            .agent
            .state
            .messages
            .iter()
            .map(compaction::estimate_tokens)
            .sum();
        let compactable = self.tool_registry.lite_compactable_names();
        let snapshot = self.agent.state.messages.clone();
        let zeroed = crate::agent::transform::lite_compact(
            &mut self.agent.state.messages,
            age,
            &compactable,
        );
        if zeroed > 0 {
            crate::log::info(&format!("lite-compact: zeroed {zeroed} stale tool results"));
            let context_window = self
                .agent
                .state
                .model
                .as_ref()
                .map(|m| m.context_window)
                .unwrap_or(200_000);
            let estimated: usize = self
                .agent
                .state
                .messages
                .iter()
                .map(compaction::estimate_tokens)
                .sum();
            if !compaction::should_compact(estimated, context_window, &self.compaction.settings) {
                let _ = self.session_manager.append_lite_compaction(
                    zeroed as u32,
                    tokens_before_lite as u32,
                    estimated as u32,
                );
                return Ok(compaction::CompactionOutcome::LiteCompact { zeroed });
            }
            // Full compaction will follow — restore original messages so the
            // summarizer and archived transcript see unzeroed tool output.
            self.agent.state.messages = snapshot;
        }

        // Operate only on the current branch (root → leaf), not the whole tree.
        // Using entries() would compact entries from sibling branches too.
        let branch = self.session_manager.current_branch_entries();

        if branch.is_empty() {
            return Ok(compaction::CompactionOutcome::None);
        }

        // Summary-compact: if a prior non-lite compaction exists and fewer
        // than N user turns have elapsed, reuse its summary instead of
        // calling the LLM. This avoids paying for Haiku when the existing
        // summary is still fresh.
        let max_turns = self.compaction.settings.summary_compact_max_turns;
        if max_turns > 0 {
            let turns_since = compaction::count_user_turns_since_compaction(&branch);
            let prior = branch.iter().rev().find_map(|e| match e {
                crate::session::types::SessionEntry::Compaction(c)
                    if c.compaction_type != "lite" =>
                {
                    Some(c)
                }
                _ => None,
            });
            if let Some(prior) = prior {
                if turns_since < max_turns && !prior.summary.is_empty() {
                    crate::log::info(&format!(
                        "summary-compact: reusing prior summary ({turns_since} turns < {max_turns} max)"
                    ));
                    let cut = compaction::find_cut_point(
                        &branch,
                        0,
                        branch.len(),
                        self.compaction.settings.keep_recent_tokens,
                        self.compaction.settings.verbatim_window_tokens,
                    );
                    let first_kept_id =
                        branch[cut.first_kept_entry_index].id().to_string();
                    let tokens_before = compaction::tokens_before_compaction(&branch);
                    let to_summarize: Vec<AgentMessage> = branch
                        [..cut.verbatim_start_index]
                        .iter()
                        .filter_map(|e| {
                            if let crate::session::types::SessionEntry::Message(me) = e {
                                Some(me.message.clone())
                            } else {
                                None
                            }
                        })
                        .collect();
                    let archived_messages = to_summarize.clone();
                    let preserved_user_messages = compaction::extract_user_messages(
                        &to_summarize,
                        self.compaction.settings.preserved_user_tokens,
                    );
                    let summary = prior.summary.clone();
                    let tokens_after = compaction::tokens_after_compaction(
                        &summary,
                        &preserved_user_messages,
                        &branch[cut.verbatim_start_index..],
                    );
                    let _ = self.session_manager.append_compaction(
                        crate::session::types::CompactionRecord {
                            summary: summary.clone(),
                            first_kept_entry_id: first_kept_id.clone(),
                            tokens_before,
                            tokens_after,
                            model_id: String::new(),
                            cost_usd_before: self.session_cost.total,
                            archived_messages,
                            preserved_user_messages,
                            compaction_type: "summary".to_string(),
                        },
                    );
                    super::notifications::fire(
                        super::notifications::NotificationMatcher::OnCompactionDone,
                        &self.config.notifications,
                    );
                    return Ok(compaction::CompactionOutcome::Full(
                        compaction::CompactionResult {
                            summary,
                            structured: None,
                            first_kept_entry_id: first_kept_id,
                            tokens_before,
                            tokens_after,
                            model_id: String::new(),
                        },
                    ));
                }
            }
        }

        // Full compact: resolve the summariser provider and model.
        let (provider, model_id) =
            self.resolve_utility_provider(self.config.compaction_model.as_deref()).ok_or_else(|| {
                "No provider available for compaction. \
                     Set compaction_model in ~/.nerv/config.json or log in to Anthropic (/login)."
                    .to_string()
            })?;
        let summarizer_context_window: u32 = self
            .model_registry
            .find_model(&model_id)
            .map(|m| m.context_window)
            .unwrap_or(200_000);

        // The kept window is split into two parts for cache efficiency:
        //   [first_kept_entry_index .. verbatim_start_index)  → summarized by LLM
        //   [verbatim_start_index .. end)                      → kept verbatim in the
        // DB
        //
        // The verbatim window covers the newest turns of the pre-compaction context,
        // which were already cache-read (Rc) hits. Preserving them byte-for-byte means
        // they remain Rc on the very next API call — only the new summary is
        // cache-cold.
        let cut = compaction::find_cut_point(
            &branch,
            0,
            branch.len(),
            self.compaction.settings.keep_recent_tokens,
            self.compaction.settings.verbatim_window_tokens,
        );
        // first_kept_entry_id is the deletion boundary: the session DB removes
        // everything before this entry and inserts the compaction summary in
        // its place.
        let first_kept_id = branch[cut.first_kept_entry_index].id().to_string();

        // Summarize only the entries before verbatim_start_index. The verbatim window
        // beyond that point is left untouched in the DB and will appear in the next
        // API call byte-for-byte, recovering its Rc status immediately.
        let to_summarize: Vec<AgentMessage> = branch[..cut.verbatim_start_index]
            .iter()
            .filter_map(|e| {
                if let crate::session::types::SessionEntry::Message(me) = e {
                    Some(me.message.clone())
                } else {
                    None
                }
            })
            .collect();

        if to_summarize.is_empty() {
            return Ok(compaction::CompactionOutcome::None);
        }

        // tokens_before: the actual context size at the moment compaction fires.
        // Delegates to the helper which walks the branch in reverse for
        // the most recent API-reported context_used, falling back to
        // estimate_tokens sum when no usage data exists yet.
        let tokens_before: u32 = compaction::tokens_before_compaction(&branch);

        // Archive only the entries being deleted (before the verbatim window).
        // The verbatim window entries stay as live DB rows, so including them
        // here would produce duplicates in exports.
        let archived_messages: Vec<AgentMessage> = branch[..cut.verbatim_start_index]
            .iter()
            .filter_map(|e| {
                if let crate::session::types::SessionEntry::Message(me) = e {
                    Some(me.message.clone())
                } else {
                    None
                }
            })
            .collect();

        // Extract verbatim user messages from the summarized region to preserve
        // exact details (file paths, edge cases, preferences) alongside the summary.
        let preserved_user_messages = compaction::extract_user_messages(
            &to_summarize,
            self.compaction.settings.preserved_user_tokens,
        );

        match generate_summary(&to_summarize, provider, &model_id, summarizer_context_window) {
            Ok(generated) => {
                let summary = generated.to_markdown();
                let structured = generated.structured().cloned();
                let tokens_after = compaction::tokens_after_compaction(
                    &summary,
                    &preserved_user_messages,
                    &branch[cut.verbatim_start_index..],
                );
                let _ = self.session_manager.append_compaction(
                    crate::session::types::CompactionRecord {
                        summary: summary.clone(),
                        first_kept_entry_id: first_kept_id.clone(),
                        tokens_before,
                        tokens_after,
                        model_id: model_id.clone(),
                        cost_usd_before: self.session_cost.total,
                        archived_messages,
                        preserved_user_messages,
                        compaction_type: "full".to_string(),
                    },
                );
                // Fire onCompactionDone hooks (fire-and-forget).
                super::notifications::fire(
                    super::notifications::NotificationMatcher::OnCompactionDone,
                    &self.config.notifications,
                );
                Ok(compaction::CompactionOutcome::Full(CompactionResult {
                    summary,
                    structured,
                    first_kept_entry_id: first_kept_id,
                    tokens_before,
                    tokens_after,
                    model_id,
                }))
            }
            Err(e) => {
                let msg = format!("Compaction failed: {e}");
                crate::log::error(&msg);
                Err(msg)
            }
        }
    }

    pub fn set_model(
        &mut self,
        provider: &str,
        model_id: &str,
        event_tx: &Sender<AgentSessionEvent>,
    ) {
        if let Some(model) = self.model_registry.get_model(provider, model_id) {
            self.agent.set_model(Some(model.clone()));
            let _ = self.session_manager.append_model_change(provider, model_id);
            let _ = event_tx.send(AgentSessionEvent::ModelChanged { model: model.clone() });
            // Persist as session-level override
            self.session_manager.update_session_config(|cfg| {
                cfg.default_model = Some(model_id.to_string());
            });
        }
    }

    pub fn set_thinking_level(
        &mut self,
        level: ThinkingLevel,
        event_tx: &Sender<AgentSessionEvent>,
    ) {
        self.agent.set_thinking_level(level);
        let _ = self.session_manager.append_thinking_level_change(level);
        let _ = event_tx.send(AgentSessionEvent::ThinkingLevelChanged { level });
        // Persist as session-level override
        self.session_manager.update_session_config(|cfg| {
            cfg.default_thinking = Some(level == ThinkingLevel::On);
        });
    }

    pub fn abort(&self) {
        self.agent.abort();
    }

    pub fn load_session(&mut self, session_id: &str, event_tx: &Sender<AgentSessionEvent>) {
        match self.session_manager.load_session(session_id) {
            Ok(ctx) => {
                // Extract fields we need for deferred use before partial moves of ctx.
                let full_history = ctx.full_history;
                let cost_usd = ctx.cost_usd;
                let total_input = ctx.total_input;
                let total_output = ctx.total_output;
                let api_calls = ctx.api_calls;
                let input_history = ctx.input_history;

                self.agent.set_messages(ctx.messages);

                // Restore accumulated session cost from persisted per-call cost_usd values.
                self.session_cost = Cost::default();
                self.session_cost.total = cost_usd;

                // Restore thinking level
                self.agent.set_thinking_level(ctx.thinking_level);
                let _ = event_tx
                    .send(AgentSessionEvent::ThinkingLevelChanged { level: ctx.thinking_level });

                // Restore model — try model_registry first, fall back to custom provider config
                if let Some((provider, model_id)) = ctx.model {
                    if self.model_registry.get_model(&provider, &model_id).is_some() {
                        self.set_model(&provider, &model_id, event_tx);
                    } else {
                        // Model not in registry — check if it's a custom provider we can
                        // re-register
                        if let Some(pcfg) =
                            self.config.custom_providers.iter().find(|p| p.name == provider)
                        {
                            let p = std::sync::Arc::new(crate::agent::OpenAICompatProvider::new(
                                pcfg.name.clone(),
                                pcfg.base_url.clone(),
                                pcfg.api_key.clone(),
                            ));
                            self.agent.provider_registry.write().unwrap().register(&provider, p);
                            // Create the model directly
                            let model = Model {
                                id: model_id.clone(),
                                name: model_id.clone(),
                                provider_name: provider.clone(),
                                context_window: 128_000,
                                max_output_tokens: 32_000,
                                reasoning: false,
                                supports_adaptive_thinking: false,
                                supports_xhigh: false,
                                pricing: ModelPricing {
                                    input: 0.0,
                                    output: 0.0,
                                    cache_read: 0.0,
                                    cache_write: 0.0,
                                },
                            };
                            self.agent.set_model(Some(model.clone()));
                            let _ = event_tx.send(AgentSessionEvent::ModelChanged { model });
                        } else {
                            let _ = event_tx.send(AgentSessionEvent::Status {
                                message: format!("Model {}/{} not available", provider, model_id),
                                is_error: true,
                            });
                        }
                    }
                }

                // Restore worktree cwd if session was tied to one
                if let Some(wt_path) = self.session_manager.session_worktree() {
                    if !wt_path.exists() {
                        let _ = event_tx.send(AgentSessionEvent::Status {
                            message: format!(
                                "Session worktree no longer exists: {}",
                                wt_path.display()
                            ),
                            is_error: true,
                        });
                        return;
                    }
                    self.set_worktree(wt_path.clone());
                    let _ = event_tx.send(AgentSessionEvent::WorktreeCreated { path: wt_path });
                }

                let _ = event_tx.send(AgentSessionEvent::SessionStarted {
                    id: self.session_manager.session_id().to_string(),
                    name: self.session_manager.name().map(|s| s.to_string()),
                });
                let _ = event_tx.send(AgentSessionEvent::SessionLoaded {
                    messages: full_history,
                    cost_usd,
                    total_input,
                    total_output,
                    api_calls,
                    input_history,
                });
                if let Some(pct) = self.apply_saved_compact_threshold() {
                    let _ = event_tx.send(AgentSessionEvent::CompactThresholdChanged { pct });
                }

                // Restore per-session config overrides
                let scfg = ctx.session_config;
                if let Some(effort) = scfg.default_effort_level {
                    self.agent.set_effort_level(Some(effort));
                    let _ = event_tx
                        .send(AgentSessionEvent::EffortLevelChanged { level: Some(effort) });
                }
                if let Some(enabled) = scfg.auto_compact {
                    self.compaction.auto_compact = enabled;
                }
                // Don't re-name sessions that were already named (or have a preview we could
                // use). We consider any loaded session as already handled.
                self.session_named = true;
            }
            Err(e) => {
                crate::log::error(&format!("failed to load session: {}", e));
                let _ = event_tx.send(AgentSessionEvent::Status {
                    message: format!("Failed to load session: {}", e),
                    is_error: true,
                });
            }
        }
    }

    /// Apply a saved compact threshold from the session DB (if any) to
    /// compaction_settings. Returns the loaded percentage (0–100) if one
    /// was saved, so the caller can notify the UI.
    fn apply_saved_compact_threshold(&mut self) -> Option<u8> {
        let pct = self.session_manager.get_compact_threshold()?;
        self.compaction.settings.threshold_pct = pct.clamp(0.01, 1.0);
        self.compaction.threshold_pct.store((pct * 100.0) as u32, Ordering::Relaxed);
        Some((pct * 100.0).round() as u8)
    }

    /// Check if a tool call with given arguments has been previously accepted
    /// in this session. Args should be serialized to JSON for consistent
    /// hashing.
    pub fn is_permission_cached(&self, tool: &str, args_json: &str) -> bool {
        self.permission_cache.lock().unwrap().contains(&permission_key(tool, args_json))
    }

    /// Record a permission accept in the session. Writes to DB and updates
    /// in-memory cache.
    pub fn accept_permission(&mut self, tool: &str, args_json: &str) {
        self.permission_cache.lock().unwrap().insert(permission_key(tool, args_json));

        // Write to session database
        use crate::session::types::{PermissionAcceptEntry, SessionEntry, gen_entry_id, now_iso};
        let entry = PermissionAcceptEntry {
            id: gen_entry_id(),
            parent_id: self.session_manager.leaf_id().map(|s| s.to_string()),
            timestamp: now_iso(),
            tool: tool.to_string(),
            args: args_json.to_string(),
        };
        let _ = self.session_manager.append_entry(SessionEntry::PermissionAccept(entry));
    }

    /// Load permission accepts from session history into the cache.
    /// Called after session is loaded to populate the cache with all previously
    /// accepted permissions.
    pub fn load_permission_cache(&mut self) {
        use crate::session::types::SessionEntry;
        let entries = self.session_manager.current_branch_entries();
        let mut cache = self.permission_cache.lock().unwrap();
        for entry in entries {
            if let SessionEntry::PermissionAccept(pe) = entry {
                cache.insert(permission_key(&pe.tool, &pe.args));
            }
        }
    }

    /// Disable automatic session naming (used in tests to prevent mock provider
    /// consumption).
    pub fn disable_session_naming(&mut self) {
        self.session_named = true;
    }
}

fn permission_key(tool: &str, args_json: &str) -> String {
    format!("{}:{}", tool, args_json)
}

pub(crate) fn last_assistant(messages: &[AgentMessage]) -> Option<&AssistantMessage> {
    messages.iter().rev().find_map(AgentMessage::as_assistant)
}
