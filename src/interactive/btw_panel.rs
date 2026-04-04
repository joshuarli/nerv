/// Inline `/btw` panel — renders a bordered box above the editor with a
/// streamed answer from a background Agent call.  The main agent continues
/// running unaffected.  Dismissed by Esc or Enter.
use std::sync::{Arc, RwLock};

use crossbeam_channel as channel;

use crate::agent::agent::Agent;
use crate::agent::provider::{CancelFlag, ProviderRegistry, new_cancel_flag};
use crate::agent::types::{AgentEvent, AgentMessage, ContentItem, Model, StopReason, StreamDelta};
use crate::interactive::btw_overlay::wrap_text;
use crate::interactive::display::{fmt_cost, fmt_tokens};
use crate::interactive::theme;
use crate::tui::tui::Component;

pub enum BtwChunk {
    Text(String),
    Error(String),
    Usage(crate::agent::types::Usage),
    Done,
}

/// Maximum lines of content shown in the panel.
const MAX_CONTENT_LINES: usize = 12;

pub struct BtwPanel {
    usage: crate::agent::types::Usage,
    /// The original question shown in the panel header.
    pub note: String,
    /// Accumulated response text.
    response: String,
    /// Whether the stream has finished (or errored).
    pub done: bool,
    /// Error text, if the API call failed.
    error: Option<String>,
    /// Receives text chunks from the background thread.
    pub rx: channel::Receiver<BtwChunk>,
    /// Used to cancel the background stream if the panel is dismissed early.
    cancel: CancelFlag,
    /// Accumulated cost for this btw call.
    cost: crate::agent::types::Cost,
    /// Pricing table for the model used (needed to compute cost from Usage).
    pricing: crate::agent::types::ModelPricing,
    /// How many wrapped lines to scroll up from the bottom (0 = tail).
    pub scroll_offset: usize,
}

impl BtwPanel {
    pub fn new(
        note: String,
        rx: channel::Receiver<BtwChunk>,
        cancel: CancelFlag,
        pricing: crate::agent::types::ModelPricing,
    ) -> Self {
        Self {
            note,
            response: String::new(),
            done: false,
            error: None,
            rx,
            cancel,
            cost: crate::agent::types::Cost::default(),
            usage: crate::agent::types::Usage {
                input: 0,
                output: 0,
                cache_read: 0,
                cache_write: 0,
            },
            pricing,
            scroll_offset: 0,
        }
    }

    /// Drain any pending chunks from the background thread.  Returns true if
    /// any new text arrived (caller should request a render).
    pub fn drain(&mut self) -> bool {
        let mut changed = false;
        loop {
            match self.rx.try_recv() {
                Ok(BtwChunk::Text(t)) => {
                    self.response.push_str(&t);
                    changed = true;
                }
                Ok(BtwChunk::Error(e)) => {
                    self.error = Some(e);
                    self.done = true;
                    changed = true;
                }
                Ok(BtwChunk::Usage(u)) => {
                    self.usage.input = self.usage.input.saturating_add(u.input);
                    self.usage.output = self.usage.output.saturating_add(u.output);
                    self.usage.cache_read = self.usage.cache_read.saturating_add(u.cache_read);
                    self.usage.cache_write = self.usage.cache_write.saturating_add(u.cache_write);
                    self.cost.add_usage(&u, &self.pricing);
                    changed = true;
                }
                Ok(BtwChunk::Done) => {
                    self.done = true;
                    changed = true;
                }
                Err(_) => break,
            }
        }
        changed
    }

    /// Signal the background thread to stop (called on dismissal).
    pub fn cancel(&self) {
        self.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// The accumulated response text (for copying / saving).
    pub fn response(&self) -> &str {
        &self.response
    }

    /// Scroll up by `n` wrapped lines.
    pub fn scroll_up(&mut self, n: usize, width: usize) {
        let inner = width.saturating_sub(4);
        let total = self.all_content_lines(inner).len();
        let visible = MAX_CONTENT_LINES;
        let max_offset = total.saturating_sub(visible);
        self.scroll_offset = (self.scroll_offset + n).min(max_offset);
    }

    /// Scroll down by `n` wrapped lines.
    pub fn scroll_down(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
    }

    /// Number of terminal lines this panel will occupy when rendered.
    pub fn line_count(&self, width: usize) -> usize {
        let inner = width.saturating_sub(4); // "│ " + " │"
        let content = self.content_lines(inner);
        // top border + content rows + bottom border
        1 + content.len().max(1) + 1
    }

    fn all_content_lines(&self, inner_width: usize) -> Vec<String> {
        if let Some(err) = &self.error {
            return wrap_text(&format!("error: {}", err), inner_width);
        }
        if self.response.is_empty() {
            if self.done {
                return vec!["(no response)".into()];
            }
            return vec!["…".into()];
        }
        wrap_text(&self.response, inner_width)
    }

    fn content_lines(&self, inner_width: usize) -> Vec<String> {
        let all = self.all_content_lines(inner_width);
        let total = all.len();
        if total <= MAX_CONTENT_LINES {
            return all;
        }
        // scroll_offset=0 → tail (most recent); higher → further up
        let end = total.saturating_sub(self.scroll_offset);
        let start = end.saturating_sub(MAX_CONTENT_LINES);
        all[start..end].to_vec()
    }
}

impl Component for BtwPanel {
    fn render(&self, width: u16) -> Vec<String> {
        let w = width as usize;
        let inner = w.saturating_sub(4);
        let content = self.content_lines(inner);

        let mut lines = Vec::new();

        // ── top border ────────────────────────────────────────────────────
        let label = " btw ";
        let dash_total = w.saturating_sub(label.len() + 2); // 2 for "╭" + "╮"
        let left = dash_total / 2;
        let right = dash_total - left;
        lines.push(format!(
            "{}╭{}{}{}╮{}",
            theme::ACCENT,
            "─".repeat(left),
            label,
            "─".repeat(right),
            theme::RESET,
        ));

        // ── content rows ──────────────────────────────────────────────────
        for row in &content {
            // Pad to fill the inner width so the right border is flush.
            let visible_len = visible_char_count(row);
            let padding = inner.saturating_sub(visible_len);
            lines.push(format!(
                "{}│{} {}{}{} {}│{}",
                theme::ACCENT,
                theme::RESET,
                row,
                " ".repeat(padding),
                theme::ACCENT,
                // extra space on the right side of content
                "",
                theme::RESET,
            ));
        }

        // ── bottom border ─────────────────────────────────────────────────
        let stats_str = if self.usage.input > 0 || self.usage.output > 0 {
            let mut s = String::from(" ");
            if self.usage.cache_read > 0 {
                s.push_str(&format!("Rc{} ", fmt_tokens(self.usage.cache_read as u64)));
            }
            if self.usage.cache_write > 0 {
                s.push_str(&format!("Wc{} ", fmt_tokens(self.usage.cache_write as u64)));
            }
            s.push_str(&format!(
                "in{} out{}",
                fmt_tokens(self.usage.input as u64),
                fmt_tokens(self.usage.output as u64)
            ));
            if self.cost.total > 0.0 {
                s.push_str(&format!(" ${}", fmt_cost(self.cost.total)));
            }
            s.push(' ');
            s
        } else {
            String::new()
        };
        let dismiss = if self.done { " ↵ dismiss " } else { "" };
        let copy_hint = if self.done && !self.response.is_empty() { " c copy " } else { "" };
        // Show scroll hint when there is more content than fits.
        let inner_w = w.saturating_sub(4);
        let total_lines = self.all_content_lines(inner_w).len();
        let scroll_hint = if total_lines > MAX_CONTENT_LINES { " ↑↓ scroll " } else { "" };
        // stats on the left of the dashes, hints on the right
        let fixed =
            stats_str.chars().count() + dismiss.len() + copy_hint.len() + scroll_hint.len() + 2; // 2 for ╰ ╯
        let bottom_dashes = w.saturating_sub(fixed);
        lines.push(format!(
            "{}╰{}{}{}{}{}╯{}",
            theme::ACCENT,
            stats_str,
            "─".repeat(bottom_dashes),
            scroll_hint,
            copy_hint,
            dismiss,
            theme::RESET,
        ));

        lines
    }
}

// ─────────────────────────────── helpers ─────────────────────────────────────

/// Count visible characters (ASCII only; good enough for prose responses that
/// don't contain ANSI escapes in the response text itself).
fn visible_char_count(s: &str) -> usize {
    s.chars().count()
}

// ─────────────────────────────── spawn ───────────────────────────────────────

/// Spawn the btw agent on a background thread and return a `BtwPanel` ready to
/// be attached to the layout.
pub fn spawn_btw(
    messages: Vec<AgentMessage>,
    system_prompt: String,
    tools: Vec<std::sync::Arc<dyn crate::agent::agent::AgentTool>>,
    model: Model,
    provider_registry: Arc<RwLock<ProviderRegistry>>,
    note: String,
) -> BtwPanel {
    let (tx, rx) = channel::bounded::<BtwChunk>(256);
    let cancel = new_cancel_flag();
    let cancel2 = cancel.clone();
    let note2 = note.clone();
    let pricing = model.pricing.clone();

    std::thread::spawn(move || {
        stream_btw(messages, system_prompt, tools, model, provider_registry, note2, tx, cancel2);
    });

    BtwPanel::new(note, rx, cancel, pricing)
}

#[allow(clippy::too_many_arguments)]
fn stream_btw(
    messages: Vec<AgentMessage>,
    system_prompt: String,
    tools: Vec<std::sync::Arc<dyn crate::agent::agent::AgentTool>>,
    model: Model,
    provider_registry: Arc<RwLock<ProviderRegistry>>,
    note: String,
    tx: channel::Sender<BtwChunk>,
    cancel: CancelFlag,
) {
    let prompt_msg = AgentMessage::User {
        content: vec![ContentItem::Text { text: note }],
        timestamp: crate::agent::types::now_millis(),
    };

    let mut agent = Agent::new(provider_registry);
    // Use the exact same messages, system prompt, and tools as the main agent so
    // Anthropic's cache breakpoints match and the prefix is a cache hit.
    agent.state.messages = messages;
    // Direct assignment is safe here: this is a freshly constructed single-shot
    // Agent for the /btw panel. `prev_estimated_tokens` is 0; the context gate
    // will not fire on a one-call agent.
    agent.state.model = Some(model);
    // Append the btw instruction to the existing system prompt rather than
    // replacing it — keeps the cached system prompt prefix identical.
    agent.state.system_prompt = format!(
        "{system_prompt}\n\n<btw>The user is asking a side question while the agent works. \
        Answer concisely in 1-4 sentences without calling any tools.</btw>"
    );
    agent.cancel = cancel;
    // Same tools as the main agent — required so tool_use blocks in the history
    // are valid. The btw instruction above discourages the model from calling them.
    agent.state.tools = tools;

    let tx2 = tx.clone();
    agent.prompt(
        vec![prompt_msg],
        &move |event| match event {
            AgentEvent::MessageUpdate { delta: StreamDelta::Text(text) } => {
                let _ = tx2.send(BtwChunk::Text(text));
            }
            AgentEvent::UsageUpdate { usage } => {
                let _ = tx2.send(BtwChunk::Usage(usage));
            }
            AgentEvent::AgentEnd { .. } => {
                let _ = tx2.send(BtwChunk::Done);
            }
            AgentEvent::MessageEnd { message } => {
                if let StopReason::Error { message: ref e } = message.stop_reason {
                    let _ = tx2.send(BtwChunk::Error(e.clone()));
                }
            }
            _ => {}
        },
        None,
    );

    let _ = tx.send(BtwChunk::Done); // idempotent safety
}
