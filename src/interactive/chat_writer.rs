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
    blocks: Vec<Block>,
    streaming: Option<StreamingState>,
    picker: Option<Vec<String>>,
    // Per-block render cache (interior mutability for use in &self render)
    cache: RefCell<RenderCache>,
}

struct RenderCache {
    block_lines: Vec<Vec<String>>,
    width: u16,
}

enum Block {
    Styled(Vec<String>),
    Markdown(String),
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
            blocks: Vec::new(),
            streaming: None,
            picker: None,
            cache: RefCell::new(RenderCache {
                block_lines: Vec::new(),
                width: 0,
            }),
        }
    }

    pub fn clear(&mut self) {
        self.blocks.clear();
        self.streaming = None;
        self.cache.borrow_mut().block_lines.clear();
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
                self.blocks.push(Block::Styled(style_thinking(&s.thinking)));
                self.blocks.push(Block::Spacer);
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
            self.blocks.push(Block::Styled(style_thinking(t)));
            self.blocks.push(Block::Spacer);
        }
        self.streaming = None;
        if !text.is_empty() {
            self.blocks.push(Block::Markdown(text.to_string()));
        }
        self.blocks.push(Block::Spacer);
    }

    pub fn cancel_stream(&mut self) {
        self.streaming = None;
    }

    pub fn push_user(&mut self, text: &str) {
        self.blocks.push(Block::Styled(vec![format!(
            "{} {}{}",
            theme::REVERSE,
            text,
            theme::RESET,
        )]));
        self.blocks.push(Block::Spacer);
    }

    pub fn push_tool_call(&mut self, name: &str, args: &serde_json::Value) {
        let detail = format_tool_call(name, args);
        self.blocks
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
        self.blocks.push(Block::Styled(lines));
    }

    pub fn push_styled(&mut self, style: &'static str, text: &str) {
        self.blocks.push(Block::Styled(vec![format!(
            "{}{}{}",
            style,
            text,
            theme::RESET,
        )]));
        self.blocks.push(Block::Spacer);
    }

    pub fn push_markdown_source(&mut self, text: &str) {
        self.blocks.push(Block::Markdown(text.to_string()));
        self.blocks.push(Block::Spacer);
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

        // Render only new blocks (cache hit for existing ones)
        for i in 0..self.blocks.len() {
            if i < cache.block_lines.len() {
                out.extend_from_slice(&cache.block_lines[i]);
            } else {
                let rendered = render_block(&self.blocks[i], width);
                out.extend_from_slice(&rendered);
                cache.block_lines.push(rendered);
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
        Block::Spacer => vec![String::new()],
    }
}

fn style_thinking(thinking: &str) -> Vec<String> {
    thinking
        .lines()
        .map(|l| format!("{}│ {}{}", theme::THINKING, l, theme::RESET))
        .collect()
}
