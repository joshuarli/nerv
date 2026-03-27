use crossbeam_channel as channel;
use std::sync::Arc;

use super::layout::AppLayout;
use super::session_picker::SessionPicker;
use super::theme;
use super::tree_selector::TreeSelector;
use crate::agent::types::*;
use crate::core::model_registry::ModelRegistry;
use crate::core::*;
use crate::tui;

pub struct InteractiveMode {
    cmd_tx: channel::Sender<SessionCommand>,
    pub is_streaming: bool,
    current_model: Option<Model>,
    current_thinking: ThinkingLevel,
    session_cost: Cost,
    model_registry: Arc<ModelRegistry>,
    skills: Vec<crate::core::skills::Skill>,
    repo_root: Option<String>,
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
    /// Session picker state (when /resume is active).
    pub session_picker: Option<SessionPicker>,
    /// Tree selector state (when /tree is active).
    pub tree_selector: Option<TreeSelector>,
    /// Saved editor text when entering history browse.
    history_saved_text: String,
    /// Pending permission request — waiting for y/n from user.
    pub pending_permission: Option<crossbeam_channel::Sender<bool>>,
    /// Current permission request details (tool, args) to record if accepted
    pub pending_permission_details: Option<(String, serde_json::Value)>,
    /// Plan mode: read-only research mode, no file mutations.
    pub plan_mode: bool,
}

impl InteractiveMode {
    pub fn new(
        cmd_tx: channel::Sender<SessionCommand>,
        model_registry: Arc<ModelRegistry>,
        initial_model: Option<Model>,
        skills: Vec<crate::core::skills::Skill>,
        repo_root: Option<String>,
    ) -> Self {
        Self {
            cmd_tx,
            is_streaming: false,
            current_model: initial_model,
            current_thinking: ThinkingLevel::Off,
            session_cost: Cost::default(),
            model_registry,
            skills,
            repo_root,
            session_id: None,
            status_message: None,
            status_is_error: false,
            quit_requested: false,
            session_picker: None,
            tree_selector: None,
            last_response: None,
            pending_messages: Vec::new(),
            editing_queue_idx: None,
            message_history: Vec::new(),
            history_index: None,
            history_saved_text: String::new(),
            pending_permission: None,
            pending_permission_details: None,
            plan_mode: false,
        }
    }

    pub fn handle_event(
        &mut self,
        event: AgentSessionEvent,
        layout: &mut AppLayout,
        tui: &mut tui::TUI,
    ) {
        match event {
            AgentSessionEvent::Agent(agent_event) => {
                self.handle_agent_event(agent_event, layout, tui);
            }
            AgentSessionEvent::ModelChanged { model } => {
                layout.footer.set_model(&model);
                self.current_model = Some(model);
            }
            AgentSessionEvent::ThinkingLevelChanged { level } => {
                self.current_thinking = level;
                layout.footer.set_thinking(level);
            }
            AgentSessionEvent::PlanModeChanged { enabled } => {
                self.plan_mode = enabled;
                layout.footer.set_plan_mode(enabled);
                let label = if enabled { "Plan mode on" } else { "Plan mode off" };
                self.status_message = Some(label.into());
            }
            AgentSessionEvent::SessionNamed { name } => {
                layout.footer.set_session_name(Some(name));
            }
            AgentSessionEvent::Status { message, is_error } => {
                self.status_message = Some(message);
                self.status_is_error = is_error;
            }
            AgentSessionEvent::SessionList { sessions } => {
                self.session_picker = Some(SessionPicker::new(sessions));
            }
            AgentSessionEvent::SearchResults { results } => {
                if let Some(ref mut picker) = self.session_picker {
                    picker.update_results(results);
                }
            }
            AgentSessionEvent::TreeData { tree, current_leaf } => {
                // Check if tree has any branch points worth showing
                let total_nodes = count_tree_nodes(&tree);
                if total_nodes == 0 {
                    self.status_message = Some("No entries in session.".into());
                } else {
                    self.tree_selector = Some(TreeSelector::new(tree, current_leaf));
                }
            }
            AgentSessionEvent::ExportDone { result } => match result {
                Ok(path) => {
                    self.status_message = Some(format!("Exported to {}", path));
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
                self.status_message = Some(format!(
                    "⚠ Permission: {} ({})\n  y = allow, n = deny",
                    tool, reason
                ));
                self.status_is_error = true;
                self.pending_permission = Some(response_tx);
                self.pending_permission_details = Some((tool.clone(), args.clone()));
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
            AgentSessionEvent::SessionStarted { id } => {
                self.session_id = Some(id);
            }
            AgentSessionEvent::SessionLoaded { messages } => {
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
                                    let preview =
                                        if text.len() > 200 { &text[..200] } else { text };
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

                self.status_message = Some(format!("Loaded ({} messages)", messages.len()));
                tui.request_render(true); // full redraw — content replaced
            }
            AgentSessionEvent::AutoCompactionStart { reason } => {
                let label = match reason {
                    crate::core::CompactionReason::Overflow => "Compacting (context overflow)...",
                    crate::core::CompactionReason::Threshold => "Compacting context...",
                    crate::core::CompactionReason::Manual => "Compacting...",
                };
                self.status_message = Some(label.into());
            }
            AgentSessionEvent::AutoCompactionEnd {
                summary,
                will_retry,
            } => {
                if will_retry {
                    self.status_message = Some("Compacted. Retrying...".into());
                } else if summary.is_some() {
                    self.status_message = Some("Context compacted.".into());
                }
                let _ = summary; // summary available if needed for display
            }
            AgentSessionEvent::ProviderHealth { provider, online } => {
                layout.footer.set_provider_online(&provider, online);
            }
            _ => {}
        }
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
            AgentEvent::AgentEnd { .. } => {
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

                // Output tokens: use API value if available, otherwise tiktoken
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
        }
    }

    pub fn handle_submit(&mut self, text: String) {
        if text.trim().is_empty() {
            return;
        }

        // Reset history browse on submit
        self.history_index = None;

        if text.starts_with('/') {
            self.handle_slash_command(&text);
            return;
        }

        // Bare "plan" enables plan mode
        if text.trim().eq_ignore_ascii_case("plan") && !self.plan_mode {
            self.plan_mode = true;
            let _ = self.cmd_tx.try_send(SessionCommand::SetPlanMode { enabled: true });
            return;
        }

        // Record in history (avoid consecutive duplicates)
        if self.message_history.last().map(|s| s.as_str()) != Some(text.as_str()) {
            self.message_history.push(text.clone());
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
    }

    pub fn handle_abort(&self) {
        let _ = self.cmd_tx.send(SessionCommand::Abort);
    }

    fn handle_slash_command(&mut self, text: &str) {
        let parts: Vec<&str> = text.splitn(2, ' ').collect();
        let command = parts[0];
        let args = parts.get(1).copied().unwrap_or("").trim();

        match command {
            "/compact" => {
                let _ = self.cmd_tx.send(SessionCommand::Compact {
                    custom_instructions: None,
                });
                self.status_message = Some("Compacting...".into());
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
                    let models = self.model_registry.available_models();
                    if models.is_empty() {
                        self.status_message = Some(
                            "No models available.\n\
                             /login            — login to Anthropic (Claude)\n\
                             ANTHROPIC_API_KEY — set env var for API key auth\n\
                             Configure a custom provider in ~/.nerv/config.jsonc"
                                .into(),
                        );
                    } else {
                        let current = self.model_name();
                        let mut lines = vec![format!("Current: {}", current), String::new()];
                        let mut last_provider = String::new();
                        for m in &models {
                            if m.provider_name != last_provider {
                                lines.push(format!("  [{}]", m.provider_name));
                                last_provider = m.provider_name.clone();
                            }
                            let marker = if m.name == current { " *" } else { "" };
                            lines.push(format!("    {} ({}){}", m.name, m.id, marker));
                        }
                        lines.push(String::new());
                        lines.push("/model <id> — switch model".into());
                        self.status_message = Some(lines.join("\n"));
                    }
                }
            }
            "/think" | "/thinking" => {
                let next = if args.is_empty() {
                    // Cycle
                    match self.current_thinking {
                        ThinkingLevel::Off => ThinkingLevel::Low,
                        ThinkingLevel::Minimal => ThinkingLevel::Low,
                        ThinkingLevel::Low => ThinkingLevel::Medium,
                        ThinkingLevel::Medium => ThinkingLevel::High,
                        ThinkingLevel::High => ThinkingLevel::Off,
                        ThinkingLevel::Xhigh => ThinkingLevel::Off,
                    }
                } else {
                    match args {
                        "off" => ThinkingLevel::Off,
                        "minimal" | "min" => ThinkingLevel::Minimal,
                        "low" => ThinkingLevel::Low,
                        "medium" | "med" => ThinkingLevel::Medium,
                        "high" => ThinkingLevel::High,
                        "xhigh" | "max" => ThinkingLevel::Xhigh,
                        _ => {
                            self.status_message = Some(format!(
                                "Unknown level: {}. Options: off, low, medium, high, xhigh",
                                args
                            ));
                            return;
                        }
                    }
                };
                let _ = self
                    .cmd_tx
                    .try_send(SessionCommand::SetThinkingLevel { level: next });
                self.current_thinking = next;
                self.status_message = Some(format!("Thinking: {:?}", next));
            }
            "/plan" => {
                let enabled = !self.plan_mode;
                self.plan_mode = enabled;
                let _ = self.cmd_tx.try_send(SessionCommand::SetPlanMode { enabled });
            }
            "/session" => {
                self.status_message = Some(format!(
                    "Model: {} | Thinking: {:?} | Cost: ${:.4}",
                    self.model_name(),
                    self.current_thinking,
                    self.session_cost.total,
                ));
            }
            "/export" | "/share" => {
                if args.is_empty() {
                    self.status_message =
                        Some("Usage: /export path.jsonl or /export path.html".into());
                } else {
                    let path = std::path::PathBuf::from(args);
                    if args.ends_with(".html") {
                        let _ = self.cmd_tx.send(SessionCommand::ExportHtml { path });
                        self.status_message = Some(format!("Exporting HTML to {}...", args));
                    } else {
                        let _ = self.cmd_tx.send(SessionCommand::ExportJsonl { path });
                        self.status_message = Some(format!("Exporting JSONL to {}...", args));
                    }
                }
            }
            "/resume" => {
                if args.is_empty() {
                    let _ = self.cmd_tx.send(SessionCommand::ListSessions {
                        repo_root: self.repo_root.clone(),
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
            "/new" => {
                let _ = self.cmd_tx.send(SessionCommand::NewSession);
                self.session_id = None;
                self.status_message = Some("New session started.".into());
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
                     /think [level]  — set thinking (off/low/medium/high/xhigh)\n\
                     /login [provider] — OAuth login (default: anthropic)\n\
                     /logout [provider] — remove stored credentials\n\
                     /compact        — compact context\n\
                     /session        — show session info\n\
                     /export <path>  — export to .jsonl or .html\n\
                     /copy           — copy last response to clipboard\n\
                     /resume [id]    — list/load sessions\n\
                     /tree           — browse/switch session branches\n\
                     /wt <branch>    — create git worktree for session\n\
                     /wt merge       — merge worktree back and clean up\n\
                     /plan           — toggle plan mode (read-only research)\n\
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
                help.push_str("\n\nKeys: Enter=send  Shift/Ctrl+Enter=newline  Shift+Tab=plan  Esc/^C=quit  ^G=$EDITOR");
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
                    return;
                }

                self.status_message = Some(format!("Unknown command: {}. Try /help", command));
            }
        }
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

    pub fn slash_completions(&self) -> Vec<String> {
        let mut cmds = vec![
            "/model".into(),
            "/think".into(),
            "/compact".into(),
            "/session".into(),
            "/copy".into(),
            "/export".into(),
            "/resume".into(),
            "/tree".into(),
            "/plan".into(),
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

    /// Cycle through thinking levels: Off → Low → Medium → High → Off
    pub fn cycle_thinking(&mut self) -> ThinkingLevel {
        self.current_thinking = match self.current_thinking {
            ThinkingLevel::Off => ThinkingLevel::Low,
            ThinkingLevel::Minimal => ThinkingLevel::Low,
            ThinkingLevel::Low => ThinkingLevel::Medium,
            ThinkingLevel::Medium => ThinkingLevel::High,
            ThinkingLevel::High => ThinkingLevel::Off,
            ThinkingLevel::Xhigh => ThinkingLevel::Off,
        };
        self.current_thinking
    }

    pub fn current_model(&self) -> Option<&Model> {
        self.current_model.as_ref()
    }
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
