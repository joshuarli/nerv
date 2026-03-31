use std::sync::atomic::Ordering;

use crossbeam_channel::Sender;

use crate::agent::types::Cost;

use super::agent_session::{AgentSession, AgentSessionEvent, CompactionReason, SessionCommand};
use crate::str::StrExt as _;

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
                    let extra_headers: Vec<(String, String)> =
                        session.config.effective_headers("anthropic");
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
                        session.agent.set_model(Some(model.clone()));
                        let _ = event_tx.send(AgentSessionEvent::ModelChanged { model });
                    }

                    // Show available models from this provider
                    let mut msg = String::from("Logged in to Anthropic.\n\nAvailable models:");
                    let current_id =
                        session.agent.state.model.as_ref().map(|m| m.id.as_str()).unwrap_or("");
                    for m in session.model_registry.all_models() {
                        if m.provider_name == "anthropic" {
                            let marker = if m.id == current_id { " *" } else { "" };
                            msg.push_str(&format!("\n  {} ({}){}", m.name, m.id, marker));
                        }
                    }
                    msg.push_str("\n\n/model <name> — switch model (e.g. /model opus)");
                    let _ =
                        event_tx.send(AgentSessionEvent::Status { message: msg, is_error: false });
                }
                Err(e) => {
                    let _ = event_tx.send(AgentSessionEvent::Status {
                        message: format!("Login failed: {}", e),
                        is_error: true,
                    });
                }
            }
        }
        "codex" => {
            let tx = event_tx.clone();
            let result = super::auth::login_codex(
                &|url| {
                    let _ = tx.send(AgentSessionEvent::Status {
                        message: format!(
                            "Opening browser for Codex login...\n\nIf the browser doesn't open, visit:\n{}",
                            url
                        ),
                        is_error: false,
                    });
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
                    auth.set("codex", super::auth::Credential::OAuth(creds));

                    let provider = std::sync::Arc::new(crate::agent::CodexProvider::new(api_key));
                    session.agent.provider_registry.write().unwrap().register("codex", provider);

                    // Default to the first available codex model if none is set.
                    if session.agent.state.model.is_none() {
                        if let Some(model) = session
                            .model_registry
                            .all_models()
                            .into_iter()
                            .find(|m| m.provider_name == "codex")
                            .cloned()
                        {
                            session.agent.set_model(Some(model.clone()));
                            let _ = event_tx.send(AgentSessionEvent::ModelChanged { model });
                        }
                    }

                    let mut msg = String::from("Logged in to Codex.\n\nAvailable models:");
                    let current_id =
                        session.agent.state.model.as_ref().map(|m| m.id.as_str()).unwrap_or("");
                    for m in session.model_registry.all_models() {
                        if m.provider_name == "codex" {
                            let marker = if m.id == current_id { " *" } else { "" };
                            msg.push_str(&format!("\n  {} ({}){}", m.name, m.id, marker));
                        }
                    }
                    msg.push_str("\n\n/model <name> — switch model");
                    let _ =
                        event_tx.send(AgentSessionEvent::Status { message: msg, is_error: false });
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
                message: format!("Unknown provider: {}. Supported: anthropic, codex", provider),
                is_error: true,
            });
        }
    }
}


/// The session task — runs in a dedicated thread, processes commands
/// sequentially.
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
                let _ =
                    session.session_manager.new_session(&session.cwd, session.worktree.as_deref());
                session.agent.clear_messages();
                session.session_cost = Cost::default();
                session.session_named = false;
                let _ = event_tx.send(AgentSessionEvent::SessionStarted {
                    id: session.session_manager.session_id().to_string(),
                    name: None,
                });
                let _ = event_tx.send(AgentSessionEvent::SessionLoaded {
                    messages: vec![],
                    cost_usd: 0.0,
                    total_input: 0,
                    total_output: 0,
                    api_calls: 0,
                    input_history: vec![],
                });
            }
            SessionCommand::LoadSession { id } => session.load_session(&id, &event_tx),
            SessionCommand::SetModel { provider, model_id } => {
                session.set_model(&provider, &model_id, &event_tx)
            }
            SessionCommand::SetThinkingLevel { level } => {
                session.set_thinking_level(level, &event_tx)
            }
            SessionCommand::SetEffortLevel { level } => {
                session.agent.set_effort_level(level);
                // Persist as session-level override
                session.session_manager.update_session_config(|cfg| {
                    cfg.default_effort_level = level;
                });
                let _ = event_tx.send(AgentSessionEvent::EffortLevelChanged { level });
            }
            SessionCommand::SetPlanMode { enabled } => {
                session.set_plan_mode(enabled, &event_tx);
            }
            SessionCommand::PlanAnswers { answers } => {
                session.inject_plan_answers(answers, &event_tx);
            }
            SessionCommand::PlanFollowUp => {
                session.inject_plan_followup(&event_tx);
            }
            SessionCommand::ExecutePlan => {
                // Capture path before set_plan_mode clears it.
                let plan_path = session.plan_path.clone();
                session.set_plan_mode(false, &event_tx);
                if let Some(path) = plan_path {
                    let text = format!(
                        "Implement the plan at `{}`. Work through it step by step.",
                        path.display()
                    );
                    session.prompt(text, &event_tx);
                }
            }
            SessionCommand::ForkSession => match session.session_manager.fork_session() {
                Ok(new_id) => {
                    let short = new_id.truncate_chars(8);
                    let _ = event_tx.send(AgentSessionEvent::SessionStarted {
                        id: new_id.clone(),
                        name: session.session_manager.name(),
                    });
                    let _ = event_tx.send(AgentSessionEvent::Status {
                        message: format!("Forked to new session {short}."),
                        is_error: false,
                    });
                }
                Err(e) => {
                    let _ = event_tx.send(AgentSessionEvent::Status {
                        message: format!("Fork failed: {e}"),
                        is_error: true,
                    });
                }
            },
            SessionCommand::SaveInputHistory { history } => {
                session.session_manager.save_input_history(&history);
            }
            SessionCommand::RecordBtw { note, response, model_id } => {
                let _ = session.session_manager.append_btw(&note, &response, &model_id);
            }
            SessionCommand::SetCompactThreshold { pct } => {
                let frac = (pct as f64 / 100.0).clamp(0.01, 1.0);
                session.compaction.settings.threshold_pct = frac;
                session.compaction.threshold_pct.store(pct as u32, Ordering::Relaxed);
                session.session_manager.set_compact_threshold(frac);
                let _ = event_tx.send(AgentSessionEvent::CompactThresholdChanged { pct });
                let _ = event_tx.send(AgentSessionEvent::Status {
                    message: format!("Auto-compact threshold set to {}%.", pct),
                    is_error: false,
                });
            }
            SessionCommand::SetAutoCompact { enabled } => {
                session.compaction.auto_compact = enabled;
                session.session_manager.update_session_config(|cfg| {
                    cfg.auto_compact = Some(enabled);
                });
                let label = if enabled { "on" } else { "off" };
                let _ = event_tx.send(AgentSessionEvent::Status {
                    message: format!("Auto-compact {}.", label),
                    is_error: false,
                });
            }
            SessionCommand::Compact { custom_instructions } => {
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
                        let _ =
                            event_tx.send(AgentSessionEvent::Status { message: e, is_error: true });
                    }
                }
            }
            SessionCommand::Export => {
                let sid = session.session_manager.session_id().to_string();
                let sid_short = if sid.len() >= 8 { &sid[..8] } else { &sid };
                let leaf = session.session_manager.leaf_id().unwrap_or("").to_string();
                let leaf_short = if leaf.len() >= 6 { &leaf[..6] } else { &leaf };
                let exports_dir = crate::nerv_dir().join("exports");
                let _ = std::fs::create_dir_all(&exports_dir);

                // Export HTML — current branch (root → leaf) only.
                let html_path = exports_dir.join(format!("{sid_short}-{leaf_short}.html"));
                let branch_entries = session.session_manager.current_branch_entries();
                let html_result = crate::export::export_entries_html(
                    &branch_entries,
                    &session.agent.state.messages,
                    &html_path,
                );

                // Export JSONL.
                let jsonl_path = exports_dir.join(format!("{sid_short}-{leaf_short}.jsonl"));
                let jsonl_result = if let Some(content) = session.session_manager.export_jsonl() {
                    std::fs::write(&jsonl_path, content)
                        .map(|_| jsonl_path.to_string_lossy().to_string())
                        .map_err(|e| e.to_string())
                } else {
                    Err("no active session".into())
                };

                // Combine results into a single status message.
                let result = match (html_result, jsonl_result) {
                    (Ok(h), Ok(j)) => Ok(format!("{}\n{}", h, j)),
                    (Err(e), _) | (_, Err(e)) => Err(e),
                };
                let _ = event_tx.send(AgentSessionEvent::ExportDone { result });
            }
            SessionCommand::Login { provider } => {
                handle_login(&provider, &mut session, &event_tx);
            }
            SessionCommand::Logout { provider } => {
                // Remove credentials from storage.
                let nerv_dir = crate::nerv_dir();
                let mut auth = super::auth::AuthStorage::load(nerv_dir);
                auth.remove(&provider);

                // Unregister the provider so available_models() stops showing it.
                session.agent.provider_registry.write().unwrap().unregister(&provider);

                let _ = event_tx.send(AgentSessionEvent::Status {
                    message: format!("Logged out from {}.", provider),
                    is_error: false,
                });
            }
            SessionCommand::ListSessions { repo_root, repo_id } => {
                let mut sessions = session.session_manager.list_sessions();
                if let Some(ref rid) = repo_id {
                    // Filter by stable fingerprint (survives renames/moves).
                    sessions.retain(|s| s.repo_id.as_deref() == Some(rid.as_str()));
                } else if let Some(ref root) = repo_root {
                    // Non-git directory: fall back to cwd-prefix.
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
                    let parent = session
                        .session_manager
                        .entries()
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
                let full_history = ctx.full_history;
                let cost_usd = ctx.cost_usd;
                let total_input = ctx.total_input;
                let total_output = ctx.total_output;
                let api_calls = ctx.api_calls;
                let input_history = ctx.input_history;
                session.agent.set_messages(ctx.messages);
                session.agent.set_thinking_level(ctx.thinking_level);
                let _ = event_tx
                    .send(AgentSessionEvent::ThinkingLevelChanged { level: ctx.thinking_level });
                if let Some((provider, model_id)) = ctx.model {
                    session.set_model(&provider, &model_id, &event_tx);
                }
                let _ = event_tx.send(AgentSessionEvent::SessionLoaded {
                    messages: full_history,
                    cost_usd,
                    total_input,
                    total_output,
                    api_calls,
                    input_history,
                });
            }
            SessionCommand::CreateWorktree { branch_name, nerv_dir } => {
                // Allow if no session, or session exists but has no entries yet (e.g. after
                // /new)
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
                match crate::worktree::create_worktree(&repo_root, &nerv_dir, &branch_name, prefix)
                {
                    Ok(wt_path) => {
                        session.set_worktree(wt_path.clone());
                        // Update existing session's DB record if one was already created
                        if session.session_manager.has_session() {
                            session.session_manager.update_worktree(&wt_path, &wt_path);
                        }
                        let _ = event_tx.send(AgentSessionEvent::WorktreeCreated { path: wt_path });
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
