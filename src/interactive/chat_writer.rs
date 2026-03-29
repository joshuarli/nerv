use std::cell::RefCell;

use crate::interactive::display::{format_tool_call, render_tool_result_line, truncate_str};
use crate::interactive::theme;
use crate::tui::components::markdown::Markdown;
use crate::tui::tui::Component;
use crate::tui::utils::wrap_text_with_ansi;

/// Centralized chat output. All content — user messages, streaming
/// thinking/text, tool calls, tool results, status — flows through here.
/// Mutated in place by the event loop; rendered by the TUI each frame.
///
/// Permanent blocks are cached after first render. Only new blocks and
/// the live streaming tail are rendered on each frame.
pub struct ChatWriter {
    blocks: RefCell<Vec<Block>>,
    streaming: Option<StreamingState>,
    picker: Option<Vec<String>>,
    // Per-block render cache (interior mutability for use in &self render)
    cache: RefCell<RenderCache>,
    /// Total rendered lines already evicted to terminal scrollback (absolute offset).
    /// notify_flushed receives an absolute scrollback_flushed count from the TUI;
    /// we subtract this to get the delta relative to the current block list.
    lines_evicted: usize,
}

struct RenderCache {
    block_lines: Vec<Vec<String>>,
    width: u16,
}

enum Block {
    Styled(Vec<String>),
    Markdown(String),
    /// Source freed after first render; lines stored for resize fallback.
    Rendered(Vec<String>),
    Spacer,
}

struct StreamingState {
    thinking: String,
    text: String,
    thinking_committed: bool,
}

impl Default for ChatWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl ChatWriter {
    pub fn new() -> Self {
        Self {
            blocks: RefCell::new(Vec::new()),
            streaming: None,
            picker: None,
            cache: RefCell::new(RenderCache {
                block_lines: Vec::new(),
                width: 0,
            }),
            lines_evicted: 0,
        }
    }

    pub fn clear(&mut self) {
        self.blocks.borrow_mut().clear();
        self.streaming = None;
        self.cache.borrow_mut().block_lines.clear();
        self.lines_evicted = 0;
    }

    /// Reset the eviction baseline. Call whenever the TUI resets scrollback_flushed
    /// to 0 (e.g. after suspend/resume), so future notify_flushed deltas are correct.
    pub fn reset_eviction(&mut self) {
        self.lines_evicted = 0;
    }

    /// Called after the TUI flushes `flushed_lines` lines to terminal scrollback
    /// (absolute cumulative count since last full redraw).
    /// Drops source blocks and cached render lines that are fully covered, freeing
    /// their heap allocation.  Re-render cost is zero because the terminal owns
    /// the scrollback — these blocks will never be diff-rendered again.
    pub fn notify_flushed(&mut self, flushed_lines: usize) {
        // Convert absolute TUI count to delta relative to our current block list.
        // lines_evicted tracks how many lines we have already dropped in prior calls.
        let delta = flushed_lines.saturating_sub(self.lines_evicted);
        if delta == 0 {
            return;
        }
        let mut cache = self.cache.borrow_mut();
        let mut covered = 0usize;
        let mut to_drop = 0usize; // number of leading blocks to evict
        for block_lines in &cache.block_lines {
            let next = covered + block_lines.len();
            if next > delta {
                break; // this block straddles the flush boundary — keep it
            }
            covered = next;
            to_drop += 1;
        }
        if to_drop == 0 {
            return;
        }
        // Drop cached render lines for flushed blocks.
        cache.block_lines.drain(..to_drop);
        // Drop source blocks too — they are fully owned by terminal scrollback.
        self.blocks.borrow_mut().drain(..to_drop);
        // Advance the eviction cursor by the lines we just dropped.
        self.lines_evicted += covered;
        // block_lines and blocks are now in sync again (same length, same offset).
    }

    pub fn begin_stream(&mut self) {
        self.streaming = Some(StreamingState {
            thinking: String::new(),
            text: String::new(),
            thinking_committed: false,
        });
    }

    pub fn append_thinking(&mut self, delta: &str) {
        if let Some(ref mut s) = self.streaming {
            s.thinking.push_str(delta);
        }
    }

    pub fn append_text(&mut self, delta: &str) {
        if let Some(ref mut s) = self.streaming {
            if !s.thinking_committed && !s.thinking.is_empty() {
                self.blocks.borrow_mut().push(Block::Styled(style_thinking(&s.thinking)));
                self.blocks.borrow_mut().push(Block::Spacer);
                s.thinking_committed = true;
            }
            s.text.push_str(delta);
        }
    }

    pub fn finish_stream(&mut self, text: &str, thinking: Option<&str>) {
        if let Some(ref s) = self.streaming
            && !s.thinking_committed
            && let Some(t) = thinking
            && !t.is_empty()
        {
            self.blocks.borrow_mut().push(Block::Styled(style_thinking(t)));
            self.blocks.borrow_mut().push(Block::Spacer);
        }
        self.streaming = None;
        if !text.is_empty() {
            self.blocks.borrow_mut().push(Block::Markdown(text.to_string()));
        }
        self.blocks.borrow_mut().push(Block::Spacer);
    }

    pub fn cancel_stream(&mut self) {
        self.streaming = None;
    }

    pub fn push_user(&mut self, text: &str) {
        self.blocks.borrow_mut().push(Block::Styled(vec![format!(
            "{} {}{}",
            theme::REVERSE,
            text,
            theme::RESET,
        )]));
        self.blocks.borrow_mut().push(Block::Spacer);
    }

    pub fn push_tool_call(&mut self, name: &str, args: &serde_json::Value) {
        let detail = format_tool_call(name, args);
        self.blocks
            .borrow_mut()
            .push(Block::Styled(vec![format!("{}› {}", theme::DIM, detail)]));
    }

    pub fn push_tool_result(&mut self, content: &str, is_error: bool) {
        let truncated = truncate_str(content, 2000);
        let mut lines = Vec::new();
        for line in truncated.split('\n').take(30) {
            lines.push(format!("  {}", render_tool_result_line(line, is_error)));
        }
        let total = content.lines().count();
        if total > 30 {
            lines.push(format!(
                "{}  ... {} more lines{}",
                theme::DIM,
                total - 30,
                theme::RESET,
            ));
            lines.push(String::new());
        }
        self.blocks.borrow_mut().push(Block::Styled(lines));
    }

    pub fn push_styled(&mut self, style: &'static str, text: &str) {
        self.blocks.borrow_mut().push(Block::Styled(vec![format!(
            "{}{}{}",
            style,
            text,
            theme::RESET,
        )]));
        self.blocks.borrow_mut().push(Block::Spacer);
    }

    pub fn push_markdown_source(&mut self, text: &str) {
        self.blocks.borrow_mut().push(Block::Markdown(text.to_string()));
        self.blocks.borrow_mut().push(Block::Spacer);
    }

    pub fn streaming_len(&self) -> usize {
        self.streaming
            .as_ref()
            .map_or(0, |s| s.thinking.len() + s.text.len())
    }

    /// Set an ephemeral picker overlay (rendered after permanent blocks, not cached).
    pub fn set_picker(&mut self, items: Vec<String>) {
        self.picker = Some(items);
    }

    /// Clear the picker overlay.
    pub fn clear_picker(&mut self) {
        self.picker = None;
    }
}

impl Component for ChatWriter {
    fn render(&self, width: u16) -> Vec<String> {
        let mut cache = self.cache.borrow_mut();

        // Width changed — invalidate all cached blocks
        if width != cache.width {
            cache.block_lines.clear();
            cache.width = width;
        }

        let mut out = Vec::new();

        // Render only new blocks (cache hit for existing ones).
        // After caching a Markdown block, replace it with Block::Rendered to free
        // the raw source string — the rendered lines in block_lines are canonical now.
        let blocks_len = self.blocks.borrow().len();
        for i in 0..blocks_len {
            if i < cache.block_lines.len() {
                out.extend_from_slice(&cache.block_lines[i]);
            } else {
                let rendered = render_block(&self.blocks.borrow()[i], width);
                out.extend_from_slice(&rendered);
                cache.block_lines.push(rendered);
                // Free the Markdown source now that the rendered lines are cached.
                let mut blocks = self.blocks.borrow_mut();
                if matches!(blocks[i], Block::Markdown(_)) {
                    blocks[i] = Block::Rendered(cache.block_lines[i].clone());
                }
            }
        }

        // Live streaming content (never cached — changes every frame)
        if let Some(ref s) = self.streaming {
            if !s.thinking.is_empty() && !s.thinking_committed {
                for line in s
                    .thinking
                    .lines()
                    .rev()
                    .take(3)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                {
                    out.extend(wrap_text_with_ansi(
                        &format!("{}│ {}{}", theme::THINKING, line, theme::RESET),
                        width,
                    ));
                }
            }
            if !s.text.is_empty() {
                out.extend(Markdown::new(&s.text).render(width));
            }
        }

        // Ephemeral picker overlay (not cached)
        if let Some(ref items) = self.picker {
            for item in items {
                out.extend(wrap_text_with_ansi(item, width));
            }
        }

        out
    }
}

fn render_block(block: &Block, width: u16) -> Vec<String> {
    match block {
        Block::Styled(lines) => lines
            .iter()
            .flat_map(|line| wrap_text_with_ansi(line, width))
            .collect(),
        Block::Markdown(src) => Markdown::new(src).render(width),
        // Source was freed after first render; return stored lines verbatim.
        // If the terminal width changed these won't reflow, but notify_flushed
        // will drop them entirely before they accumulate.
        Block::Rendered(lines) => lines.clone(),
        Block::Spacer => vec![String::new()],
    }
}

fn style_thinking(thinking: &str) -> Vec<String> {
    thinking
        .lines()
        .map(|l| format!("{}│ {}{}", theme::THINKING, l, theme::RESET))
        .collect()
}
