use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel::Sender;

use super::model_registry::ModelRegistry;
use super::resource_loader::LoadedResources;
use super::system_prompt::build_system_prompt;
use super::tool_registry::ToolRegistry;
use crate::agent::agent::Agent;
use crate::agent::types::*;
use crate::compaction::summarize::generate_summary;
use crate::compaction::{self, CompactionResult, CompactionSettings};
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
    /// A session is now active (created or loaded).
    SessionStarted {
        id: String,
    },
    /// Session loaded — clear UI and display history.
    SessionLoaded {
        messages: Vec<AgentMessage>,
    },
    /// Provider health check result (from background thread on startup).
    ProviderHealth {
        provider: String,
        online: bool,
    },
    /// Permission request — agent blocks until response is sent back.
    PermissionRequest {
        tool: String,
        args: serde_json::Value,
        reason: String,
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
    Compact { custom_instructions: Option<String> },
    ExportJsonl { path: PathBuf },
    ExportHtml { path: PathBuf },
    AddLocal,
    Login { provider: String },
    ListSessions { repo_root: Option<String> },
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
        }
    }

    pub fn cost(&self) -> &Cost {
        &self.session_cost
    }

    pub fn prompt(&mut self, text: String, event_tx: &Sender<AgentSessionEvent>) {
        // Lazily create session on first prompt (not on startup)
        if !self.session_manager.has_session() {
            let _ = self.session_manager.new_session(&self.cwd);
            let _ = event_tx.send(AgentSessionEvent::SessionStarted {
                id: self.session_manager.session_id().to_string(),
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
            self.agent.state.permission_fn = Some(std::sync::Arc::new(
                move |tool: &str, args: &serde_json::Value| {
                    let perm = super::permissions::check(tool, args, repo_root.as_deref());
                    match perm {
                        super::permissions::Permission::Allow => true,
                        super::permissions::Permission::Ask(reason) => {
                            let (resp_tx, resp_rx) = crossbeam_channel::bounded(1);
                            let _ = perm_tx.send(AgentSessionEvent::PermissionRequest {
                                tool: tool.to_string(),
                                args: args.clone(),
                                reason,
                                response_tx: resp_tx,
                            });
                            // Block until user responds
                            resp_rx.recv().unwrap_or(false)
                        }
                    }
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

            if let Some(result) = self.run_compaction(None) {
                let _ = event_tx.send(AgentSessionEvent::AutoCompactionEnd {
                    summary: Some(result.summary),
                    will_retry: true,
                });

                // Reload agent context from compacted session
                self.reload_agent_context();

                // Retry the original prompt
                let retry_msg = AgentMessage::User {
                    content: vec![ContentItem::Text { text }],
                    timestamp: now_millis(),
                };
                self.prepare_system_prompt();
                let _retry_messages = self.run_agent_prompt(vec![retry_msg], event_tx);
            } else {
                let _ = event_tx.send(AgentSessionEvent::AutoCompactionEnd {
                    summary: None,
                    will_retry: false,
                });
                let _ = event_tx.send(AgentSessionEvent::Status {
                    message: "Context overflow: auto-compact failed. Try /compact manually or reduce context.".into(),
                    is_error: true,
                });
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
                let result = self.run_compaction(None);
                if result.is_some() {
                    self.reload_agent_context();
                }
                let _ = event_tx.send(AgentSessionEvent::AutoCompactionEnd {
                    summary: result.map(|r| r.summary),
                    will_retry: false,
                });
            }
        }
    }

    fn prepare_system_prompt(&mut self) {
        // Reload memory in case it was updated by a tool call
        let nerv_dir = crate::home_dir().unwrap_or_default().join(".nerv");
        self.resources.memory = std::fs::read_to_string(nerv_dir.join("memory.md")).ok();

        self.agent.state.tools = self.tool_registry.active_tools();
        let tool_names: Vec<&str> = self.agent.state.tools.iter().map(|t| t.name()).collect();
        let snippets = self.tool_registry.prompt_snippets();
        let guidelines = self.tool_registry.prompt_guidelines();
        self.agent.state.system_prompt = build_system_prompt(
            &self.cwd,
            &self.resources,
            &tool_names,
            &snippets,
            &guidelines,
        );
    }

    /// Run agent.prompt() with event forwarding and persistence. Returns new messages.
    fn run_agent_prompt(
        &mut self,
        prompt_messages: Vec<AgentMessage>,
        event_tx: &Sender<AgentSessionEvent>,
    ) -> Vec<AgentMessage> {
        let tx = event_tx.clone();
        let input_tokens = std::sync::atomic::AtomicU32::new(0);

        let new_messages = self.agent.prompt(prompt_messages, &|event: AgentEvent| {
            if let AgentEvent::UsageUpdate { ref usage } = event {
                input_tokens.store(usage.input, std::sync::atomic::Ordering::Relaxed);
            }
            let _ = tx.send(AgentSessionEvent::Agent(event));
        });

        let input_tok = input_tokens.load(std::sync::atomic::Ordering::Relaxed);
        let context_window = self
            .agent
            .state
            .model
            .as_ref()
            .map(|m| m.context_window)
            .unwrap_or(0);

        // Persist new messages to SQLite with token metadata
        let mut running_output: u32 = 0;
        for msg in &new_messages {
            let tokens = if let AgentMessage::Assistant(a) = msg {
                let output = a.usage.as_ref().map(|u| u.output).unwrap_or(0);
                running_output += output;
                Some(crate::session::types::TokenInfo {
                    input: input_tok,
                    output,
                    context_used: input_tok + running_output,
                    context_window,
                })
            } else {
                None
            };
            let _ = self.session_manager.append_message(msg, tokens);
        }
        self.last_input_tokens = input_tok;

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

    pub fn run_compaction(
        &mut self,
        _custom_instructions: Option<String>,
    ) -> Option<CompactionResult> {
        let model = self.agent.state.model.as_ref()?;
        let provider = self
            .agent
            .provider_registry
            .read()
            .unwrap()
            .get(&model.provider_name)?;
        let _messages = &self.agent.state.messages;

        let entries = self.session_manager.entries();
        if entries.is_empty() {
            return None;
        }

        // Find cut point using token-budget-aware algorithm
        let cut = compaction::find_cut_point(
            entries,
            0,
            entries.len(),
            self.compaction_settings.keep_recent_tokens,
        );
        let first_kept_id = entries[cut.first_kept_entry_index].id().to_string();

        // Collect messages before the cut point for summarization
        let to_summarize: Vec<AgentMessage> = entries[..cut.first_kept_entry_index]
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
            return None;
        }

        let tokens_before = to_summarize
            .iter()
            .map(compaction::estimate_tokens)
            .sum::<usize>() as u32;

        match generate_summary(&to_summarize, None, provider, &model.id) {
            Ok(summary) => {
                let _ = self.session_manager.append_compaction(
                    summary.clone(),
                    first_kept_id.clone(),
                    tokens_before,
                );
                Some(CompactionResult {
                    summary,
                    first_kept_entry_id: first_kept_id,
                    tokens_before,
                })
            }
            Err(e) => {
                crate::log::error(&format!("compaction failed: {}", e));
                None
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
            let nerv_dir = crate::home_dir().unwrap_or_default().join(".nerv");
            let mut cfg = super::config::NervConfig::load(&nerv_dir);
            cfg.default_model = Some(model_id.to_string());
            let _ = cfg.save(&nerv_dir);
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

    pub fn add_local(&mut self, event_tx: &Sender<AgentSessionEvent>) {
        let base_url = "http://localhost:1234/v1";
        let models_url = format!("{}/models", base_url);

        let response = match crate::http::agent().get(&models_url).call() {
            Ok(r) => r,
            Err(e) => {
                let _ = event_tx.send(AgentSessionEvent::Status {
                    message: format!("Cannot reach local server at {} — is it running?", base_url),
                    is_error: true,
                });
                crate::log::error(&format!("local server connection error: {}", e));
                return;
            }
        };

        let body: serde_json::Value = match response.into_body().read_json() {
            Ok(v) => v,
            Err(e) => {
                let _ = event_tx.send(AgentSessionEvent::Status {
                    message: format!("Failed to parse local server response: {}", e),
                    is_error: true,
                });
                return;
            }
        };

        let models = body["data"].as_array();
        let first_id = models
            .and_then(|a| a.first())
            .and_then(|m| m["id"].as_str());

        let Some(first_model) = first_id else {
            let _ = event_tx.send(AgentSessionEvent::Status {
                message: "local server has no models loaded.".into(),
                is_error: true,
            });
            return;
        };

        let model = Model {
            id: first_model.to_string(),
            name: first_model.to_string(),
            provider_name: "local".to_string(),
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

        let provider = std::sync::Arc::new(crate::agent::OpenAICompatProvider::new(
            "local".into(),
            base_url.into(),
            None,
        ));
        self.agent
            .provider_registry
            .write()
            .unwrap()
            .register("local", provider);
        self.agent.state.model = Some(model.clone());
        let _ = event_tx.send(AgentSessionEvent::ModelChanged { model });
        let _ = event_tx.send(AgentSessionEvent::Status {
            message: format!("Connected to local server — model: {}", first_model),
            is_error: false,
        });

        let nerv_dir = crate::home_dir().unwrap_or_default().join(".nerv");
        let mut config = crate::core::config::NervConfig::load(&nerv_dir);
        config.custom_providers.retain(|p| p.name != "local");
        let custom_models: Vec<_> = models
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| {
                        m["id"]
                            .as_str()
                            .map(|id| crate::core::config::CustomModelConfig {
                                id: id.into(),
                                name: Some(id.into()),
                                context_window: Some(128_000),
                                reasoning: None,
                            })
                    })
                    .collect()
            })
            .unwrap_or_default();
        config
            .custom_providers
            .push(crate::core::config::CustomProviderConfig {
                name: "local".into(),
                base_url: base_url.into(),
                api_key: None,
                models: custom_models,
            });
        let _ = config.save(&nerv_dir);
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
                        let nerv_dir = crate::home_dir().unwrap_or_default().join(".nerv");
                        let config = crate::core::config::NervConfig::load(&nerv_dir);
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

                let _ = event_tx.send(AgentSessionEvent::SessionStarted {
                    id: self.session_manager.session_id().to_string(),
                });
                let _ = event_tx.send(AgentSessionEvent::SessionLoaded {
                    messages: self.agent.state.messages.clone(),
                });
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
                    let nerv_dir = crate::home_dir().unwrap_or_default().join(".nerv");
                    let mut auth = super::auth::AuthStorage::load(&nerv_dir);
                    let api_key = creds.access.clone();
                    auth.set("anthropic", super::auth::Credential::OAuth(creds));

                    // Register the provider (OAuth uses Bearer auth)
                    let nerv_config = super::config::NervConfig::load(&nerv_dir);
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
                let _ = session.session_manager.new_session(&session.cwd);
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
            SessionCommand::Compact {
                custom_instructions,
            } => {
                let _ = event_tx.send(AgentSessionEvent::AutoCompactionStart {
                    reason: CompactionReason::Manual,
                });
                let result = session.run_compaction(custom_instructions);
                if result.is_some() {
                    session.reload_agent_context();
                }
                let _ = event_tx.send(AgentSessionEvent::AutoCompactionEnd {
                    summary: result.map(|r| r.summary),
                    will_retry: false,
                });
            }
            SessionCommand::ExportJsonl { path } => {
                let result = if let Some(content) = session.session_manager.export_jsonl() {
                    std::fs::write(&path, content)
                        .map(|_| path.to_string_lossy().to_string())
                        .map_err(|e| e.to_string())
                } else {
                    Err("no active session".into())
                };
                let _ = event_tx.send(AgentSessionEvent::ExportDone { result });
            }
            SessionCommand::ExportHtml { path } => {
                let result = export_html(&session, &path);
                let _ = event_tx.send(AgentSessionEvent::ExportDone { result });
            }
            SessionCommand::AddLocal => session.add_local(&event_tx),
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
        }
    }
}

fn export_html(session: &AgentSession, path: &std::path::Path) -> Result<String, String> {
    let mut html = String::from(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>nerv session</title>
<style>
*{box-sizing:border-box}
body{font-family:-apple-system,system-ui,'Segoe UI',sans-serif;max-width:720px;margin:0 auto;padding:2rem 1rem;color:#1a1a1a;line-height:1.7;background:#fff}
.user{background:#f5f5f5;border-radius:8px;padding:0.75rem 1rem;margin:1.5rem 0;font-weight:500}
.assistant{margin:1.5rem 0}
.assistant h1,.assistant h2,.assistant h3{margin:1rem 0 0.5rem;font-weight:600}
.assistant h1{font-size:1.4rem}
.assistant h2{font-size:1.2rem}
.assistant h3{font-size:1.1rem}
.assistant code{background:#f0f0f0;padding:0.15em 0.4em;border-radius:3px;font-size:0.9em;font-family:'SF Mono',Menlo,monospace}
.assistant pre{background:#f7f7f7;padding:1rem;border-radius:6px;overflow-x:auto;border:1px solid #e5e5e5}
.assistant pre code{background:none;padding:0}
.assistant blockquote{border-left:3px solid #ddd;padding-left:1rem;color:#555;margin:0.75rem 0}
.assistant ul,.assistant ol{padding-left:1.5rem}
.tool{background:#f9f9f9;border:1px solid #eee;border-radius:6px;padding:0.75rem;margin:0.5rem 0;font-family:'SF Mono',Menlo,monospace;font-size:0.8rem;white-space:pre-wrap;color:#555;max-height:300px;overflow-y:auto}
.meta{font-size:0.75rem;color:#999;margin-top:0.25rem}
hr{border:none;border-top:1px solid #eee;margin:2rem 0}
</style>
</head>
<body>
"#,
    );

    // Get entries — prefer session DB, fall back to agent state
    let entries = session.session_manager.entries();
    let from_agent;
    let entries = if entries.is_empty() {
        from_agent = session
            .agent
            .state
            .messages
            .iter()
            .map(|msg| {
                crate::session::types::SessionEntry::Message(crate::session::types::MessageEntry {
                    id: String::new(),
                    parent_id: None,
                    timestamp: String::new(),
                    message: msg.clone(),
                    tokens: None,
                })
            })
            .collect::<Vec<_>>();
        &from_agent
    } else {
        entries
    };

    for entry in entries {
        if let crate::session::types::SessionEntry::SystemPrompt(sp) = entry {
            html.push_str(&format!(
                "<details><summary class='meta'>System prompt ({} tok)</summary><pre class='tool'>{}</pre></details>\n",
                sp.token_count,
                html_escape(&sp.prompt),
            ));
            continue;
        }
        if let crate::session::types::SessionEntry::Message(me) = entry {
            match &me.message {
                AgentMessage::User { content, .. } => {
                    html.push_str("<div class='user'>");
                    for item in content {
                        if let ContentItem::Text { text } = item {
                            html.push_str(&html_escape(text));
                        }
                    }
                    html.push_str("</div>\n");
                }
                AgentMessage::Assistant(a) => {
                    html.push_str("<div class='assistant'>");
                    let text = a.text_content();
                    html.push_str(&markdown_to_html(&text));
                    if let Some(ref tok) = me.tokens {
                        html.push_str(&format!(
                            "<div class='meta'>↑{} ↓{} · {}/{} context</div>",
                            tok.input, tok.output, tok.context_used, tok.context_window,
                        ));
                    }
                    html.push_str("</div>\n");
                }
                AgentMessage::ToolResult { content, .. } => {
                    html.push_str("<div class='tool'>");
                    for item in content {
                        if let ContentItem::Text { text } = item {
                            html.push_str(&html_escape(text));
                        }
                    }
                    html.push_str("</div>\n");
                }
                _ => {}
            }
        }
    }

    html.push_str("</body>\n</html>\n");
    std::fs::write(path, &html).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().to_string())
}

fn markdown_to_html(markdown: &str) -> String {
    let parser = pulldown_cmark::Parser::new(markdown);
    let mut html_output = String::new();
    pulldown_cmark::html::push_html(&mut html_output, parser);
    html_output
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\n', "<br>")
}
