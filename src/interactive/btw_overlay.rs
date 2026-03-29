/// Full-screen alt-screen overlay for `/btw` — a one-shot ephemeral Q&A that
/// runs a parallel Agent call against a snapshot of the current conversation.
///
/// The overlay owns stdin while it is open; the main TUI is suspended on the
/// alt screen behind it.  The response is *never* appended to the main session.
///
/// Testable logic lives in pure functions at the bottom of the file.
use std::io::{self, Write};
use std::mem::MaybeUninit;
use std::sync::{Arc, RwLock};

use crossbeam_channel as channel;

use crate::agent::agent::Agent;
use crate::agent::provider::{ProviderRegistry, new_cancel_flag};
use crate::agent::types::{
    AgentEvent, AgentMessage, AssistantMessage, ContentBlock, ContentItem, Model, StopReason,
    StreamDelta,
};
use crate::interactive::theme;
use crate::tui::keys;
use crate::tui::stdin_buffer::{StdinBuffer, StdinEvent};

// ─────────────────────────────── public entry ────────────────────────────────

/// Pause stdin, open the btw overlay, block until dismissed, resume stdin.
///
/// `messages` — snapshot of the current conversation (not mutated).
/// `model`    — the model currently in use by the main session.
/// `provider_registry` — shared Arc so we can make an API call.
/// `note`     — the user's question / context note.
pub fn run_btw_overlay(
    messages: Vec<AgentMessage>,
    model: Model,
    provider_registry: Arc<RwLock<ProviderRegistry>>,
    note: String,
) {
    let mut out = io::stdout();

    drain_stdin();

    // Enter alt screen, hide cursor.
    let _ = out.write_all(b"\x1b[?1049h\x1b[?25l");
    let _ = out.flush();

    // ── streaming thread ───────────────────────────────────────────────────
    // Channel delivers chunks to the render loop.
    let (chunk_tx, chunk_rx) = channel::bounded::<Chunk>(256);
    let cancel = new_cancel_flag();
    let cancel2 = cancel.clone();

    let thread_note = note.clone();
    let thread_model = model.clone();

    std::thread::Builder::new()
        .name("nerv-btw".into())
        .stack_size(2 * 1024 * 1024)
        .spawn(move || {
            stream_btw(messages, thread_model, provider_registry, thread_note, chunk_tx, cancel2);
        })
        .expect("failed to spawn btw thread");

    // ── render state ───────────────────────────────────────────────────────
    let mut response = String::new();
    let mut done = false;
    let mut error: Option<String> = None;

    // Draw the initial frame (empty response area, spinner implied by !done).
    render(&mut out, &note, &response, done, error.as_deref());

    // ── event loop ─────────────────────────────────────────────────────────
    let mut stdin_buf = StdinBuffer::new();

    loop {
        // Non-blocking drain of streaming chunks first; batch into a single render.
        let mut needs_render = false;
        loop {
            match chunk_rx.try_recv() {
                Ok(Chunk::Text(t)) => {
                    response.push_str(&t);
                    needs_render = true;
                }
                Ok(Chunk::Done) => {
                    done = true;
                    needs_render = true;
                }
                Ok(Chunk::Error(e)) => {
                    error = Some(e);
                    done = true;
                    needs_render = true;
                }
                Err(channel::TryRecvError::Empty) => break,
                Err(channel::TryRecvError::Disconnected) => {
                    done = true;
                    needs_render = true;
                    break;
                }
            }
        }
        if needs_render {
            render(&mut out, &note, &response, done, error.as_deref());
        }

        // Block on stdin with a short timeout so we keep draining chunks.
        let mut pfd = libc::pollfd { fd: libc::STDIN_FILENO, events: libc::POLLIN, revents: 0 };
        // 80 ms — fast enough for smooth streaming, low enough CPU
        let ready = unsafe { libc::poll(&mut pfd, 1, 80) };

        if ready > 0 {
            let mut buf = [0u8; 256];
            let n = unsafe {
                libc::read(libc::STDIN_FILENO, buf.as_mut_ptr() as *mut libc::c_void, buf.len())
            };
            if n <= 0 {
                break;
            }

            let events = stdin_buf.process(&buf[..n as usize]);
            for event in events {
                if let StdinEvent::Sequence(seq) = event {
                    let is_quit = keys::matches_key(&seq, "escape")
                        || keys::matches_key(&seq, "ctrl+c")
                        || (done && keys::matches_key(&seq, "enter"));
                    if is_quit {
                        // Signal the streaming thread to stop.
                        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                        // Exit alt screen, show cursor, return.
                        let _ = write!(out, "\x1b[?25h\x1b[?1049l");
                        let _ = out.flush();
                        return;
                    }
                }
            }
        }

        // If done and there are no more chunks, stay open until user presses a
        // key.
    }

    let _ = write!(out, "\x1b[?25h\x1b[?1049l");
    let _ = out.flush();
}

/// Returns true if the turn completed cleanly (not aborted, not errored).
/// Used to guard `messages_snapshot` from accumulating partial/retried turns.
pub fn turn_succeeded(messages: &[AgentMessage]) -> bool {
    !messages.iter().any(|m| {
        matches!(
            m,
            AgentMessage::Assistant(a)
            if a.stop_reason.is_error()
                || matches!(a.stop_reason, StopReason::Aborted)
        )
    })
}

enum Chunk {
    Text(String),
    Done,
    Error(String),
}

/// System prompt for the ephemeral btw agent — keeps answers short and
/// tool-free.
pub const BTW_SYSTEM_PROMPT: &str = "\
You are a concise assistant helping a developer mid-task. \
Answer in 1-4 sentences. \
No markdown headers. \
No tool use.";

/// Strip all tool-related content from a message snapshot before sending to
/// btw.
///
/// The btw agent runs without any tools defined.  If we forward tool_use /
/// tool_result blocks to the Anthropic API, it returns a 400 error because
/// tool_use content requires matching tool definitions.  We keep only the text
/// content from each message so btw has the full conversational context without
/// the tool machinery.
///
/// - `AgentMessage::ToolResult` rows are dropped entirely (they carry no
///   prose).
/// - `AgentMessage::Assistant` rows have their `ContentBlock::ToolCall` blocks
///   removed; if no text/thinking remains, the message is dropped too.
/// - `AgentMessage::User` rows are passed through unchanged.
/// Build a one-line description of a single tool call for context summaries.
fn tool_call_summary(name: &str, args: &serde_json::Value) -> String {
    // Show the most useful argument for each tool so the model understands what
    // happened.
    let detail = match name {
        "bash" => args
            .get("command")
            .and_then(|v| v.as_str())
            .map(|s| s.char_indices().nth(120).map_or(s, |(i, _)| &s[..i]).to_string()),
        "read" | "edit" | "write" | "ls" | "find" => {
            args.get("path").and_then(|v| v.as_str()).map(|s| s.to_string())
        }
        "grep" => args.get("pattern").and_then(|v| v.as_str()).map(|s| {
            format!(
                "{:?}{}",
                s,
                args.get("path")
                    .and_then(|v| v.as_str())
                    .map(|p| format!(" in {p}"))
                    .unwrap_or_default()
            )
        }),
        _ => None,
    };
    match detail {
        Some(d) => format!("{name}({d})"),
        None => name.to_string(),
    }
}

pub fn strip_tool_content(messages: Vec<AgentMessage>) -> Vec<AgentMessage> {
    // Build a lookup from tool_call_id → first line of tool result text, so we
    // can attach a brief outcome to each tool-call summary.
    let mut result_snippets: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for msg in &messages {
        if let AgentMessage::ToolResult { tool_call_id, content, is_error, .. } = msg {
            let first_text = content
                .iter()
                .find_map(|item| {
                    if let ContentItem::Text { text } = item { Some(text.as_str()) } else { None }
                })
                .unwrap_or("");
            let first_line = first_text.lines().next().unwrap_or("");
            let snippet =
                first_line.char_indices().nth(120).map_or(first_line, |(i, _)| &first_line[..i]);
            let prefix = if *is_error { "error: " } else { "" };
            result_snippets.insert(tool_call_id.clone(), format!("{prefix}{snippet}"));
        }
    }

    messages
        .into_iter()
        .filter_map(|msg| match msg {
            // Drop raw ToolResult messages — their content is folded into the
            // assistant summary above.
            AgentMessage::ToolResult { .. } => None,
            AgentMessage::Assistant(a) => {
                // Collect any real text blocks first.
                let mut content_out: Vec<ContentBlock> = a
                    .content
                    .iter()
                    .filter(|b| matches!(b, ContentBlock::Text { .. }))
                    .cloned()
                    .collect();

                // Summarise tool calls as a single text block so btw understands
                // what the agent did, without requiring tool definitions in the API call.
                let tool_summaries: Vec<String> = a
                    .content
                    .iter()
                    .filter_map(|b| {
                        if let ContentBlock::ToolCall { id, name, arguments } = b {
                            let call = tool_call_summary(name, arguments);
                            let outcome = result_snippets
                                .get(id)
                                .filter(|s| !s.is_empty())
                                .map(|s| format!(" → {s}"))
                                .unwrap_or_default();
                            Some(format!("[{call}{outcome}]"))
                        } else {
                            None
                        }
                    })
                    .collect();

                if !tool_summaries.is_empty() {
                    content_out.push(ContentBlock::Text { text: tool_summaries.join("\n") });
                }

                if content_out.is_empty() {
                    None
                } else {
                    Some(AgentMessage::Assistant(AssistantMessage {
                        content: content_out,
                        stop_reason: a.stop_reason,
                        usage: a.usage,
                        timestamp: a.timestamp,
                    }))
                }
            }
            other => Some(other),
        })
        .collect()
}

fn stream_btw(
    messages: Vec<AgentMessage>,
    model: Model,
    provider_registry: Arc<RwLock<ProviderRegistry>>,
    note: String,
    tx: channel::Sender<Chunk>,
    cancel: crate::agent::provider::CancelFlag,
) {
    let prompt_msg = AgentMessage::User {
        content: vec![ContentItem::Text { text: note }],
        timestamp: crate::agent::types::now_millis(),
    };

    let mut agent = Agent::new(provider_registry);
    // Strip tool_use/tool_result blocks so the API call is valid without tools.
    agent.state.messages = strip_tool_content(messages);
    agent.state.model = Some(model);
    agent.state.system_prompt = BTW_SYSTEM_PROMPT.into();
    agent.cancel = cancel;
    // No tools — pure conversation.
    agent.state.tools = Vec::new();

    let tx2 = tx.clone();
    agent.prompt(
        vec![prompt_msg],
        &move |event| match event {
            AgentEvent::MessageUpdate { delta: StreamDelta::Text(text) } => {
                let _ = tx2.send(Chunk::Text(text));
            }
            AgentEvent::AgentEnd { .. } => {
                let _ = tx2.send(Chunk::Done);
            }
            AgentEvent::MessageEnd { message } => {
                if let crate::agent::types::StopReason::Error { message: ref e } =
                    message.stop_reason
                {
                    let _ = tx2.send(Chunk::Error(e.clone()));
                }
            }
            _ => {}
        },
        None,
    );

    let _ = tx.send(Chunk::Done); // idempotent safety
}

// ─────────────────────────────── rendering ───────────────────────────────────

fn render(out: &mut io::Stdout, note: &str, response: &str, done: bool, error: Option<&str>) {
    let (cols, rows) = term_size();
    let cols = cols as usize;

    let mut buf: Vec<u8> = Vec::with_capacity(8192);

    // Synchronized output — terminal batches clear + paint.
    buf.extend_from_slice(b"\x1b[?2026h\x1b[H\x1b[2J");

    // ── header ─────────────────────────────────────────────────────────────
    let label = " /btw ";
    let border_len = cols.saturating_sub(label.len() + 2);
    let left_dashes = border_len / 2;
    let right_dashes = border_len - left_dashes;

    push_str(&mut buf, theme::ACCENT);
    push_str(&mut buf, "╭");
    push_str(&mut buf, &"─".repeat(left_dashes));
    push_str(&mut buf, label);
    push_str(&mut buf, &"─".repeat(right_dashes));
    push_str(&mut buf, "╮");
    push_str(&mut buf, theme::RESET);
    buf.extend_from_slice(b"\r\n");

    // ── note (wrapped inside the box) ──────────────────────────────────────
    let inner = cols.saturating_sub(4); // "│ " + " │"
    for line in wrap_text(note, inner) {
        push_str(&mut buf, theme::ACCENT);
        push_str(&mut buf, "│ ");
        push_str(&mut buf, theme::MUTED);
        let padded = pad_right(&line, inner);
        push_str(&mut buf, &padded);
        push_str(&mut buf, theme::ACCENT);
        push_str(&mut buf, " │");
        push_str(&mut buf, theme::RESET);
        buf.extend_from_slice(b"\r\n");
    }

    // Bottom of note box.
    push_str(&mut buf, theme::ACCENT);
    push_str(&mut buf, "╰");
    push_str(&mut buf, &"─".repeat(cols.saturating_sub(2)));
    push_str(&mut buf, "╯");
    push_str(&mut buf, theme::RESET);
    buf.extend_from_slice(b"\r\n");

    // ── response area ──────────────────────────────────────────────────────
    buf.extend_from_slice(b"\r\n");

    let max_response_rows = rows.saturating_sub(8) as usize; // leave room for header + hint

    if let Some(e) = error {
        push_str(&mut buf, theme::ERROR);
        push_str(&mut buf, "Error: ");
        push_str(&mut buf, e);
        push_str(&mut buf, theme::RESET);
        buf.extend_from_slice(b"\r\n");
    } else if response.is_empty() && !done {
        // Waiting for first token — show a subtle spinner-like cue.
        push_str(&mut buf, theme::DIM);
        push_str(&mut buf, "▸ …");
        push_str(&mut buf, theme::RESET);
        buf.extend_from_slice(b"\r\n");
    } else {
        // Word-wrap the response text, show the tail (most recent lines).
        let wrapped = wrap_text(response, cols);
        let start = wrapped.len().saturating_sub(max_response_rows);
        for line in &wrapped[start..] {
            push_str(&mut buf, line);
            buf.extend_from_slice(b"\r\n");
        }
    }

    // ── footer hint ────────────────────────────────────────────────────────
    // Position at the bottom.
    let hint_row = rows;
    write!(&mut buf as &mut dyn Write, "\x1b[{};1H", hint_row).ok();
    push_str(&mut buf, theme::DIM);
    if done {
        push_str(&mut buf, "  [Enter/Esc] close");
    } else {
        push_str(&mut buf, "  [Esc] cancel");
    }
    push_str(&mut buf, theme::RESET);

    // End synchronized output.
    buf.extend_from_slice(b"\x1b[?2026l");

    let _ = out.write_all(&buf);
    let _ = out.flush();
}

// ─────────────────────────────── helpers ─────────────────────────────────────

/// Word-wrap `text` to `max_chars` columns. Returns visual lines.
/// Handles `\n` as explicit breaks. Words longer than `max_chars` are
/// hard-wrapped at the column boundary rather than allowed to overflow.
pub fn wrap_text(text: &str, max_chars: usize) -> Vec<String> {
    if max_chars == 0 {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            // Hard-wrap any word that is itself wider than max_chars.
            let mut remaining = word;
            while remaining.chars().count() > max_chars {
                let split_byte = remaining
                    .char_indices()
                    .nth(max_chars)
                    .map(|(i, _)| i)
                    .unwrap_or(remaining.len());
                if !current.is_empty() {
                    lines.push(std::mem::take(&mut current));
                }
                lines.push(remaining[..split_byte].to_string());
                remaining = &remaining[split_byte..];
            }
            // remaining is now <= max_chars chars
            let word_len = remaining.chars().count();
            if word_len == 0 {
                continue;
            }
            let cur_len = current.chars().count();
            if cur_len == 0 {
                current.push_str(remaining);
            } else if cur_len + 1 + word_len <= max_chars {
                current.push(' ');
                current.push_str(remaining);
            } else {
                lines.push(std::mem::take(&mut current));
                current.push_str(remaining);
            }
        }
        if !current.is_empty() {
            lines.push(current);
        }
    }
    lines
}

/// Pad a string to exactly `width` visible characters with spaces.
/// Assumes no ANSI codes in the input.
pub fn pad_right(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        // Truncate safely at char boundary.
        s.char_indices().nth(width).map_or(s, |(i, _)| &s[..i]).to_string()
    } else {
        let mut out = s.to_string();
        for _ in 0..(width - len) {
            out.push(' ');
        }
        out
    }
}

fn push_str(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(s.as_bytes());
}

fn drain_stdin() {
    let mut buf = [0u8; 256];
    let mut pfd = libc::pollfd { fd: libc::STDIN_FILENO, events: libc::POLLIN, revents: 0 };
    loop {
        let ready = unsafe { libc::poll(&mut pfd, 1, 0) };
        if ready <= 0 {
            break;
        }
        let n = unsafe {
            libc::read(libc::STDIN_FILENO, buf.as_mut_ptr() as *mut libc::c_void, buf.len())
        };
        if n <= 0 {
            break;
        }
    }
}

fn term_size() -> (u16, u16) {
    unsafe {
        let mut ws = MaybeUninit::<libc::winsize>::uninit();
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, ws.as_mut_ptr()) == 0 {
            let ws = ws.assume_init();
            (ws.ws_col, ws.ws_row)
        } else {
            (80, 24)
        }
    }
}
