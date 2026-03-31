//! Word-wrapping text component.
//!
//! Delegates to `utils::wrap_text_with_ansi` so ANSI styling is preserved
//! across soft-wrapped lines. An optional left indent is prepended to every
//! output line.

use crate::tui::tui::Component;
use crate::tui::utils;

pub struct WordWrap {
    content: String,
    /// Number of spaces to prepend to every rendered line.
    indent: u16,
}

impl WordWrap {
    pub fn new(content: impl Into<String>) -> Self {
        Self { content: content.into(), indent: 0 }
    }

    pub fn with_indent(mut self, indent: u16) -> Self {
        self.indent = indent;
        self
    }

    pub fn set_content(&mut self, content: impl Into<String>) {
        self.content = content.into();
    }
}

impl Component for WordWrap {
    fn render(&self, width: u16) -> Vec<String> {
        let inner_width = width.saturating_sub(self.indent);
        let lines = utils::wrap_text_with_ansi(&self.content, inner_width);
        if self.indent == 0 {
            return lines;
        }
        let pad = " ".repeat(self.indent as usize);
        lines.into_iter().map(|l| format!("{pad}{l}")).collect()
    }
}
