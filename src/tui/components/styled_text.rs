use crate::tui::tui::Component;
use crate::tui::utils::wrap_text_with_ansi;

/// Text with a consistent ANSI style applied to every rendered line.
/// Handles wrapping correctly — each wrapped line gets the style prefix
/// and a reset suffix, preventing color bleed across component boundaries.
pub struct StyledText {
    style: &'static str,
    content: String,
}

impl StyledText {
    pub fn new(style: &'static str, content: impl Into<String>) -> Self {
        Self { style, content: content.into() }
    }
}

impl Component for StyledText {
    fn render(&self, width: u16) -> Vec<String> {
        if self.content.is_empty() {
            return vec![];
        }
        let wrapped = wrap_text_with_ansi(&self.content, width);
        wrapped
            .into_iter()
            .map(
                |line| {
                    if line.is_empty() { line } else { format!("{}{}\x1b[0m", self.style, line) }
                },
            )
            .collect()
    }
}
