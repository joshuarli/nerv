/// Interactive session picker for /resume.
use super::display::{format_age, shorten_path, truncate_str};
use super::theme;

pub struct SessionPicker {
    pub sessions: Vec<crate::session::manager::SessionSummary>,
    pub selected: usize,
}

impl SessionPicker {
    pub fn new(sessions: Vec<crate::session::manager::SessionSummary>) -> Self {
        Self {
            sessions,
            selected: 0,
        }
    }

    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.sessions.len() {
            self.selected += 1;
        }
    }

    pub fn selected_id(&self) -> Option<&str> {
        self.sessions
            .get(self.selected)
            .map(|s| s.id_short.as_str())
    }

    /// Render picker as formatted lines. Each line is a pre-styled string.
    pub fn render_lines(&self, repo_root: Option<&str>) -> Vec<String> {
        let home = crate::home_dir()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_default();
        let mut lines = Vec::new();

        lines.push(format!(
            "{}Sessions (↑↓ navigate, Enter select, Esc cancel):{}",
            theme::MUTED,
            theme::RESET,
        ));

        for (i, s) in self.sessions.iter().take(20).enumerate() {
            let age = format_age(&s.modified);
            let preview = if s.preview.is_empty() {
                "(empty)"
            } else {
                truncate_str(&s.preview, 50)
            };
            let cwd_short = shorten_path(&s.cwd, &home, repo_root);

            let (marker, end) = if i == self.selected {
                (theme::REVERSE, theme::RESET)
            } else {
                ("", "")
            };
            lines.push(format!(
                " {}{} {:>4} {:>3}msg {}  {}{}",
                marker, s.id_short, age, s.message_count, cwd_short, preview, end,
            ));
        }

        lines
    }
}
