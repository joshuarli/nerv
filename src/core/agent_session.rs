use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel::Sender;

use super::model_registry::ModelRegistry;
use super::resource_loader::LoadedResources;
use super::system_prompt::build_system_prompt_for_model;
use super::tool_registry::ToolRegistry;
use crate::agent::agent::Agent;
use crate::agent::provider::Provider;
use crate::agent::types::*;
use crate::compaction::summarize::{generate_summary, generate_session_name};
use crate::compaction::{self, CompactionResult, CompactionSettings};
use crate::core::config::NervConfig;
use crate::session::SessionManager;

#[derive(Debug, Clone, Copy)]
pub enum CompactionReason {
    Overflow,
    Threshold,
    Manual,
}

#[derive(Debug, Clone)]
pub enum AgentSessionEvent {
    Agent(AgentEvent),
    AutoCompactionStart {
        reason: CompactionReason,
    },
    AutoCompactionEnd {
        summary: Option<String>,
        will_retry: bool,
        /// Post-compaction messages (for UI rebuild). Empty when compaction failed/skipped.
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
    /// Permission request — agent blocks until response is sent back.
    PlanModeChanged {
        enabled: bool,
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
    /// Context gate — agent blocks until user confirms or denies the large request.
    ContextGateRequest {
        estimated_tokens: usize,
        prev_tokens: usize,
        context_window: u32,
        response_tx: crossbeam_channel::Sender<bool>,
    },
}

pub enum SessionCommand {
    Prompt { text: String },
    Abort,
    NewSession,
    LoadSession { id: String },
    SetModel { provider: String, model_id: String },
    SetThinkingLevel { level: ThinkingLevel },
    SetEffortLevel { level: Option<EffortLevel> },
    Compact { custom_instructions: Option<String> },
    SetCompactThreshold { pct: u8 },
    ExportJsonl,
    ExportHtml,
    Login { provider: String },
    ListSessions { repo_root: Option<String> },
    GetTree,
    SwitchBranch {
        entry_id: String,
        /// If true, set leaf to the *parent* of entry_id instead (user message re-submission).
        use_parent: bool,
        /// If true, reset leaf to None (root user message selected).
        reset_leaf: bool,
    },
    CreateWorktree { branch_name: String, nerv_dir: PathBuf },
    MergeWorktree,
    SetPlanMode { enabled: bool },
}

pub struct AgentSession {
    pub agent: Agent,
    pub session_manager: SessionManager,
    pub tool_registry: ToolRegistry,
    compaction_settings: CompactionSettings,
    model_registry: Arc<ModelRegistry>,
    resources: LoadedResources,
    cwd: PathBuf,
    session_cost: Cost,
    last_input_tokens: u32,
    pub permissions_enabled: bool,
    /// Cache of accepted permissions: (tool, args_json) keyed by args hash
    /// Shared arc for use in permission_fn closure
    permission_cache: Arc<std::sync::Mutex<HashSet<String>>>,
    /// Worktree path tied to this session (set via --wt or /wt).
    worktree: Option<PathBuf>,
    /// Plan mode: restrict tools to read-only, steer model toward planning.
    plan_mode: bool,
    /// True once the session has been given an auto-generated name, to avoid re-naming.
    session_named: bool,
}

impl AgentSession {
    pub fn new(
        agent: Agent,
        session_manager: SessionManager,
        tool_registry: ToolRegistry,
        model_registry: Arc<ModelRegistry>,
        resources: LoadedResources,
        cwd: PathBuf,
    ) -> Self {
        Self {
            agent,
            session_manager,
            tool_registry,
            compaction_settings: CompactionSettings::default(),
            model_registry,
            resources,
            cwd,
            session_cost: Cost::default(),
            last_input_tokens: 0,
            permissions_enabled: false,
            permission_cache: Arc::new(std::sync::Mutex::new(HashSet::new())),
            worktree: None,
            plan_mode: false,
            session_named: false,
        }
    }

    pub fn set_plan_mode(&mut self, enabled: bool, event_tx: &Sender<AgentSessionEvent>) {
        self.plan_mode = enabled;
        if enabled {
            self.tool_registry
                .set_active(&["read", "bash", "grep", "find", "ls", "memory"]);
        } else {
            self.tool_registry.set_active(&[]);
        }
        let _ = event_tx.send(AgentSessionEvent::PlanModeChanged { enabled });
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
            let _ = self
                .session_manager
                .new_session(&self.cwd, self.worktree.as_deref());
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
            self.session_manager
                .append_entry(crate::session::types::SessionEntry::SystemPrompt(
                    crate::session::types::SystemPromptEntry {
                        id: crate::session::types::gen_entry_id(),
                        parent_id: self.session_manager.leaf_id().map(|s| s.to_string()),
                        timestamp: crate::session::types::now_iso(),
                        prompt: self.agent.state.system_prompt.clone(),
                        token_count: prompt_tokens,
                    },
                ));

        // Set up permission checking for tool calls
        if self.permissions_enabled {
            let repo_root = crate::find_repo_root(&self.cwd);
            let perm_tx = event_tx.clone();
            let cache = self.permission_cache.clone();
            let (perm_accept_tx, perm_accept_rx) = crossbeam_channel::unbounded();

            self.agent.state.permission_fn = Some(std::sync::Arc::new(
                move |tool: &str, args: &serde_json::Value| {
                    // Check exact-match cache first
                    let args_json = serde_json::to_string(args).unwrap_or_default();
                    let key = format!("{}:{}", tool, args_json);
                    if cache.lock().unwrap().contains(&key) {
                        return true;
                    }

                    let perm = super::permissions::check(tool, args, repo_root.as_deref());
                    match perm {
                        super::permissions::Permission::Allow => true,
                        super::permissions::Permission::Ask(reason) => {
                            // Check whether this reason was already approved (e.g. same
                            // outside-repo path referenced in a different command). This
                            // lets the user approve once and have subsequent calls that
                            // trigger the same reason auto-approved for the rest of the
                            // session.
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
                            // Block until user responds
                            let approved = resp_rx.recv().unwrap_or(false);
                            if approved {
                                // Cache both the exact call and the reason so future
                                // calls that trigger the same reason are auto-approved.
                                let mut c = cache.lock().unwrap();
                                c.insert(key.clone());
                                c.insert(reason_key);
                                // Queue for DB recording
                                let _ = perm_accept_tx.send((tool.to_string(), args_json.clone()));
                            }
                            approved
                        }
                    }
                },
            ));
            
            // Process queued permission accepts after the prompt
            while let Ok((tool, args)) = perm_accept_rx.try_recv() {
                use crate::session::types::{gen_entry_id, now_iso, PermissionAcceptEntry, SessionEntry};
                let entry = PermissionAcceptEntry {
                    id: gen_entry_id(),
                    parent_id: self.session_manager.leaf_id().map(|s| s.to_string()),
                    timestamp: now_iso(),
                    tool,
                    args,
                };
                let _ = self.session_manager.append_entry(SessionEntry::PermissionAccept(entry));
            }

        }

        // Set up context gate (circuit breaker for context growth)
        {
            let gate_tx = event_tx.clone();
            self.agent.state.context_gate_fn = Some(std::sync::Arc::new(
                move |info: crate::agent::agent::ContextGateInfo| {
                    // Need at least a few rounds to establish a baseline — early calls
                    // always show huge growth from initial file reads.
                    if info.tool_rounds < 4 || info.prev_tokens == 0 {
                        return true;
                    }
                    let delta = info.estimated_tokens.saturating_sub(info.prev_tokens);
                    // Only trigger when the absolute growth is significant (>20k tokens,
                    // roughly a 500-line file) AND represents >30% growth. Small deltas
                    // from normal tool use (reads, edits) should never prompt.
                    if delta < 20_000 {
                        return true;
                    }
                    let pct = (delta as f64 / info.prev_tokens as f64) * 100.0;
                    if pct <= 30.0 {
                        return true;
                    }
                    // Ask user
                    let (resp_tx, resp_rx) = crossbeam_channel::bounded(1);
                    let _ = gate_tx.send(AgentSessionEvent::ContextGateRequest {
                        estimated_tokens: info.estimated_tokens,
                        prev_tokens: info.prev_tokens,
                        context_window: info.context_window,
                        response_tx: resp_tx,
                    });
                    resp_rx.recv().unwrap_or(false)
                },
            ));
        }

        let new_messages = self.run_agent_prompt(vec![user_msg], event_tx);

        // Check for context overflow → auto-compact → retry
        if let Some(last) = last_assistant(&new_messages)
            && last.stop_reason.is_context_overflow()
        {
            crate::log::info("context overflow detected, attempting auto-compact + retry");
            let _ = event_tx.send(AgentSessionEvent::AutoCompactionStart {
                reason: CompactionReason::Overflow,
            });

            match self.run_compaction(None) {
                Ok(Some(result)) => {
                    // Reload agent context from compacted session before notifying UI
                    self.reload_agent_context();
                    let _ = event_tx.send(AgentSessionEvent::AutoCompactionEnd {
                        summary: Some(result.summary),
                        will_retry: true,
                        messages: self.agent.state.messages.clone(),
                    });

                    // Retry the original prompt
                    let retry_msg = AgentMessage::User {
                        content: vec![ContentItem::Text { text }],
                        timestamp: now_millis(),
                    };
                    self.prepare_system_prompt();
                    let _retry_messages = self.run_agent_prompt(vec![retry_msg], event_tx);
                }
                Ok(None) => {
                    let _ = event_tx.send(AgentSessionEvent::AutoCompactionEnd {
                        summary: None,
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

        // Check threshold-based auto-compaction (proactive, before we hit the wall)
        if let Some(last) = last_assistant(&new_messages)
            && !last.stop_reason.is_error()
            && let Some(ref usage) = last.usage
            && let Some(ref model) = self.agent.state.model
        {
            let context_tokens = (usage.input + usage.output + usage.cache_read) as usize;
            if compaction::should_compact(
                context_tokens,
                model.context_window,
                &self.compaction_settings,
            ) {
                crate::log::info("threshold auto-compact triggered");
                let _ = event_tx.send(AgentSessionEvent::AutoCompactionStart {
                    reason: CompactionReason::Threshold,
                });
                match self.run_compaction(None) {
                    Ok(Some(result)) => {
                        self.reload_agent_context();
                        let _ = event_tx.send(AgentSessionEvent::AutoCompactionEnd {
                            summary: Some(result.summary),
                            will_retry: false,
                            messages: self.agent.state.messages.clone(),
                        });
                    }
                    Ok(None) | Err(_) => {
                        // Threshold auto-compact: silently skip on failure (will retry next turn)
                        let _ = event_tx.send(AgentSessionEvent::AutoCompactionEnd {
                            summary: None,
                            will_retry: false,
                            messages: vec![],
                        });
                    }
                }
            }
        }

        // Generate a session title after the first completed turn.
        // session_naming_model: None = unset (use default), Some(None) = disabled, Some(Some(s)) = use s.
        if !self.session_named && self.session_manager.name().is_none() {
            let config = NervConfig::load(crate::nerv_dir());
            let naming_model_override: Option<Option<&str>> = config
                .session_naming_model
                .as_ref()
                .map(|inner| inner.as_deref());
            // Some(None) means the user explicitly set null → skip naming entirely.
            let should_name = !matches!(naming_model_override, Some(None));
            if should_name {
                let model_hint = naming_model_override.flatten();
                if let Some((provider, model_id)) =
                    self.resolve_utility_provider(model_hint)
                {
                    // `text` is the original user message string (still in scope).
                    if !text.is_empty() {
                        match generate_session_name(&text, provider, &model_id) {
                            Ok(name) => {
                                self.session_manager.set_name(&name);
                                self.session_named = true;
                                let _ = event_tx.send(AgentSessionEvent::SessionNamed {
                                    name,
                                });
                            }
                            Err(e) => {
                                crate::log::info(&format!("session naming failed: {e}"));
                            }
                        }
                    }
                }
            }
        }
    }

    fn prepare_system_prompt(&mut self) {
        // Reload memory in case it was updated by a tool call
        self.resources.memory =
            std::fs::read_to_string(crate::nerv_dir().join("memory.md")).ok();

        self.agent.state.tools = self.tool_registry.active_tools();
        let tool_names: Vec<&str> = self.agent.state.tools.iter().map(|t| t.name()).collect();
        let snippets = self.tool_registry.prompt_snippets();
        let guidelines = self.tool_registry.prompt_guidelines();
        let model_id = self.agent.state.model.as_ref().map(|m| m.id.as_str());
        self.agent.state.system_prompt = build_system_prompt_for_model(
            &self.cwd,
            &self.resources,
            &tool_names,
            &snippets,
            &guidelines,
            model_id,
        );

        if let Some(ref wt) = self.worktree {
            self.agent.state.system_prompt.push_str(&format!(
                "\n\nYou are working in a git worktree at {}. \
                 All file paths and commands run from this directory, not the original repo. \
                 Do not cd to other directories.",
                wt.display()
            ));
        }

        if self.plan_mode {
            self.agent.state.system_prompt.push_str(
                "\n\n# Plan Mode\n\n\
                 You are in plan mode. Research the codebase and outline an implementation plan. \
                 Do not modify any files — the edit and write tools are unavailable. \
                 Focus on: identifying relevant files, understanding existing patterns, \
                 and producing a clear step-by-step plan.",
            );
        }
    }

    /// Run agent.prompt() with event forwarding and persistence. Returns new messages.
    fn run_agent_prompt(
        &mut self,
        prompt_messages: Vec<AgentMessage>,
        event_tx: &Sender<AgentSessionEvent>,
    ) -> Vec<AgentMessage> {
        let tx = event_tx.clone();

        let new_messages = self.agent.prompt(prompt_messages, &|event: AgentEvent| {
            let _ = tx.send(AgentSessionEvent::Agent(event));
        });

        let context_window = self
            .agent
            .state
            .model
            .as_ref()
            .map(|m| m.context_window)
            .unwrap_or(0);

        // Persist new messages to SQLite with token metadata.
        // Each AssistantMessage carries its own Usage from its API call,
        // so we use per-message input tokens (not a single value for the whole turn).
        // context_used = input + output for that call; input already includes all prior
        // conversation history, so no running accumulation needed.
        let mut last_input: u32 = 0;
        for msg in &new_messages {
            let tokens = if let AgentMessage::Assistant(a) = msg {
                let input = a.usage.as_ref().map(|u| u.input).unwrap_or(0);
                let output = a.usage.as_ref().map(|u| u.output).unwrap_or(0);
                let cache_read = a.usage.as_ref().map(|u| u.cache_read).unwrap_or(0);
                let cache_write = a.usage.as_ref().map(|u| u.cache_write).unwrap_or(0);
                last_input = input;
                Some(crate::session::types::TokenInfo {
                    input,
                    output,
                    cache_read,
                    cache_write,
                    context_used: input + output,
                    context_window,
                })
            } else {
                None
            };
            let _ = self.session_manager.append_message(msg, tokens);
        }
        self.last_input_tokens = last_input;

        // Update cost
        for msg in &new_messages {
            if let AgentMessage::Assistant(assistant) = msg
                && let Some(ref usage) = assistant.usage
                && let Some(model) = &self.agent.state.model
            {
                self.session_cost.add_usage(usage, &model.pricing);
            }
        }

        // Surface non-overflow errors
        if let Some(last) = last_assistant(&new_messages)
            && let StopReason::Error { ref message } = last.stop_reason
            && !last.stop_reason.is_context_overflow()
        {
            let _ = event_tx.send(AgentSessionEvent::Status {
                message: message.clone(),
                is_error: true,
            });
        }

        new_messages
    }

    /// Rebuild agent message history from the current session entries.
    fn reload_agent_context(&mut self) {
        let entries = self.session_manager.entries();
        self.agent.state.messages = entries
            .iter()
            .filter_map(|e| {
                if let crate::session::types::SessionEntry::Message(me) = e {
                    Some(me.message.clone())
                } else {
                    None
                }
            })
            .collect();
    }

    /// Resolve the provider and model id to use for background utility tasks
    /// (compaction, session naming). Resolution order:
    ///   1. The `model_override` from config (fuzzy-matched via ModelRegistry).
    ///   2. claude-haiku-4-5 on the anthropic provider (if registered).
    ///   3. The active session model as fallback.
    fn resolve_utility_provider(
        &self,
        model_override: Option<&str>,
    ) -> Option<(Arc<dyn Provider>, String)> {
        let registry = self.agent.provider_registry.read().ok()?;

        // 1. Config override
        if let Some(override_id) = model_override {
            if let Some(model) = self.model_registry.find_model(override_id) {
                if let Some(provider) = registry.get(&model.provider_name) {
                    return Some((provider, model.id.clone()));
                }
            }
        }

        // 2. Default utility model (haiku) on anthropic
        if let Some(provider) = registry.get(crate::compaction::summarize::DEFAULT_UTILITY_PROVIDER) {
            return Some((
                provider,
                crate::compaction::summarize::DEFAULT_UTILITY_MODEL.to_string(),
            ));
        }

        // 3. Fall back to the current session model
        let model = self.agent.state.model.as_ref()?;
        let provider = registry.get(&model.provider_name)?;
        Some((provider, model.id.clone()))
    }

    /// Run compaction. Returns `Ok(Some(result))` on success, `Ok(None)` when there is
    /// nothing to compact (context too small / no messages before the cut point), and
    /// `Err(msg)` when compaction cannot proceed (no suitable provider, summarization
    /// API call failed, etc.).
    pub fn run_compaction(
        &mut self,
        _custom_instructions: Option<String>,
    ) -> Result<Option<CompactionResult>, String> {
        let config = NervConfig::load(crate::nerv_dir());
        let (provider, model_id) =
            self.resolve_utility_provider(config.compaction_model.as_deref())
                .ok_or_else(|| {
                    "No provider available for compaction. \
                     Set compaction_model in ~/.nerv/config.json or log in to Anthropic (/login)."
                        .to_string()
                })?;

        // Operate only on the current branch (root → leaf), not the whole tree.
        // Using entries() would compact entries from sibling branches too.
        let branch = self.session_manager.current_branch_entries();
        if branch.is_empty() {
            return Ok(None);
        }

        // Find cut point using token-budget-aware algorithm
        let cut = compaction::find_cut_point(
            &branch,
            0,
            branch.len(),
            self.compaction_settings.keep_recent_tokens,
        );
        let first_kept_id = branch[cut.first_kept_entry_index].id().to_string();

        // Collect messages before the cut point for summarization
        let to_summarize: Vec<AgentMessage> = branch[..cut.first_kept_entry_index]
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
            return Ok(None);
        }

        let tokens_before = to_summarize
            .iter()
            .map(compaction::estimate_tokens)
            .sum::<usize>() as u32;

        match generate_summary(&to_summarize, None, provider, &model_id) {
            Ok(summary) => {
                let _ = self.session_manager.append_compaction(
                    summary.clone(),
                    first_kept_id.clone(),
                    tokens_before,
                );
                Ok(Some(CompactionResult {
                    summary,
                    first_kept_entry_id: first_kept_id,
                    tokens_before,
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
            self.agent.state.model = Some(model.clone());
            let _ = self.session_manager.append_model_change(provider, model_id);
            let _ = event_tx.send(AgentSessionEvent::ModelChanged {
                model: model.clone(),
            });
            // Persist as default for next startup
            let nerv_dir = crate::nerv_dir();
            let mut cfg = super::config::NervConfig::load(nerv_dir);
            cfg.default_model = Some(model_id.to_string());
            let _ = cfg.save(nerv_dir);
        }
    }

    pub fn set_thinking_level(
        &mut self,
        level: ThinkingLevel,
        event_tx: &Sender<AgentSessionEvent>,
    ) {
        self.agent.state.thinking_level = level;
        let _ = self.session_manager.append_thinking_level_change(level);
        let _ = event_tx.send(AgentSessionEvent::ThinkingLevelChanged { level });
    }

    pub fn abort(&self) {
        self.agent.abort();
    }

    pub fn load_session(&mut self, session_id: &str, event_tx: &Sender<AgentSessionEvent>) {
        match self.session_manager.load_session(session_id) {
            Ok(ctx) => {
                self.agent.state.messages = ctx.messages;

                // Restore thinking level
                self.agent.state.thinking_level = ctx.thinking_level;
                let _ = event_tx.send(AgentSessionEvent::ThinkingLevelChanged {
                    level: ctx.thinking_level,
                });

                // Restore model — try model_registry first, fall back to custom provider config
                if let Some((provider, model_id)) = ctx.model {
                    if self
                        .model_registry
                        .get_model(&provider, &model_id)
                        .is_some()
                    {
                        self.set_model(&provider, &model_id, event_tx);
                    } else {
                        // Model not in registry — check if it's a custom provider we can re-register
                        let config = crate::core::config::NervConfig::load(crate::nerv_dir());
                        if let Some(pcfg) =
                            config.custom_providers.iter().find(|p| p.name == provider)
                        {
                            let p = std::sync::Arc::new(crate::agent::OpenAICompatProvider::new(
                                pcfg.name.clone(),
                                pcfg.base_url.clone(),
                                pcfg.api_key.clone(),
                            ));
                            self.agent
                                .provider_registry
                                .write()
                                .unwrap()
                                .register(&provider, p);
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
                            self.agent.state.model = Some(model.clone());
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
                    messages: self.agent.state.messages.clone(),
                });
                self.load_permission_cache();
                if let Some(pct) = self.apply_saved_compact_threshold() {
                    let _ = event_tx.send(AgentSessionEvent::CompactThresholdChanged { pct });
                }
                // Don't re-name sessions that were already named (or have a preview we could use).
                // We consider any loaded session as already handled.
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

    /// Apply a saved compact threshold from the session DB (if any) to compaction_settings.
    /// Returns the loaded percentage (0–100) if one was saved, so the caller can notify the UI.
    fn apply_saved_compact_threshold(&mut self) -> Option<u8> {
        let pct = self.session_manager.get_compact_threshold()?;
        self.compaction_settings.threshold_pct = pct.clamp(0.01, 1.0);
        Some((pct * 100.0).round() as u8)
    }

    /// Check if a tool call with given arguments has been previously accepted in this session.
    /// Args should be serialized to JSON for consistent hashing.
    pub fn is_permission_cached(&self, tool: &str, args_json: &str) -> bool {
        let key = format!("{}:{}", tool, args_json);
        self.permission_cache.lock().unwrap().contains(&key)
    }

    /// Record a permission accept in the session. Writes to DB and updates in-memory cache.
    pub fn accept_permission(&mut self, tool: &str, args_json: &str) {
        let key = format!("{}:{}", tool, args_json);
        self.permission_cache.lock().unwrap().insert(key);

        // Write to session database
        use crate::session::types::{gen_entry_id, now_iso, PermissionAcceptEntry, SessionEntry};
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
    /// Called after session is loaded to populate the cache with all previously accepted permissions.
    pub fn load_permission_cache(&mut self) {
        use crate::session::types::SessionEntry;
        let entries = self.session_manager.current_branch_entries();
        let mut cache = self.permission_cache.lock().unwrap();
        for entry in entries {
            if let SessionEntry::PermissionAccept(pe) = entry {
                let key = format!("{}:{}", pe.tool, pe.args);
                cache.insert(key);
            }
        }
    }
}

fn handle_login(provider: &str, session: &mut AgentSession, event_tx: &Sender<AgentSessionEvent>) {
    match provider {
        "anthropic" => {
            let tx = event_tx.clone();
            let result = super::auth::login_anthropic(
                &|url| {
                    let _ = tx.send(AgentSessionEvent::Status {
                        message: format!(
                            "Opening browser for Anthropic login...\n\nIf the browser doesn't open, visit:\n{}",
                            url
                        ),
                        is_error: false,
                    });
                    // Try to open browser
                    let _ = std::process::Command::new("open").arg(url).spawn();
                },
                &|msg| {
                    let _ = tx.send(AgentSessionEvent::Status {
                        message: msg.to_string(),
                        is_error: false,
                    });
                },
            );

            match result {
                Ok(creds) => {
                    let nerv_dir = crate::nerv_dir();
                    let mut auth = super::auth::AuthStorage::load(nerv_dir);
                    let api_key = creds.access.clone();
                    auth.set("anthropic", super::auth::Credential::OAuth(creds));

                    // Register the provider (OAuth uses Bearer auth)
                    let nerv_config = super::config::NervConfig::load(nerv_dir);
                    let extra_headers: Vec<(String, String)> = nerv_config
                        .headers
                        .get("anthropic")
                        .map(|h| h.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                        .unwrap_or_default();
                    let provider = std::sync::Arc::new(
                        crate::agent::AnthropicProvider::new_oauth(api_key)
                            .with_headers(extra_headers),
                    );
                    session
                        .agent
                        .provider_registry
                        .write()
                        .unwrap()
                        .register("anthropic", provider);

                    // Set default model — haiku is always available with OAuth
                    if session.agent.state.model.is_none() {
                        let model = crate::agent::types::Model {
                            id: "claude-sonnet-4-6".into(),
                            name: "Claude Sonnet 4.6".into(),
                            provider_name: "anthropic".into(),
                            context_window: 200_000,
                            max_output_tokens: 32_000,
                            reasoning: true,
                            supports_adaptive_thinking: true,
                            supports_xhigh: false,
                            pricing: crate::agent::types::ModelPricing {
                                input: 3.0,
                                output: 15.0,
                                cache_read: 0.3,
                                cache_write: 3.75,
                            },
                        };
                        session.agent.state.model = Some(model.clone());
                        let _ = event_tx.send(AgentSessionEvent::ModelChanged { model });
                    }

                    // Show available models from this provider
                    let mut msg = String::from("Logged in to Anthropic.\n\nAvailable models:");
                    let current_id = session
                        .agent
                        .state
                        .model
                        .as_ref()
                        .map(|m| m.id.as_str())
                        .unwrap_or("");
                    for m in session.model_registry.all_models() {
                        if m.provider_name == "anthropic" {
                            let marker = if m.id == current_id { " *" } else { "" };
                            msg.push_str(&format!("\n  {} ({}){}", m.name, m.id, marker));
                        }
                    }
                    msg.push_str("\n\n/model <name> — switch model (e.g. /model opus)");
                    let _ = event_tx.send(AgentSessionEvent::Status {
                        message: msg,
                        is_error: false,
                    });
                }
                Err(e) => {
                    let _ = event_tx.send(AgentSessionEvent::Status {
                        message: format!("Login failed: {}", e),
                        is_error: true,
                    });
                }
            }
        }
        _ => {
            let _ = event_tx.send(AgentSessionEvent::Status {
                message: format!("Unknown provider: {}. Supported: anthropic", provider),
                is_error: true,
            });
        }
    }
}

fn last_assistant(messages: &[AgentMessage]) -> Option<&AssistantMessage> {
    messages.iter().rev().find_map(|m| match m {
        AgentMessage::Assistant(a) => Some(a),
        _ => None,
    })
}

/// The session task — runs in a dedicated thread, processes commands sequentially.
pub fn session_task(
    cmd_rx: crossbeam_channel::Receiver<SessionCommand>,
    event_tx: Sender<AgentSessionEvent>,
    mut session: AgentSession,
) {
    while let Ok(cmd) = cmd_rx.recv() {
        match cmd {
            SessionCommand::Prompt { text } => session.prompt(text, &event_tx),
            SessionCommand::Abort => session.abort(),
            SessionCommand::NewSession => {
                let _ = session
                    .session_manager
                    .new_session(&session.cwd, session.worktree.as_deref());
                session.agent.state.messages.clear();
                session.session_cost = Cost::default();
            }
            SessionCommand::LoadSession { id } => session.load_session(&id, &event_tx),
            SessionCommand::SetModel { provider, model_id } => {
                session.set_model(&provider, &model_id, &event_tx)
            }
            SessionCommand::SetThinkingLevel { level } => {
                session.set_thinking_level(level, &event_tx)
            }
            SessionCommand::SetEffortLevel { level } => {
                session.agent.state.effort_level = level;
                let _ = event_tx.send(AgentSessionEvent::EffortLevelChanged { level });
            }
            SessionCommand::SetPlanMode { enabled } => {
                session.set_plan_mode(enabled, &event_tx);
            }
            SessionCommand::SetCompactThreshold { pct } => {
                let frac = (pct as f64 / 100.0).clamp(0.01, 1.0);
                session.compaction_settings.threshold_pct = frac;
                session.session_manager.set_compact_threshold(frac);
                let _ = event_tx.send(AgentSessionEvent::CompactThresholdChanged { pct });
                let _ = event_tx.send(AgentSessionEvent::Status {
                    message: format!("Auto-compact threshold set to {}%.", pct),
                    is_error: false,
                });
            }
            SessionCommand::Compact {
                custom_instructions,
            } => {
                let _ = event_tx.send(AgentSessionEvent::AutoCompactionStart {
                    reason: CompactionReason::Manual,
                });
                match session.run_compaction(custom_instructions) {
                    Ok(Some(result)) => {
                        session.reload_agent_context();
                        let _ = event_tx.send(AgentSessionEvent::AutoCompactionEnd {
                            summary: Some(result.summary),
                            will_retry: false,
                            messages: session.agent.state.messages.clone(),
                        });
                    }
                    Ok(None) => {
                        let _ = event_tx.send(AgentSessionEvent::AutoCompactionEnd {
                            summary: None,
                            will_retry: false,
                            messages: vec![],
                        });
                        let _ = event_tx.send(AgentSessionEvent::Status {
                            message: "Nothing to compact — context is already short enough.".into(),
                            is_error: false,
                        });
                    }
                    Err(e) => {
                        let _ = event_tx.send(AgentSessionEvent::AutoCompactionEnd {
                            summary: None,
                            will_retry: false,
                            messages: vec![],
                        });
                        let _ = event_tx.send(AgentSessionEvent::Status {
                            message: e,
                            is_error: true,
                        });
                    }
                }
            }
            SessionCommand::ExportJsonl => {
                let sid = session.session_manager.session_id().to_string();
                let sid_short = if sid.len() >= 8 { &sid[..8] } else { &sid };
                let leaf = session.session_manager.leaf_id().unwrap_or("").to_string();
                let leaf_short = if leaf.len() >= 6 { &leaf[..6] } else { &leaf };
                let exports_dir = crate::nerv_dir().join("exports");
                let _ = std::fs::create_dir_all(&exports_dir);
                let path = exports_dir.join(format!("{sid_short}-{leaf_short}.jsonl"));
                let result = if let Some(content) = session.session_manager.export_jsonl() {
                    std::fs::write(&path, content)
                        .map(|_| path.to_string_lossy().to_string())
                        .map_err(|e| e.to_string())
                } else {
                    Err("no active session".into())
                };
                let _ = event_tx.send(AgentSessionEvent::ExportDone { result });
            }
            SessionCommand::ExportHtml => {
                let sid = session.session_manager.session_id().to_string();
                let sid_short = if sid.len() >= 8 { &sid[..8] } else { &sid };
                let leaf = session.session_manager.leaf_id().unwrap_or("").to_string();
                let leaf_short = if leaf.len() >= 6 { &leaf[..6] } else { &leaf };
                let exports_dir = crate::nerv_dir().join("exports");
                let _ = std::fs::create_dir_all(&exports_dir);
                let path = exports_dir.join(format!("{sid_short}-{leaf_short}.html"));
                // Export only the current branch (root → leaf), not all branches.
                let branch_entries = session.session_manager.current_branch_entries();
                let result = crate::export::export_entries_html(
                    &branch_entries,
                    &session.agent.state.messages,
                    &path,
                );
                let _ = event_tx.send(AgentSessionEvent::ExportDone { result });
            }
            SessionCommand::Login { provider } => {
                handle_login(&provider, &mut session, &event_tx);
            }
            SessionCommand::ListSessions { repo_root } => {
                let mut sessions = session.session_manager.list_sessions();
                // Filter by repo root if provided
                if let Some(ref root) = repo_root {
                    sessions.retain(|s| s.cwd.starts_with(root.as_str()));
                }
                let _ = event_tx.send(AgentSessionEvent::SessionList { sessions });
            }
            SessionCommand::GetTree => {
                let tree = session.session_manager.get_tree();
                let current_leaf = session.session_manager.leaf_id().map(|s| s.to_string());
                let _ = event_tx.send(AgentSessionEvent::TreeData { tree, current_leaf });
            }
            SessionCommand::SwitchBranch { entry_id, use_parent, reset_leaf } => {
                if reset_leaf {
                    session.session_manager.reset_leaf();
                } else if use_parent {
                    // Find the parent of entry_id and branch to it
                    let parent = session.session_manager.entries()
                        .iter()
                        .find(|e| e.id() == entry_id)
                        .and_then(|e| e.parent_id())
                        .map(|s| s.to_string());
                    if let Some(ref pid) = parent {
                        session.session_manager.branch(pid);
                    } else {
                        // No parent — treat as reset (root node)
                        session.session_manager.reset_leaf();
                    }
                } else {
                    session.session_manager.branch(&entry_id);
                }
                let ctx = session.session_manager.build_session_context();
                session.agent.state.messages = ctx.messages;
                session.agent.state.thinking_level = ctx.thinking_level;
                let _ = event_tx.send(AgentSessionEvent::ThinkingLevelChanged {
                    level: ctx.thinking_level,
                });
                if let Some((provider, model_id)) = ctx.model {
                    session.set_model(&provider, &model_id, &event_tx);
                }
                let _ = event_tx.send(AgentSessionEvent::SessionLoaded {
                    messages: session.agent.state.messages.clone(),
                });
            }
            SessionCommand::CreateWorktree {
                branch_name,
                nerv_dir,
            } => {
                // Allow if no session, or session exists but has no entries yet (e.g. after /new)
                if session.session_manager.entry_count() > 0 {
                    let _ = event_tx.send(AgentSessionEvent::Status {
                        message: "/wt only works before the first prompt. Use /new first.".into(),
                        is_error: true,
                    });
                    continue;
                }
                let repo_root = match crate::find_repo_root(&session.cwd) {
                    Some(r) => r,
                    None => {
                        let _ = event_tx.send(AgentSessionEvent::Status {
                            message: "Not in a git repository.".into(),
                            is_error: true,
                        });
                        continue;
                    }
                };
                // Generate session ID prefix for branch naming
                let prefix = &crate::session::types::gen_session_id()[..8];
                match crate::worktree::create_worktree(
                    &repo_root,
                    &nerv_dir,
                    &branch_name,
                    prefix,
                ) {
                    Ok(wt_path) => {
                        session.set_worktree(wt_path.clone());
                        // Update existing session's DB record if one was already created
                        if session.session_manager.has_session() {
                            session
                                .session_manager
                                .update_worktree(&wt_path, &wt_path);
                        }
                        let _ = event_tx.send(AgentSessionEvent::WorktreeCreated {
                            path: wt_path,
                        });
                    }
                    Err(e) => {
                        let _ = event_tx.send(AgentSessionEvent::Status {
                            message: format!("Worktree creation failed: {}", e),
                            is_error: true,
                        });
                    }
                }
            }
            SessionCommand::MergeWorktree => {
                let wt_path = match session.worktree.as_ref() {
                    Some(p) => p.clone(),
                    None => {
                        let _ = event_tx.send(AgentSessionEvent::Status {
                            message: "No worktree attached to this session.".into(),
                            is_error: true,
                        });
                        continue;
                    }
                };
                match crate::worktree::merge_worktree(&wt_path) {
                    Ok(main_wt) => {
                        session.cwd = main_wt.clone();
                        session.worktree = None;
                        session.session_manager.clear_worktree();
                        let _ = event_tx.send(AgentSessionEvent::WorktreeMerged {
                            original_path: main_wt,
                            message: "Worktree merged and removed.".into(),
                        });
                    }
                    Err(e) => {
                        let _ = event_tx.send(AgentSessionEvent::Status {
                            message: format!("Merge failed: {}", e),
                            is_error: true,
                        });
                    }
                }
            }
        }
    }
}


