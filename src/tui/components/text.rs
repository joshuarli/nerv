use crate::tui::tui::Component;
use crate::tui::utils;

/// Static text with optional ANSI styling. Wraps to terminal width.
pub struct Text {
    content: String,
}

impl Text {
    pub fn new(content: impl Into<String>) -> Self {
        Self { content: content.into() }
    }

    pub fn set_content(&mut self, content: impl Into<String>) {
        self.content = content.into();
    }
}

impl Component for Text {
    fn render(&self, width: u16) -> Vec<String> {
        utils::wrap_text_with_ansi(&self.content, width)
    }
}

/// Truncates content to fit within the viewport width, appending "…" if needed.
pub struct TruncatedText {
    content: String,
}

impl TruncatedText {
    pub fn new(content: impl Into<String>) -> Self {
        Self { content: content.into() }
    }

    pub fn set_content(&mut self, content: impl Into<String>) {
        self.content = content.into();
    }
}

impl Component for TruncatedText {
    fn render(&self, width: u16) -> Vec<String> {
        vec![utils::truncate_to_width(&self.content, width)]
    }
}
