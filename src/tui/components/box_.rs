use crate::tui::tui::Component;
use crate::tui::utils;

/// Unicode border around a child component.
pub struct Box_ {
    child: Box<dyn Component>,
    title: Option<String>,
}

impl Box_ {
    pub fn new(child: Box<dyn Component>) -> Self {
        Self { child, title: None }
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }
}

impl Component for Box_ {
    fn render(&self, width: u16) -> Vec<String> {
        if width < 4 {
            return self.child.render(width);
        }

        let inner_width = width - 4; // 2 for borders + 1 padding each side
        let child_lines = self.child.render(inner_width);
        let mut lines = Vec::with_capacity(child_lines.len() + 2);

        // Top border
        let top = if let Some(title) = &self.title {
            let title_display = utils::truncate_to_width(title, inner_width);
            let title_width = utils::visible_width(&title_display);
            let remaining = (width as usize).saturating_sub(4 + title_width as usize);
            format!("╭─ {} {}╮", title_display, "─".repeat(remaining))
        } else {
            format!("╭{}╮", "─".repeat((width - 2) as usize))
        };
        lines.push(top);

        // Content lines
        for line in &child_lines {
            let truncated = utils::truncate_to_width(line, inner_width);
            let content_width = utils::visible_width(&truncated);
            let padding = inner_width.saturating_sub(content_width) as usize;
            lines.push(format!("│ {}{} │", truncated, " ".repeat(padding)));
        }

        // Bottom border
        lines.push(format!("╰{}╯", "─".repeat((width - 2) as usize)));

        lines
    }

    fn invalidate(&mut self) {
        self.child.invalidate();
    }
}
