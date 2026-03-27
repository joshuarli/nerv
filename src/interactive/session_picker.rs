/// Interactive session picker for /resume with FTS5 search.
use super::display::{format_age, shorten_path, truncate_str};
use super::theme;

use crate::session::manager::{SearchResult, SessionSummary};

enum PickerMode {
    Browse,
    Search,
}

pub struct SessionPicker {
    all_sessions: Vec<SessionSummary>,
    search_results: Vec<SearchResult>,
    pub query: String,
    pub selected: usize,
    mode: PickerMode,
}

impl SessionPicker {
    pub fn new(sessions: Vec<SessionSummary>) -> Self {
        Self {
            all_sessions: sessions,
            search_results: Vec::new(),
            query: String::new(),
            selected: 0,
            mode: PickerMode::Browse,
        }
    }

    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub fn move_down(&mut self) {
        let len = self.visible_count();
        if self.selected + 1 < len {
            self.selected += 1;
        }
    }

    pub fn push_char(&mut self, ch: char) {
        self.query.push(ch);
        self.mode = PickerMode::Search;
        self.selected = 0;
    }

    pub fn pop_char(&mut self) {
        self.query.pop();
        if self.query.is_empty() {
            self.mode = PickerMode::Browse;
        }
        self.selected = 0;
    }

    pub fn clear_query(&mut self) {
        self.query.clear();
        self.mode = PickerMode::Browse;
        self.selected = 0;
    }

    pub fn update_results(&mut self, results: Vec<SearchResult>) {
        self.search_results = results;
        if self.selected >= self.visible_count() {
            self.selected = self.visible_count().saturating_sub(1);
        }
    }

    pub fn has_query(&self) -> bool {
        !self.query.is_empty()
    }

    fn visible_count(&self) -> usize {
        match self.mode {
            PickerMode::Browse => self.all_sessions.len().min(20),
            PickerMode::Search => self.search_results.len().min(20),
        }
    }

    pub fn selected_id(&self) -> Option<&str> {
        match self.mode {
            PickerMode::Browse => self
                .all_sessions
                .get(self.selected)
                .map(|s| s.id_short.as_str()),
            PickerMode::Search => self
                .search_results
                .get(self.selected)
                .map(|s| s.id_short.as_str()),
        }
    }

    pub fn render_lines(&self, repo_root: Option<&str>) -> Vec<String> {
        let home = crate::home_dir()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_default();
        let mut lines = Vec::new();

        // Search input line
        lines.push(format!(
            "{}Search sessions:{} {}{}\u{2588}{}",
            theme::MUTED,
            theme::RESET,
            theme::BOLD,
            self.query,
            theme::RESET,
        ));

        match self.mode {
            PickerMode::Browse => {
                if self.all_sessions.is_empty() {
                    lines.push(format!(
                        " {}No previous sessions found.{}",
                        theme::MUTED,
                        theme::RESET,
                    ));
                } else {
                    for (i, s) in self.all_sessions.iter().take(20).enumerate() {
                        let age = format_age(&s.modified);
                        // Show the auto-generated name if available, otherwise fall back to preview.
                        let label = if let Some(ref name) = s.name {
                            name.as_str()
                        } else if s.preview.is_empty() {
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
                            marker, s.id_short, age, s.message_count, cwd_short, label, end,
                        ));
                    }
                }
            }
            PickerMode::Search => {
                if self.search_results.is_empty() {
                    lines.push(format!(
                        " {}No matches.{}",
                        theme::MUTED,
                        theme::RESET,
                    ));
                } else {
                    for (i, r) in self.search_results.iter().take(20).enumerate() {
                        let age = format_age(&r.modified);
                        let cwd_short = shorten_path(&r.cwd, &home, repo_root);
                        let excerpt = truncate_str(&r.excerpt, 60);

                        let selected = i == self.selected;
                        // Replace placeholders with ANSI codes. When the row is
                        // selected (REVERSE), use bold for highlights instead of
                        // color so the reset doesn't break the reverse video.
                        let excerpt = if selected {
                            excerpt
                                .replace("<<HL>>", theme::BOLD)
                                .replace("<</HL>>", theme::RESET_BOLD)
                        } else {
                            excerpt
                                .replace("<<HL>>", theme::MATCH_HL)
                                .replace("<</HL>>", theme::RESET)
                        };

                        let (marker, end) = if selected {
                            (theme::REVERSE, theme::RESET)
                        } else {
                            ("", "")
                        };
                        lines.push(format!(
                            " {}{} {:>4} {:>3}msg {}  {}{}",
                            marker, r.id_short, age, r.message_count, cwd_short, excerpt, end,
                        ));
                    }
                }
            }
        }

        lines.push(format!(
            " {}↑↓ navigate, Enter select, Esc cancel{}",
            theme::MUTED,
            theme::RESET,
        ));

        lines
    }
}
