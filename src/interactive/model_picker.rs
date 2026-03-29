/// Full-screen model picker for `/model`.
///
/// Implements [`FullscreenList`] so it can be driven by
/// [`run_fullscreen_picker`]. Typing filters models by name/id/provider;
/// up/down moves the cursor; Enter returns the selected model ID.
use std::io::Write;

use super::fullscreen_picker::FullscreenList;
use super::theme;
use crate::agent::types::Model;

pub struct ModelPicker {
    /// All available models.
    models: Vec<Model>,
    /// The ID of the currently-active model (shown with a ★ marker).
    current_id: String,
    /// Live filter query typed by the user.
    query: String,
    /// Cursor position into `filtered()`.
    cursor: usize,
}

impl ModelPicker {
    pub fn new(models: Vec<Model>, current_id: String) -> Self {
        Self { models, current_id, query: String::new(), cursor: 0 }
    }

    /// Models that match the current query, in order.
    fn filtered(&self) -> Vec<&Model> {
        let q = self.query.to_lowercase();
        self.models
            .iter()
            .filter(|m| {
                q.is_empty()
                    || m.name.to_lowercase().contains(&q)
                    || m.id.to_lowercase().contains(&q)
                    || m.provider_name.to_lowercase().contains(&q)
            })
            .collect()
    }
}

impl FullscreenList for ModelPicker {
    fn render(&self, out: &mut dyn Write, cols: u16, rows: u16) {
        let cols = cols as usize;
        let rows = rows as usize;

        // ── header (2 lines) ────────────────────────────────────────────────
        let title = "  Select model";
        let _ = write!(
            out,
            "{BOLD}{title:<width$}{RESET}\r\n",
            BOLD = theme::BOLD,
            RESET = theme::RESET,
            width = cols,
        );

        // Query bar
        let prompt = format!("  / {}", self.query);
        let _ = write!(
            out,
            "{ACCENT}{prompt:<width$}{RESET}\r\n",
            ACCENT = theme::ACCENT,
            RESET = theme::RESET,
            width = cols,
        );

        // Separator
        let _ = write!(out, "{}{}{}\r\n", theme::DIM, "─".repeat(cols), theme::RESET);

        // ── list ────────────────────────────────────────────────────────────
        let filtered = self.filtered();
        let list_rows = rows.saturating_sub(5); // header(2) + sep(1) + footer(2)
        let total = filtered.len();

        // Scroll so cursor stays visible.
        let scroll = if self.cursor >= list_rows { self.cursor - list_rows + 1 } else { 0 };

        let mut last_provider = "";
        let mut visual_row = 0usize;
        let mut model_idx = 0usize;

        for m in &filtered {
            // Provider heading
            if m.provider_name != last_provider {
                if visual_row >= scroll && visual_row - scroll < list_rows {
                    let heading = format!("  [{}]", m.provider_name);
                    let _ = write!(
                        out,
                        "{DIM}{heading:<width$}{RESET}\r\n",
                        DIM = theme::DIM,
                        RESET = theme::RESET,
                        width = cols,
                    );
                }
                visual_row += 1;
                last_provider = &m.provider_name;
            }

            let is_selected = model_idx == self.cursor;
            let is_current = m.id == self.current_id;

            if visual_row >= scroll && visual_row - scroll < list_rows {
                let marker = if is_current { "★" } else { " " };
                let label = format!("  {} {} ({})", marker, m.name, m.id);
                let label = if label.len() > cols {
                    format!("{}…", &label[..cols.saturating_sub(1)])
                } else {
                    format!("{label:<width$}", width = cols)
                };

                if is_selected {
                    let _ = write!(
                        out,
                        "\x1b[7m{}{}\x1b[27m{}\r\n",
                        if is_current { theme::ACCENT } else { "" },
                        label,
                        theme::RESET,
                    );
                } else if is_current {
                    let _ = write!(out, "{}{}{}\r\n", theme::ACCENT, label, theme::RESET);
                } else {
                    let _ = write!(out, "{}{}\r\n", label, theme::RESET);
                }
            }

            visual_row += 1;
            model_idx += 1;
        }

        // Fill empty rows
        let drawn = visual_row.saturating_sub(scroll).min(list_rows);
        for _ in drawn..list_rows {
            let _ = write!(out, "{:<width$}\r\n", "", width = cols);
        }

        if total == 0 {
            let msg =
                format!("  {DIM}no models match{RESET}", DIM = theme::DIM, RESET = theme::RESET);
            let _ = write!(out, "\x1b[4;1H{}", msg); // overwrite row 4
        }

        // ── footer ──────────────────────────────────────────────────────────
        let _ = write!(out, "{}{}{}\r\n", theme::DIM, "─".repeat(cols), theme::RESET);
        let hint = format!(
            "  {DIM}↑↓ navigate · enter select · esc cancel{RESET}",
            DIM = theme::DIM,
            RESET = theme::RESET,
        );
        let _ = write!(out, "{hint}");
    }

    fn move_up(&mut self) {
        let n = self.filtered().len();
        if n == 0 {
            return;
        }
        if self.cursor > 0 {
            self.cursor -= 1;
        } else {
            self.cursor = n - 1;
        }
    }

    fn move_down(&mut self) {
        let n = self.filtered().len();
        if n == 0 {
            return;
        }
        self.cursor = (self.cursor + 1) % n;
    }

    fn push_char(&mut self, ch: char) {
        self.query.push(ch);
        // Clamp cursor to new filtered length.
        let n = self.filtered().len();
        if n > 0 && self.cursor >= n {
            self.cursor = n - 1;
        }
    }

    fn pop_char(&mut self) {
        self.query.pop();
        let n = self.filtered().len();
        if n > 0 && self.cursor >= n {
            self.cursor = n - 1;
        }
    }

    fn clear_query(&mut self) {
        self.query.clear();
        self.cursor = 0;
    }

    /// Returns "provider_name/model_id" for the selected entry.
    fn enter(&self) -> Option<String> {
        let filtered = self.filtered();
        filtered.get(self.cursor).map(|m| format!("{}/{}", m.provider_name, m.id))
    }
}
