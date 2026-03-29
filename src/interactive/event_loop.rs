use crossbeam_channel as channel;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use super::layout::AppLayout;
use super::theme;
use crate::agent::provider::{CancelFlag, ProviderRegistry};
use crate::agent::types::*;
use crate::core::model_registry::ModelRegistry;
use crate::core::*;
use crate::session::types::SessionTreeNode;
use crate::tui;

/// Returned by [`InteractiveMode::handle_event`] when a full-screen picker
/// should be launched by the caller.
pub enum PickerRequest {
    /// Open the session picker.  Sessions are ready; search_fn is synchronous.
    SessionPicker {
        sessions: Vec<crate::session::manager::SessionSummary>,
        repo_root: Option<String>,
    },
    /// Open the session tree selector.
    TreeSelector {
        tree: Vec<SessionTreeNode>,
        current_leaf: Option<String>,
    },
    /// Open the model picker.
    ModelPicker,
    /// Open the /btw ephemeral overlay.
    BtwOverlay {
        messages: Vec<AgentMessage>,
        system_prompt: String,
        tools: Vec<std::sync::Arc<dyn crate::agent::agent::AgentTool>>,
        model: Model,
        note: String,
    },
    /// Toggle the nervHud line on/off.
    ToggleHud,
}

pub struct InteractiveMode {
    cmd_tx: channel::Sender<SessionCommand>,
    pub is_streaming: bool,
    pub is_compacting: bool,
    current_model: Option<Model>,
    current_thinking: ThinkingLevel,
    current_effort: Option<EffortLevel>,
    model_registry: Arc<ModelRegistry>,
    /// Shared provider registry — cloned Arc from the session, used by the /btw overlay.
    pub provider_registry: Arc<RwLock<ProviderRegistry>>,
    /// Snapshot of the full agent state as of the last AgentEnd — used by /btw.
    messages_snapshot: Vec<AgentMessage>,
    system_prompt_snapshot: String,
    tools_snapshot: Vec<std::sync::Arc<dyn crate::agent::agent::AgentTool>>,
    skills: Vec<crate::core::skills::Skill>,
    repo_root: Option<String>,
    /// Stable repo fingerprint (SHA of initial commit) — rename-safe session filter.
    repo_id: Option<String>,
    pub session_id: Option<String>,
    pub status_message: Option<String>,
    pub status_is_error: bool,
    pub quit_requested: bool,
    /// The plain text of the last completed assistant response (for /copy).
    last_response: Option<String>,
    /// Pending messages queued while streaming.
    pub pending_messages: Vec<String>,
    /// If editing a queued message, which index.
    pub editing_queue_idx: Option<usize>,
    /// History of submitted user messages for up-arrow recall.
    message_history: Vec<String>,
    /// Current position in history (None = not browsing).
    pub history_index: Option<usize>,
    /// Saved editor text when entering history browse.
    history_saved_text: String,
    /// Pending permission request — waiting for y/n from user.
    pub pending_permission: Option<crossbeam_channel::Sender<bool>>,
    /// Current permission request details (tool, args) to record if accepted
    pub pending_permission_details: Option<(String, serde_json::Value)>,
    /// Plan mode: read-only research mode, no file mutations.
    pub plan_mode: bool,
    /// Current auto-compact threshold (0–100). Mirrors what was last sent to the session.
    pub compact_threshold: u8,
    /// Directories the user has granted full access to (shared with the session thread).
    pub allowed_dirs: Arc<Mutex<Vec<PathBuf>>>,
    /// Shared cancel flag — set this to interrupt a running stream immediately.
    cancel_flag: CancelFlag,
    /// Shared compact threshold (percent 0–100) — written directly so `/compact at N`
    /// takes effect before the next turn without going through cmd_tx.
    compact_threshold_arc: Arc<AtomicU32>,
}

impl InteractiveMode {
    pub fn new(
        cmd_tx: channel::Sender<SessionCommand>,
        model_registry: Arc<ModelRegistry>,
        provider_registry: Arc<RwLock<ProviderRegistry>>,
        tools_snapshot: Vec<std::sync::Arc<dyn crate::agent::agent::AgentTool>>,
        initial_model: Option<Model>,
        initial_thinking: ThinkingLevel,
        initial_effort: Option<EffortLevel>,
        skills: Vec<crate::core::skills::Skill>,
        repo_root: Option<String>,
        repo_id: Option<String>,
        allowed_dirs: Arc<Mutex<Vec<PathBuf>>>,
        cancel_flag: CancelFlag,
        compact_threshold_arc: Arc<AtomicU32>,
    ) -> Self {
        Self {
            cmd_tx,
            is_streaming: false,
            is_compacting: false,
            current_model: initial_model,
            current_thinking: initial_thinking,
            current_effort: initial_effort,
            model_registry,
            provider_registry,
            messages_snapshot: Vec::new(),
            system_prompt_snapshot: String::new(),
            tools_snapshot,
            skills,
            repo_root,
            repo_id,
            session_id: None,
            status_message: None,
            status_is_error: false,
            quit_requested: false,
            last_response: None,
            pending_messages: Vec::new(),
            editing_queue_idx: None,
            message_history: Vec::new(),
            history_index: None,
            history_saved_text: String::new(),
            pending_permission: None,
            pending_permission_details: None,
            plan_mode: false,
            compact_threshold: 50,
            allowed_dirs,
            cancel_flag,
            compact_threshold_arc,
        }
    }

    pub fn handle_event(
        &mut self,
        event: AgentSessionEvent,
        layout: &mut AppLayout,
        tui: &mut tui::TUI,
    ) -> Option<PickerRequest> {
        match event {
            AgentSessionEvent::Agent(agent_event) => {
                self.handle_agent_event(agent_event, layout, tui);
            }
            AgentSessionEvent::ModelChanged { model } => {
                layout.footer.set_model(&model);
                self.current_model = Some(model);
                tui.request_render(false);
            }
            AgentSessionEvent::ThinkingLevelChanged { level } => {
                self.current_thinking = level;
                layout.footer.set_thinking(level);
            }
            AgentSessionEvent::EffortLevelChanged { level: effort } => {
                self.current_effort = effort;
                layout.footer.set_effort(effort);
            }
            AgentSessionEvent::PlanModeChanged { enabled } => {
                self.plan_mode = enabled;
                layout.footer.set_plan_mode(enabled);
                let label = if enabled { "Plan mode on" } else { "Plan mode off" };
                self.status_message = Some(label.into());
            }
            AgentSessionEvent::SessionNamed { name } => {
                layout.footer.set_session_name(Some(name));
                tui.request_render(false);
            }
            AgentSessionEvent::CompactThresholdChanged { pct } => {
                self.compact_threshold = pct;
                layout.footer.set_compact_threshold(pct);
            }
            AgentSessionEvent::Status { message, is_error } => {
                self.status_message = Some(message);
                self.status_is_error = is_error;
            }
            AgentSessionEvent::SessionList { sessions } => {
                return Some(PickerRequest::SessionPicker {
                    sessions,
                    repo_root: self.repo_root.clone(),
                });
            }
            AgentSessionEvent::TreeData { tree, current_leaf } => {
                let total_nodes = count_tree_nodes(&tree);
                if total_nodes == 0 {
                    self.status_message = Some("No entries in session.".into());
                } else {
                    return Some(PickerRequest::TreeSelector { tree, current_leaf });
                }
            }
            AgentSessionEvent::ExportDone { result } => match result {
                Ok(paths) => {
                    // paths contains newline-separated export paths (html + jsonl)
                    let msg = paths
                        .lines()
                        .map(|p| format!("Exported to {}", p))
                        .collect::<Vec<_>>()
                        .join("\n");
                    self.status_message = Some(msg);
                }
                Err(e) => {
                    self.status_message = Some(format!("Export failed: {}", e));
                    self.status_is_error = true;
                }
            },
            AgentSessionEvent::PermissionRequest {
                tool,
                args,
                reason,
                response_tx,
                ..
            } => {
                // Show the full command/path so the user can make an informed decision.
                // For bash, pull out the command string directly; for others show the reason.
                let detail = if tool == "bash" {
                    args["command"]
                        .as_str()
                        .unwrap_or(&reason)
                        .to_string()
                } else {
                    reason.clone()
                };
                self.status_message = Some(format!(
                    "⚠ Permission: {}\n  {}\n  y = allow, n = deny, a = allow dir",
                    tool, detail
                ));
                self.status_is_error = true;
                self.pending_permission = Some(response_tx);
                self.pending_permission_details = Some((tool.clone(), args.clone()));
            }
            AgentSessionEvent::ContextGateRequest {
                estimated_tokens,
                prev_tokens,
                context_window,
                response_tx,
            } => {
                let delta = estimated_tokens.saturating_sub(prev_tokens);
                let pct = if prev_tokens > 0 {
                    (delta as f64 / prev_tokens as f64 * 100.0) as u32
                } else {
                    0
                };
                self.status_message = Some(format!(
                    "⚠ Context grew {}k → {}k (+{}%, {}/{}k window)\n  y = continue, n = abort",
                    prev_tokens / 1000,
                    estimated_tokens / 1000,
                    pct,
                    estimated_tokens / 1000,
                    context_window / 1000,
                ));
                self.status_is_error = true;
                self.pending_permission = Some(response_tx);
                self.pending_permission_details = None;
            }
            AgentSessionEvent::OutputGateRequest {
                command,
                line_count,
                estimated_tokens,
                response_tx,
            } => {
                // Show the command, size, and hint so user can make an informed decision.
                // Truncate long commands for display.
                let cmd_display = if command.len() > 80 {
                    let end = command.floor_char_boundary(80);
                    format!("{}…", &command[..end])
                } else {
                    command.clone()
                };
                self.status_message = Some(format!(
                    "⚠ Output gate: bash\n  {}\n  {} lines / ~{}k tokens\n  y = allow, n = deny (model gets hint to retry)",
                    cmd_display,
                    line_count,
                    estimated_tokens / 1000,
                ));
                self.status_is_error = true;
                self.pending_permission = Some(response_tx);
                self.pending_permission_details = None;
            }
            AgentSessionEvent::WorktreeCreated { path } => {
                layout.footer.set_cwd(&path.to_string_lossy());
                self.status_message =
                    Some(format!("Worktree created: {}", path.display()));
            }
            AgentSessionEvent::WorktreeMerged {
                original_path,
                message,
            } => {
                layout.footer.set_cwd(&original_path.to_string_lossy());
                self.status_message = Some(message);
            }
            AgentSessionEvent::SessionStarted { id, name } => {
                self.session_id = Some(id.clone());
                // Clear accumulated messages on any new/reset session.
                self.messages_snapshot.clear();
                layout.footer.set_session_id(id);
                if let Some(n) = name {
                    layout.footer.set_session_name(Some(n));
                } else {
                    layout.footer.set_session_name(None);
                }
            }
            AgentSessionEvent::SessionLoaded { messages, cost_usd, total_input, total_output, api_calls, input_history } => {
                // Dump full history to terminal scrollback
                let mut scrollback = String::new();
                for msg in &messages {
                    match msg {
                        AgentMessage::User { content, .. } => {
                            let text: String = content
                                .iter()
                                .filter_map(|c| match c {
                                    ContentItem::Text { text } => Some(text.as_str()),
                                    _ => None,
                                })
                                .collect::<Vec<_>>()
                                .join("");
                            scrollback.push_str(&format!("> {}\n\n", text));
                        }
                        AgentMessage::Assistant(a) => {
                            for block in &a.content {
                                match block {
                                    ContentBlock::Thinking { thinking } if !thinking.is_empty() => {
                                        for line in thinking.lines() {
                                            scrollback.push_str(&format!("│ {}\n", line));
                                        }
                                        scrollback.push('\n');
                                    }
                                    ContentBlock::Text { text } if !text.is_empty() => {
                                        scrollback.push_str(text);
                                        scrollback.push_str("\n\n");
                                    }
                                    _ => {}
                                }
                            }
                        }
                        AgentMessage::ToolResult { content, .. } => {
                            for item in content {
                                if let ContentItem::Text { text } = item {
                                    let preview = if text.len() > 200 {
                                        let end = text.floor_char_boundary(200);
                                        &text[..end]
                                    } else {
                                        text.as_str()
                                    };
                                    scrollback.push_str(&format!("  {}\n", preview));
                                }
                            }
                        }
                        _ => {}
                    }
                }
                if !scrollback.is_empty() {
                    tui.dump_scrollback(&scrollback);
                }

                // Show recent context via ChatWriter
                layout.chat.clear();
                let recent = if messages.len() > 6 {
                    &messages[messages.len() - 6..]
                } else {
                    &messages
                };
                for msg in recent {
                    match msg {
                        AgentMessage::User { content, .. } => {
                            let text: String = content
                                .iter()
                                .filter_map(|c| match c {
                                    ContentItem::Text { text } => Some(text.as_str()),
                                    _ => None,
                                })
                                .collect::<Vec<_>>()
                                .join("");
                            layout.chat.push_user(&text);
                        }
                        AgentMessage::Assistant(a) => {
                            for block in &a.content {
                                match block {
                                    ContentBlock::Thinking { thinking } if !thinking.is_empty() => {
                                        layout.chat.push_styled(
                                            theme::THINKING,
                                            &format!(
                                                "│ {}",
                                                thinking.lines().collect::<Vec<_>>().join("\n│ ")
                                            ),
                                        );
                                    }
                                    ContentBlock::Text { text } if !text.is_empty() => {
                                        layout.chat.push_markdown_source(text);
                                    }
                                    _ => {}
                                }
                            }
                        }
                        AgentMessage::ToolResult {
                            content, is_error, ..
                        } => {
                            let text: String = content
                                .iter()
                                .filter_map(|c| match c {
                                    ContentItem::Text { text } => Some(text.as_str()),
                                    _ => None,
                                })
                                .collect::<Vec<_>>()
                                .join("");
                            if !text.is_empty() {
                                layout.chat.push_tool_result(&text, *is_error);
                            }
                        }
                        _ => {}
                    }
                }
                // Estimate context tokens from loaded messages
                let context_tokens: usize = messages
                    .iter()
                    .map(crate::compaction::estimate_tokens)
                    .sum();
                layout.footer.reset_context();
                layout.footer.set_context_used(context_tokens as u32);
                // Restore accumulated stats from the session DB (after reset_context clears them).
                layout.footer.restore_stats(cost_usd, total_input, total_output, api_calls);

                self.status_message = if messages.is_empty() {
                    Some("New session started.".into())
                } else {
                    Some(format!("Loaded ({} messages)", messages.len()))
                };
                // Reset the messages snapshot to the loaded history.
                self.messages_snapshot = messages;
                // Restore input history for up-arrow recall; reset navigation state.
                self.message_history = input_history;
                self.history_index = None;
                self.history_saved_text = String::new();
                tui.request_render(true); // full redraw — content replaced
            }
            AgentSessionEvent::AutoCompactionStart { reason } => {
                let label = match reason {
                    crate::core::CompactionReason::Overflow => "Compacting (context overflow)...",
                    crate::core::CompactionReason::Threshold => "Compacting context...",
                    crate::core::CompactionReason::Manual => "Compacting...",
                };
                self.status_message = Some(label.into());
                self.is_compacting = true;
                layout.footer.set_compacting(true);
                tui.request_render(false);
            }
            AgentSessionEvent::AutoCompactionEnd {
                summary,
                will_retry,
                messages,
            } => {
                layout.footer.set_compacting(false);
                self.is_compacting = false;
                if will_retry {
                    self.status_message = Some("Compacted. Retrying...".into());
                } else if summary.is_some() {
                    self.status_message = Some("Context compacted.".into());
                }

                // Rebuild the UI whenever compaction succeeded (messages non-empty)
                if !messages.is_empty() {
                    // Dump pre-compaction history to scrollback, then show recent tail in chat
                    let mut scrollback = String::new();
                    for msg in &messages {
                        match msg {
                            AgentMessage::User { content, .. } => {
                                let text: String = content
                                    .iter()
                                    .filter_map(|c| match c {
                                        ContentItem::Text { text } => Some(text.as_str()),
                                        _ => None,
                                    })
                                    .collect::<Vec<_>>()
                                    .join("");
                                scrollback.push_str(&format!("> {}\n\n", text));
                            }
                            AgentMessage::Assistant(a) => {
                                for block in &a.content {
                                    match block {
                                        ContentBlock::Thinking { thinking }
                                            if !thinking.is_empty() =>
                                        {
                                            for line in thinking.lines() {
                                                scrollback.push_str(&format!("│ {}\n", line));
                                            }
                                            scrollback.push('\n');
                                        }
                                        ContentBlock::Text { text } if !text.is_empty() => {
                                            scrollback.push_str(text);
                                            scrollback.push_str("\n\n");
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    if !scrollback.is_empty() {
                        tui.dump_scrollback(&scrollback);
                    }

                    layout.chat.clear();
                    let recent = if messages.len() > 6 {
                        &messages[messages.len() - 6..]
                    } else {
                        &messages
                    };
                    for msg in recent {
                        match msg {
                            AgentMessage::User { content, .. } => {
                                let text: String = content
                                    .iter()
                                    .filter_map(|c| match c {
                                        ContentItem::Text { text } => Some(text.as_str()),
                                        _ => None,
                                    })
                                    .collect::<Vec<_>>()
                                    .join("");
                                layout.chat.push_user(&text);
                            }
                            AgentMessage::Assistant(a) => {
                                for block in &a.content {
                                    match block {
                                        ContentBlock::Thinking { thinking }
                                            if !thinking.is_empty() =>
                                        {
                                            layout.chat.push_styled(
                                                theme::THINKING,
                                                &format!(
                                                    "│ {}",
                                                    thinking
                                                        .lines()
                                                        .collect::<Vec<_>>()
                                                        .join("\n│ ")
                                                ),
                                            );
                                        }
                                        ContentBlock::Text { text } if !text.is_empty() => {
                                            layout.chat.push_markdown_source(text);
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            AgentMessage::ToolResult {
                                content, is_error, ..
                            } => {
                                let text: String = content
                                    .iter()
                                    .filter_map(|c| match c {
                                        ContentItem::Text { text } => Some(text.as_str()),
                                        _ => None,
                                    })
                                    .collect::<Vec<_>>()
                                    .join("");
                                if !text.is_empty() {
                                    layout.chat.push_tool_result(&text, *is_error);
                                }
                            }
                            _ => {}
                        }
                    }

                    // Update footer context estimate from post-compaction messages.
                    // Preserve all running stats — reset_context() zeroes them.
                    let (prior_cost, prior_input, prior_output, prior_calls) =
                        layout.footer.snapshot_stats();
                    let context_tokens: usize = messages
                        .iter()
                        .map(crate::compaction::estimate_tokens)
                        .sum();
                    layout.footer.reset_context();
                    layout.footer.restore_stats(prior_cost, prior_input, prior_output, prior_calls);
                    layout.footer.set_context_used(context_tokens as u32);

                    tui.request_render(true); // full redraw — context replaced
                }
            }
            AgentSessionEvent::ProviderHealth { provider, online } => {
                layout.footer.set_provider_online(&provider, online);
            }
            _ => {}
        }
        None
    }

    fn handle_agent_event(
        &mut self,
        event: AgentEvent,
        layout: &mut AppLayout,
        tui: &mut tui::TUI,
    ) {
        match event {
            AgentEvent::AgentStart => {
                self.is_streaming = true;
                layout.chat.begin_stream();
                layout.statusbar.start_streaming();
                tui.request_render(false);
            }
            AgentEvent::AgentEnd { messages, system_prompt } => {
                // Replace the snapshot with the full history from the agent.
                // AgentEnd now carries agent.state.messages (everything), not just
                // new_messages from this turn, so we assign rather than extend.
                // Skip aborted/errored turns: the snapshot stays at the last good state.
                if crate::interactive::btw_overlay::turn_succeeded(&messages) {
                    self.messages_snapshot = messages;
                    self.system_prompt_snapshot = system_prompt;
                }
                self.is_streaming = false;
                layout.chat.cancel_stream();
                layout.statusbar.finish();
                if !self.pending_messages.is_empty() {
                    let msg = self.pending_messages.remove(0);
                    self.editing_queue_idx = None;
                    let _ = self.cmd_tx.send(SessionCommand::Prompt { text: msg });
                }
                layout
                    .statusbar
                    .set_queue(&self.pending_messages, self.editing_queue_idx);
                tui.request_render(false);
            }
            AgentEvent::MessageStart { message } => {
                if let AgentMessage::User { ref content, .. } = message {
                    let text = content
                        .iter()
                        .filter_map(|c| match c {
                            ContentItem::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("");
                    layout.chat.push_user(&text);
                    tui.request_render(false);
                }
            }
            AgentEvent::UsageUpdate { usage } => {
                layout.statusbar.set_input_tokens(usage.input);
                layout.footer.set_context_used(usage.input);
                layout.footer.record_api_call(usage.input);
            }
            AgentEvent::MessageUpdate { delta } => {
                match delta {
                    StreamDelta::Text(ref text) => layout.chat.append_text(text),
                    StreamDelta::Thinking(ref text) => layout.chat.append_thinking(text),
                    StreamDelta::ToolCallArgsStart { .. } => {}
                    StreamDelta::ToolCallArgsDelta { .. } => {}
                }
                layout
                    .statusbar
                    .set_output_tokens(layout.chat.streaming_len() as u32 / 4);
                tui.request_render(false);
            }
            AgentEvent::MessageEnd { message } => {
                let text = message.text_content();
                if !text.is_empty() {
                    self.last_response = Some(text.clone());
                }
                let thinking: Option<String> = message
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Thinking { thinking } if !thinking.is_empty() => {
                            Some(thinking.clone())
                        }
                        _ => None,
                    })
                    .next();
                layout.chat.finish_stream(&text, thinking.as_deref());

                // Output tokens: use API value if available, otherwise chars/4 heuristic (local models)
                let raw = message.usage.clone().unwrap_or_default();
                let output_tokens = if raw.output > 0 {
                    raw.output
                } else {
                    message
                        .content
                        .iter()
                        .map(|b| match b {
                            ContentBlock::Text { text } => {
                                crate::compaction::count_tokens(text) as u32
                            }
                            ContentBlock::Thinking { thinking } => {
                                crate::compaction::count_tokens(thinking) as u32
                            }
                            ContentBlock::ToolCall { arguments, .. } => {
                                crate::compaction::count_tokens(&arguments.to_string()) as u32
                            }
                        })
                        .sum()
                };
                // Only set output — input was already set by UsageUpdate
                layout.statusbar.set_output_tokens(output_tokens);
                if let Some(ref model) = self.current_model {
                    layout.footer.add_cost(&raw, &model.pricing);
                }
                tui.request_render(false);
            }
            AgentEvent::ToolExecutionStart { name, args, .. } => {
                layout.chat.push_tool_call(&name, &args);
                tui.request_render(false);
            }
            AgentEvent::ToolExecutionUpdate { .. } => {
                tui.request_render(false);
            }
            AgentEvent::ToolExecutionEnd { result, .. } => {
                let text = result.display.as_deref().unwrap_or(&result.content);
                layout.chat.push_tool_result(text, result.is_error);
                tui.request_render(false);
            }
            AgentEvent::TurnStart | AgentEvent::TurnEnd => {}
            AgentEvent::Retrying { attempt, wait_secs, reason: _ } => {
                self.status_message = Some(format!(
                    "Overloaded — retrying in {}s (attempt {}/3)…",
                    wait_secs, attempt
                ));
                tui.request_render(false);
            }
        }
    }

    pub fn handle_submit(&mut self, text: String) -> Option<PickerRequest> {
        if text.trim().is_empty() {
            return None;
        }

        // Reset history browse on submit
        self.history_index = None;

        // Record in history (avoid consecutive duplicates)
        if self.message_history.last().map(|s| s.as_str()) != Some(text.as_str()) {
            self.message_history.push(text.clone());
            // Persist the full history so it survives restarts and compactions.
            let _ = self.cmd_tx.try_send(SessionCommand::SaveInputHistory {
                history: self.message_history.clone(),
            });
        }

        if text.starts_with('/') {
            return self.handle_slash_command(&text);
        }

        // Expand inline skill references: `/skillname` tokens within the text
        // are replaced with the skill's content before sending to the LLM.
        let text = self.expand_inline_skills(text);

        // Bare "plan" enables plan mode
        if text.trim().eq_ignore_ascii_case("plan") && !self.plan_mode {
            self.plan_mode = true;
            let _ = self.cmd_tx.try_send(SessionCommand::SetPlanMode { enabled: true });
            return None;
        }

        // Fire onUserInput hooks (e.g. reset tmux window colour set by onResponseComplete).
        {
            let cfg = crate::core::config::NervConfig::load(crate::nerv_dir());
            crate::core::notifications::fire(
                crate::core::notifications::NotificationMatcher::OnUserInput,
                &cfg.notifications,
            );
        }

        if self.is_streaming {
            if let Some(idx) = self.editing_queue_idx {
                self.pending_messages[idx] = text;
                self.editing_queue_idx = None;
            } else {
                self.pending_messages.push(text);
            }
        } else {
            self.editing_queue_idx = None;
            let _ = self.cmd_tx.send(SessionCommand::Prompt { text });
        }
        None
    }

    pub fn handle_abort(&mut self) {
        // Set the cancel flag immediately so the running stream stops between chunks,
        // without waiting for the session thread to dequeue the Abort command.
        self.cancel_flag.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = self.cmd_tx.send(SessionCommand::Abort);
        // Immediately queue the top pending message behind the Abort so the
        // session thread starts on it as soon as the current turn is cancelled,
        // without waiting for AgentEnd to round-trip through the main loop.
        if !self.pending_messages.is_empty() {
            let msg = self.pending_messages.remove(0);
            self.editing_queue_idx = self.editing_queue_idx.and_then(|i| i.checked_sub(1));
            let _ = self.cmd_tx.send(SessionCommand::Prompt { text: msg });
        }
    }

    /// Replace `/skillname` tokens inside `text` with the matching skill's content.
    fn expand_inline_skills(&self, text: String) -> String {
        expand_inline_skills_impl(text, &self.skills)
    }

    fn handle_slash_command(&mut self, text: &str) -> Option<PickerRequest> {
        let parts: Vec<&str> = text.splitn(2, ' ').collect();
        let command = parts[0];
        let args = parts.get(1).copied().unwrap_or("").trim();

        match command {
            "/compact" => {
                // `/compact at 70` — set session threshold
                // `/compact on` / `/compact off` — toggle auto-compact
                // `/compact` (no args) — compact now
                if let Some(rest) = args.strip_prefix("at ") {
                    if let Ok(pct) = rest.trim().parse::<u8>() {
                        self.compact_threshold = pct;
                        // Write the shared atomic directly — takes effect immediately
                        // even if a stream is in progress (the session thread reads
                        // this before each auto-compact check).
                        self.compact_threshold_arc.store(pct as u32, Ordering::Relaxed);
                        // Also send through the channel so the session persists it to DB.
                        let _ = self
                            .cmd_tx
                            .send(SessionCommand::SetCompactThreshold { pct });
                    } else {
                        self.status_message =
                            Some("Usage: /compact at <1-100>".into());
                    }
                } else if args == "off" || args == "false" || args == "0" {
                    let _ = self.cmd_tx.send(SessionCommand::SetAutoCompact { enabled: false });
                } else if args == "on" || args == "true" || args == "1" {
                    let _ = self.cmd_tx.send(SessionCommand::SetAutoCompact { enabled: true });
                } else if args.is_empty() {
                    // Interrupt any running stream immediately so the session thread
                    // can pick up the Compact command without waiting for it to finish.
                    self.cancel_flag.store(true, std::sync::atomic::Ordering::Relaxed);
                    let _ = self.cmd_tx.send(SessionCommand::Abort);
                    let _ = self.cmd_tx.send(SessionCommand::Compact {
                        custom_instructions: None,
                    });
                } else {
                    self.status_message = Some(
                        "Usage: /compact [on|off|at <1-100>]".into(),
                    );
                }
            }
            "/model" => {
                if !args.is_empty() {
                    let found = if let Some((p, m)) = args.split_once('/') {
                        self.model_registry.get_model(p, m)
                    } else {
                        self.model_registry.find_model(args)
                    };

                    if let Some(m) = found {
                        let _ = self.cmd_tx.try_send(SessionCommand::SetModel {
                            provider: m.provider_name.clone(),
                            model_id: m.id.clone(),
                        });
                    } else {
                        self.status_message = Some(format!("Unknown model: {}", args));
                    }
                } else {
                    return Some(PickerRequest::ModelPicker);
                }
            }
            "/think" | "/thinking" => {
                // Toggle on/off, or accept "on"/"off" argument.
                let next = if args.is_empty() {
                    if self.current_thinking == ThinkingLevel::Off {
                        ThinkingLevel::On
                    } else {
                        ThinkingLevel::Off
                    }
                } else {
                    match args {
                        "on" | "true" | "1" => ThinkingLevel::On,
                        "off" | "false" | "0" => ThinkingLevel::Off,
                        _ => {
                            self.status_message = Some(
                                "Usage: /think [on|off]".into(),
                            );
                            return None;
                        }
                    }
                };
                let _ = self
                    .cmd_tx
                    .try_send(SessionCommand::SetThinkingLevel { level: next });
                self.current_thinking = next;
                let label = if next == ThinkingLevel::On {
                    "Thinking on"
                } else {
                    "Thinking off"
                };
                self.status_message = Some(label.into());
            }
            "/effort" => {
                // Set adaptive effort level (low/medium/high/max) or "off" to clear.
                let next: Option<EffortLevel> = if args.is_empty() {
                    // Cycle: off → low → medium → high → max → off
                    match self.current_effort {
                        None => Some(EffortLevel::Low),
                        Some(EffortLevel::Low) => Some(EffortLevel::Medium),
                        Some(EffortLevel::Medium) => Some(EffortLevel::High),
                        Some(EffortLevel::High) => Some(EffortLevel::Max),
                        Some(EffortLevel::Max) => None,
                    }
                } else {
                    match args {
                        "off" | "none" => None,
                        "low" => Some(EffortLevel::Low),
                        "medium" | "med" => Some(EffortLevel::Medium),
                        "high" => Some(EffortLevel::High),
                        "max" => Some(EffortLevel::Max),
                        _ => {
                            self.status_message = Some(
                                "Usage: /effort [off|low|medium|high|max]".into(),
                            );
                            return None;
                        }
                    }
                };
                let _ = self
                    .cmd_tx
                    .try_send(SessionCommand::SetEffortLevel { level: next });
                self.current_effort = next;
                let label = match next {
                    None => "Effort: off".into(),
                    Some(e) => format!("Effort: {:?}", e).to_lowercase().replace("some(", "").replace(")", ""),
                };
                self.status_message = Some(format!("Effort: {}", label));
            }
            "/btw" => {
                if args.is_empty() {
                    self.status_message = Some("Usage: /btw <note>".into());
                } else if let Some(model) = self.current_model.clone() {
                    let snap = self.messages_snapshot.clone();
                    return Some(PickerRequest::BtwOverlay {
                        messages: snap,
                        system_prompt: self.system_prompt_snapshot.clone(),
                        tools: self.tools_snapshot.clone(),
                        model,
                        note: args.to_string(),
                    });
                } else {
                    self.status_message = Some("/btw: no model configured".into());
                    self.status_is_error = true;
                }
            }
            "/plan" => {
                let enabled = !self.plan_mode;
                self.plan_mode = enabled;
                let _ = self.cmd_tx.try_send(SessionCommand::SetPlanMode { enabled });
            }
            "/hud" => {
                return Some(PickerRequest::ToggleHud);
            }
            "/session" => {
                // Open full-screen session picker.
                let _ = self.cmd_tx.send(SessionCommand::ListSessions {
                    repo_root: self.repo_root.clone(),
                    repo_id: self.repo_id.clone(),
                });
            }
            "/export" | "/share" => {
                let _ = self.cmd_tx.send(SessionCommand::Export);
                self.status_message = Some("Exporting...".into());
            }
            "/resume" => {
                if args.is_empty() {
                    // Open full-screen session picker.
                    let _ = self.cmd_tx.send(SessionCommand::ListSessions {
                        repo_root: self.repo_root.clone(),
                        repo_id: self.repo_id.clone(),
                    });
                } else {
                    let _ = self.cmd_tx.send(SessionCommand::LoadSession {
                        id: args.to_string(),
                    });
                }
            }
            "/wt" => {
                if args == "merge" {
                    let _ = self.cmd_tx.send(SessionCommand::MergeWorktree);
                } else if args.is_empty() {
                    self.status_message = Some("Usage: /wt <branch-name> | /wt merge".into());
                } else {
                    let nerv_dir = crate::nerv_dir().to_path_buf();
                    let _ = self.cmd_tx.send(SessionCommand::CreateWorktree {
                        branch_name: args.to_string(),
                        nerv_dir,
                    });
                }
            }
            "/tree" => {
                if self.session_id.is_none() {
                    self.status_message = Some("No active session.".into());
                } else {
                    let _ = self.cmd_tx.send(SessionCommand::GetTree);
                }
            }
            "/login" => {
                let provider = if args.is_empty() { "anthropic" } else { args };
                self.status_message = Some(format!("Starting {} login...", provider));
                let _ = self.cmd_tx.send(SessionCommand::Login {
                    provider: provider.to_string(),
                });
            }
            "/logout" => {
                let provider = if args.is_empty() { "anthropic" } else { args };
                let nerv_dir = crate::nerv_dir();
                let mut auth = crate::core::auth::AuthStorage::load(nerv_dir);
                auth.remove(provider);
                self.status_message = Some(format!("Logged out from {}.", provider));
            }
            "/fork" => {
                let _ = self.cmd_tx.send(SessionCommand::ForkSession);
            }
            "/new" => {
                let _ = self.cmd_tx.send(SessionCommand::NewSession);
            }
            "/copy" => {
                if let Some(ref text) = self.last_response.clone() {
                    match copy_to_clipboard(text) {
                        Ok(()) => {
                            self.status_message = Some("Copied to clipboard.".into());
                        }
                        Err(e) => {
                            self.status_message = Some(format!("Copy failed: {}", e));
                            self.status_is_error = true;
                        }
                    }
                } else {
                    self.status_message = Some("Nothing to copy yet.".into());
                }
            }
            "/quit" | "/exit" | "/q" => {
                // Signal quit — handled by setting a flag the main loop checks
                self.quit_requested = true;
            }
            "/help" | "/?" => {
                let mut help = String::from(
                    "Commands:\n\
                     /model          — list/switch models\n\
                     /think [on|off] — toggle extended thinking (Shift+Tab to cycle)\n\
                     /effort [low|medium|high|max] — set adaptive effort level (^E to cycle)\n\
                     /login [provider] — OAuth login (default: anthropic)\n\
                     /logout [provider] — remove stored credentials\n\
                     /compact        — compact context now\n\
                     /compact on|off — toggle auto-compact for this session\n\
                     /compact at N   — set auto-compact threshold to N% for this session\n\
                     /session        — browse and resume sessions\n\
                     /export          — export session to ~/.nerv/exports/ (html + jsonl)\n\
                     /copy           — copy last response to clipboard\n\
                     /resume [id]    — list/load sessions\n\
                     /tree           — browse/switch session branches\n\
                     /wt <branch>    — create git worktree for session\n\
                     /wt merge       — merge worktree back and clean up\n\
                     /btw <note>     — add background context; model acknowledges briefly\n\
                     /plan           — toggle plan mode (read-only research)\n\
                     /hud            — toggle nervHud (RSS/CPU stats)\n\
                     /fork           — fork session into a new independent copy\n\
                     /new            — start new session\n\
                     /quit           — quit nerv\n\
                     /help           — this message",
                );
                if !self.skills.is_empty() {
                    help.push_str("\n\nSkills:");
                    for skill in &self.skills {
                        help.push_str(&format!("\n /{}  — {}", skill.name, skill.description));
                    }
                }
                help.push_str("\n\nKeys: Enter=send  Shift/Ctrl+Enter=newline  Shift+Tab=think  ^S=tree  Esc/^C=quit  ^G=$EDITOR");
                self.status_message = Some(help);
            }
            _ => {
                // Check for skill commands: /skill:name or /name (if matches a skill)
                let skill_name = command
                    .strip_prefix("/skill:")
                    .or_else(|| command.strip_prefix("/"));

                if let Some(name) = skill_name
                    && let Some(skill) = self.skills.iter().find(|s| s.name == name)
                {
                    // Send skill content + any args as a prompt
                    let prompt = if args.is_empty() {
                        skill.content.clone()
                    } else {
                        format!("{}\n\n{}", skill.content, args)
                    };
                    let _ = self.cmd_tx.send(SessionCommand::Prompt { text: prompt });
                    return None;
                }

                self.status_message = Some(format!("Unknown command: {}. Try /help", command));
            }
        }
        None
    }

    /// Navigate up through message history. Returns text for editor, or None.
    pub fn history_up(&mut self, current_text: &str) -> Option<String> {
        if self.message_history.is_empty() {
            return None;
        }
        match self.history_index {
            None => {
                // Start browsing — save current text
                self.history_saved_text = current_text.to_string();
                self.history_index = Some(self.message_history.len() - 1);
                Some(self.message_history.last()?.clone())
            }
            Some(idx) if idx > 0 => {
                self.history_index = Some(idx - 1);
                Some(self.message_history[idx - 1].clone())
            }
            _ => None, // already at oldest
        }
    }

    /// Navigate down through message history. Returns text for editor, or None.
    pub fn history_down(&mut self) -> Option<String> {
        let idx = self.history_index?;
        if idx + 1 < self.message_history.len() {
            self.history_index = Some(idx + 1);
            Some(self.message_history[idx + 1].clone())
        } else {
            // Past newest — restore saved text
            self.history_index = None;
            Some(std::mem::take(&mut self.history_saved_text))
        }
    }

    pub fn repo_root(&self) -> Option<String> {
        self.repo_root.clone()
    }

    pub fn repo_id(&self) -> Option<String> {
        self.repo_id.clone()
    }

    pub fn slash_completions(&self) -> Vec<String> {
        let mut cmds = vec![
            "/model".into(),
            "/think".into(),
            "/compact".into(),
            "/compact at ".into(),
            "/session".into(),
            "/copy".into(),
            "/export".into(),
            "/resume".into(),
            "/tree".into(),
            "/plan".into(),
            "/hud".into(),
            "/btw".into(),
            "/fork".into(),
            "/wt".into(),
            "/login".into(),
            "/logout".into(),
            "/new".into(),
            "/quit".into(),
            "/help".into(),
        ];
        for skill in &self.skills {
            cmds.push(format!("/{}", skill.name));
        }
        cmds
    }

    pub fn cmd_tx(&self) -> &channel::Sender<SessionCommand> {
        &self.cmd_tx
    }

    pub fn model_registry(&self) -> &Arc<ModelRegistry> {
        &self.model_registry
    }

    pub fn model_name(&self) -> &str {
        self.current_model
            .as_ref()
            .map(|m| m.name.as_str())
            .unwrap_or("no model")
    }

    /// Start editing a queued message — load it into the editor.
    /// Returns the text to put in the editor, or None.
    pub fn edit_queue_up(&mut self) -> Option<String> {
        if self.pending_messages.is_empty() {
            return None;
        }
        let idx = match self.editing_queue_idx {
            Some(i) if i > 0 => i - 1,
            Some(_) => return None,                  // already at first
            None => self.pending_messages.len() - 1, // start from last
        };
        self.editing_queue_idx = Some(idx);
        Some(self.pending_messages[idx].clone())
    }

    /// Move down in the queue. Returns text for editor, or None to exit queue editing.
    pub fn edit_queue_down(&mut self) -> Option<String> {
        let idx = self.editing_queue_idx?;
        if idx + 1 < self.pending_messages.len() {
            self.editing_queue_idx = Some(idx + 1);
            Some(self.pending_messages[idx + 1].clone())
        } else {
            // Past the end — exit queue editing, clear editor
            self.editing_queue_idx = None;
            Some(String::new())
        }
    }

    /// Remove the currently-edited queued message.
    pub fn remove_editing_queue_item(&mut self) {
        if let Some(idx) = self.editing_queue_idx {
            if idx < self.pending_messages.len() {
                self.pending_messages.remove(idx);
            }
            self.editing_queue_idx = None;
        }
    }

    pub fn toggle_plan_mode(&mut self) -> bool {
        let enabled = !self.plan_mode;
        self.plan_mode = enabled;
        let _ = self
            .cmd_tx
            .try_send(SessionCommand::SetPlanMode { enabled });
        enabled
    }

    pub fn current_thinking(&self) -> ThinkingLevel {
        self.current_thinking
    }

    /// Toggle thinking on/off (Shift+Tab)
    pub fn cycle_thinking(&mut self) -> ThinkingLevel {
        self.current_thinking = if self.current_thinking == ThinkingLevel::Off {
            ThinkingLevel::On
        } else {
            ThinkingLevel::Off
        };
        self.current_thinking
    }

    /// Cycle effort level: off → low → medium → high → max → off (^E)
    pub fn cycle_effort(&mut self) -> Option<EffortLevel> {
        self.current_effort = match self.current_effort {
            None => Some(EffortLevel::Low),
            Some(EffortLevel::Low) => Some(EffortLevel::Medium),
            Some(EffortLevel::Medium) => Some(EffortLevel::High),
            Some(EffortLevel::High) => Some(EffortLevel::Max),
            Some(EffortLevel::Max) => None,
        };
        self.current_effort
    }

    pub fn current_model(&self) -> Option<&Model> {
        self.current_model.as_ref()
    }

    /// Push all locally-tracked state to the footer in one shot.
    /// Call this after any slash command or key binding that changes settings,
    /// instead of scattering individual set_* calls.
    pub fn refresh_footer(&self, footer: &mut super::footer::FooterComponent) {
        if let Some(m) = &self.current_model {
            footer.set_model(m);
        }
        footer.set_thinking(self.current_thinking);
        footer.set_effort(self.current_effort);
        footer.set_plan_mode(self.plan_mode);
        footer.set_compact_threshold(self.compact_threshold);
    }
}

/// Replace `/skillname` tokens inside `text` with the matching skill's content.
/// Tokens are only expanded when preceded by whitespace (or at start of string)
/// and followed by whitespace, punctuation, or end-of-string — so URLs and paths
/// like `/usr/bin` are left untouched.
fn expand_inline_skills_impl(text: String, skills: &[crate::core::skills::Skill]) -> String {
    if skills.is_empty() || !text.contains('/') {
        return text;
    }
    let mut result = String::with_capacity(text.len());
    let mut chars = text.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        if ch == '/' && (i == 0 || text[..i].ends_with(|c: char| c.is_whitespace())) {
            // Collect the word after the slash
            let rest = &text[i + 1..];
            let word_end = rest
                .find(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
                .unwrap_or(rest.len());
            let word = &rest[..word_end];
            let after = &rest[word_end..];
            let followed_by_boundary = after.is_empty()
                || after.starts_with(|c: char| {
                    // A following '/' means this is a path component (e.g. /usr/bin), not a skill
                    c != '/' && (c.is_whitespace() || c.is_ascii_punctuation())
                });

            if followed_by_boundary {
                if let Some(skill) = skills.iter().find(|s| s.name == word) {
                    result.push_str(&skill.content);
                    // Advance the char iterator past the word we consumed
                    for _ in 0..word.chars().count() {
                        chars.next();
                    }
                    continue;
                }
            }
        }
        result.push(ch);
    }
    result
}

fn count_tree_nodes(tree: &[crate::session::types::SessionTreeNode]) -> usize {
    tree.iter()
        .map(|n| 1 + count_tree_nodes(&n.children))
        .sum()
}

/// Copy `text` to the system clipboard.
///
/// Tries, in order: `pbcopy` (macOS), `wl-copy` (Wayland), `xclip`, `xsel`.
fn copy_to_clipboard(text: &str) -> Result<(), String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    // On macOS pbcopy is always present; on Linux try Wayland then X11.
    #[cfg(target_os = "macos")]
    let candidates: &[&[&str]] = &[&["pbcopy"]];

    #[cfg(target_os = "linux")]
    let candidates: &[&[&str]] = &[
        &["wl-copy"],
        &["xclip", "-selection", "clipboard"],
        &["xsel", "--clipboard", "--input"],
    ];

    for argv in candidates {
        let (prog, args) = argv.split_first().unwrap();
        let Ok(mut child) = Command::new(prog)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        match child.wait() {
            Ok(status) if status.success() => return Ok(()),
            _ => continue,
        }
    }

    #[cfg(target_os = "macos")]
    return Err("pbcopy failed".into());

    #[cfg(target_os = "linux")]
    return Err("no clipboard utility found (wl-copy / xclip / xsel)".into());

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    return Err("clipboard not supported on this platform".into());
}

#[cfg(test)]
mod tests {
    use super::expand_inline_skills_impl;
    use crate::core::skills::Skill;
    use std::path::PathBuf;

    fn skill(name: &str, content: &str) -> Skill {
        Skill {
            name: name.to_string(),
            description: String::new(),
            file_path: PathBuf::new(),
            content: content.to_string(),
        }
    }

    #[test]
    fn expands_inline_skill() {
        let skills = vec![skill("commit", "git commit -v")];
        let out = expand_inline_skills_impl("update docs and /commit".to_string(), &skills);
        assert_eq!(out, "update docs and git commit -v");
    }

    #[test]
    fn expands_at_start() {
        let skills = vec![skill("commit", "git commit -v")];
        let out = expand_inline_skills_impl("/commit the changes".to_string(), &skills);
        assert_eq!(out, "git commit -v the changes");
    }

    #[test]
    fn does_not_expand_url_path() {
        let skills = vec![skill("usr", "REPLACED")];
        let out = expand_inline_skills_impl("see /usr/bin/foo".to_string(), &skills);
        assert_eq!(out, "see /usr/bin/foo");
    }

    #[test]
    fn does_not_expand_unknown_skill() {
        let skills = vec![skill("commit", "git commit -v")];
        let out = expand_inline_skills_impl("run /deploy please".to_string(), &skills);
        assert_eq!(out, "run /deploy please");
    }

    #[test]
    fn expands_multiple_skills() {
        let skills = vec![skill("foo", "FOO"), skill("bar", "BAR")];
        let out = expand_inline_skills_impl("/foo and /bar".to_string(), &skills);
        assert_eq!(out, "FOO and BAR");
    }

    #[test]
    fn no_skills_returns_unchanged() {
        let out = expand_inline_skills_impl("run /deploy".to_string(), &[]);
        assert_eq!(out, "run /deploy");
    }

    #[test]
    fn expands_before_punctuation() {
        let skills = vec![skill("commit", "git commit -v")];
        let out = expand_inline_skills_impl("done? /commit.".to_string(), &skills);
        assert_eq!(out, "done? git commit -v.");
    }
}
