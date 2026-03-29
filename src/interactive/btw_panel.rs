/// Inline `/btw` panel — renders a bordered box above the editor with a
/// streamed answer from a background Agent call.  The main agent continues
/// running unaffected.  Dismissed by Esc or Enter.
use std::sync::{Arc, RwLock};

use crossbeam_channel as channel;

use crate::agent::agent::Agent;
use crate::agent::provider::{new_cancel_flag, CancelFlag, ProviderRegistry};
use crate::agent::types::{AgentEvent, AgentMessage, ContentItem, Model, StopReason, StreamDelta};
use crate::interactive::btw_overlay::turn_succeeded;
use crate::interactive::theme;
use crate::tui::tui::Component;

// ─────────────────────────────── chunk protocol ──────────────────────────────

pub enum BtwChunk {
    Text(String),
    Error(String),
    Done,
}

// ─────────────────────────────── panel ───────────────────────────────────────

/// Minimum height of the panel (border + 1 content line + border).
const MIN_PANEL_LINES: usize = 3;

/// Maximum lines of content shown in the panel.
const MAX_CONTENT_LINES: usize = 12;

pub struct BtwPanel {
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
}

impl BtwPanel {
    pub fn new(note: String, rx: channel::Receiver<BtwChunk>, cancel: CancelFlag) -> Self {
        Self {
            note,
            response: String::new(),
            done: false,
            error: None,
            rx,
            cancel,
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

    /// Number of terminal lines this panel will occupy when rendered.
    pub fn line_count(&self, width: usize) -> usize {
        let inner = width.saturating_sub(4); // "│ " + " │"
        let content = self.content_lines(inner);
        // top border + content rows + bottom border
        1 + content.len().max(1) + 1
    }

    fn content_lines(&self, inner_width: usize) -> Vec<String> {
        if let Some(err) = &self.error {
            return wrap_text(&format!("error: {}", err), inner_width);
        }
        if self.response.is_empty() {
            if self.done {
                return vec!["(no response)".into()];
            }
            return vec!["…".into()];
        }
        let mut lines = wrap_text(&self.response, inner_width);
        // Keep only the last MAX_CONTENT_LINES lines so the panel doesn't grow unboundedly.
        if lines.len() > MAX_CONTENT_LINES {
            lines = lines[lines.len() - MAX_CONTENT_LINES..].to_vec();
        }
        lines
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
        let hint = if self.done { " ↵ dismiss " } else { "" };
        let bottom_dashes = w.saturating_sub(hint.len() + 2);
        lines.push(format!(
            "{}╰{}{}╯{}",
            theme::ACCENT,
            "─".repeat(bottom_dashes),
            hint,
            theme::RESET,
        ));

        lines
    }
}

// ─────────────────────────────── helpers ─────────────────────────────────────

/// Wrap `text` to at most `max_chars` visible characters per line, splitting on
/// word boundaries.  Handles `\n` in the source text.
fn wrap_text(text: &str, max_chars: usize) -> Vec<String> {
    if max_chars == 0 {
        return vec![];
    }
    let mut out = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut line = String::new();
        let mut line_len = 0usize;
        for word in paragraph.split_whitespace() {
            let wlen = word.chars().count();
            if line.is_empty() {
                line.push_str(word);
                line_len = wlen;
            } else if line_len + 1 + wlen <= max_chars {
                line.push(' ');
                line.push_str(word);
                line_len += 1 + wlen;
            } else {
                out.push(line.clone());
                line.clear();
                line.push_str(word);
                line_len = wlen;
            }
        }
        if !line.is_empty() {
            out.push(line);
        }
    }
    out
}

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

    std::thread::spawn(move || {
        stream_btw(messages, system_prompt, tools, model, provider_registry, note2, tx, cancel2);
    });

    BtwPanel::new(note, rx, cancel)
}

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
