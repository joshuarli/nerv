/// Full-screen session picker for /session.
///
/// Implements [`FullscreenList`] so it can be driven by
/// [`run_fullscreen_picker`]. Search is performed synchronously via the
/// `search_fn` closure passed at construction, which typically calls
/// `SessionManager::search_sessions` on a dedicated read-only DB connection.
use std::io::Write;

use super::display::{format_age, shorten_path, truncate_str};
use super::fullscreen_picker::FullscreenList;
use super::theme;
use crate::session::manager::{SearchResult, SessionSummary};

// ─────────────────────────── types ──────────────────────────────────────────

type SearchFn = dyn Fn(&str) -> Vec<SearchResult> + 'static;

enum Mode {
    Browse,
    Search,
}

pub struct SessionPicker {
    all_sessions: Vec<SessionSummary>,
    search_results: Vec<SearchResult>,
    query: String,
    selected: usize,
    mode: Mode,
    /// Synchronous search callback.
    search_fn: Box<SearchFn>,
    repo_root: Option<String>,
}

// ─────────────────────────── impl ───────────────────────────────────────────

impl SessionPicker {
    pub fn new(
        sessions: Vec<SessionSummary>,
        search_fn: Box<SearchFn>,
        repo_root: Option<String>,
    ) -> Self {
        Self {
            all_sessions: sessions,
            search_results: Vec::new(),
            query: String::new(),
            selected: 0,
            mode: Mode::Browse,
            search_fn,
            repo_root,
        }
    }

    fn visible_count(&self) -> usize {
        match self.mode {
            Mode::Browse => self.all_sessions.len(),
            Mode::Search => self.search_results.len(),
        }
    }

    fn run_search(&mut self) {
        self.search_results = (self.search_fn)(&self.query);
        self.selected = 0;
    }
}

// ─────────────────────── FullscreenList impl ─────────────────────────────────

impl FullscreenList for SessionPicker {
    fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    fn move_down(&mut self) {
        if self.selected + 1 < self.visible_count() {
            self.selected += 1;
        }
    }

    fn push_char(&mut self, ch: char) {
        self.query.push(ch);
        self.mode = Mode::Search;
        self.run_search();
    }

    fn pop_char(&mut self) {
        self.query.pop();
        if self.query.is_empty() {
            self.mode = Mode::Browse;
            self.selected = 0;
        } else {
            self.run_search();
        }
    }

    fn clear_query(&mut self) {
        self.query.clear();
        self.mode = Mode::Browse;
        self.selected = 0;
    }

    fn enter(&self) -> Option<String> {
        match self.mode {
            Mode::Browse => self.all_sessions.get(self.selected).map(|s| s.id_short.clone()),
            Mode::Search => self.search_results.get(self.selected).map(|r| r.id_short.clone()),
        }
    }

    fn render(&self, out: &mut dyn Write, cols: u16, rows: u16) {
        let home = crate::home_dir().map(|h| h.to_string_lossy().to_string()).unwrap_or_default();
        let repo = self.repo_root.as_deref();

        // Available rows: 1 header + 1 search bar + list + 1 footer
        let list_rows = (rows as usize).saturating_sub(3);
        let cols = cols as usize;

        // ── header ─────────────────────────────────────────────────────────
        let title = "  Sessions";
        let hint = "↑↓ navigate · Enter select · Esc cancel  ";
        let gap = cols.saturating_sub(title.len() + hint.len());
        let _ = write!(
            out,
            "\x1b[H{bold}{title}{reset}{muted}{gap}{hint}{reset}\r\n",
            bold = theme::BOLD,
            reset = theme::RESET,
            muted = theme::MUTED,
            title = title,
            gap = " ".repeat(gap),
            hint = hint,
        );

        // ── search bar ─────────────────────────────────────────────────────
        let _ = write!(
            out,
            "{muted}  /{reset} {bold}{query}{reset}{cursor}\x1b[K\r\n",
            muted = theme::MUTED,
            reset = theme::RESET,
            bold = theme::BOLD,
            query = self.query,
            cursor = if self.query.is_empty() { "\u{2588}" } else { "" },
        );

        // ── list ───────────────────────────────────────────────────────────
        match self.mode {
            Mode::Browse => {
                if self.all_sessions.is_empty() {
                    let _ = write!(
                        out,
                        "\r\n  {}No previous sessions.{}\r\n",
                        theme::MUTED,
                        theme::RESET
                    );
                } else {
                    for (i, s) in self.all_sessions.iter().take(list_rows).enumerate() {
                        render_session_row(out, i, self.selected, s, &home, repo, cols);
                    }
                }
            }
            Mode::Search => {
                if self.search_results.is_empty() {
                    let _ = write!(out, "\r\n  {}No matches.{}\r\n", theme::MUTED, theme::RESET);
                } else {
                    for (i, r) in self.search_results.iter().take(list_rows).enumerate() {
                        render_search_row(out, i, self.selected, r, &home, repo, cols);
                    }
                }
            }
        }
    }
}

// ─────────────────────────── row helpers ─────────────────────────────────────

fn render_session_row(
    out: &mut dyn Write,
    idx: usize,
    selected: usize,
    s: &SessionSummary,
    home: &str,
    repo: Option<&str>,
    cols: usize,
) {
    let selected = idx == selected;
    let age = format_age(&s.modified);
    let label = if let Some(ref name) = s.name {
        name.as_str()
    } else if s.preview.is_empty() {
        "(empty)"
    } else {
        &s.preview
    };
    let cwd_short = shorten_path(&s.cwd, home, repo);

    // Fixed columns: "  {id_short}  {age}  {msg_count}msg  {cwd}  "
    let meta = format!("  {}  {:>4}  {:>3}msg  {}  ", s.id_short, age, s.message_count, cwd_short);
    let label_width = cols.saturating_sub(meta.len());
    let label = truncate_str(label, label_width);

    if selected {
        let _ = write!(
            out,
            "{rev}{meta}{label:<lw$}{reset}\x1b[K\r\n",
            rev = theme::REVERSE,
            reset = theme::RESET,
            meta = meta,
            label = label,
            lw = label_width
        );
    } else {
        let _ = write!(out, "{meta}{label}\x1b[K\r\n", meta = meta, label = label);
    }
}

fn render_search_row(
    out: &mut dyn Write,
    idx: usize,
    selected: usize,
    r: &SearchResult,
    home: &str,
    repo: Option<&str>,
    cols: usize,
) {
    let is_selected = idx == selected;
    let age = format_age(&r.modified);
    let cwd_short = shorten_path(&r.cwd, home, repo);

    let meta = format!("  {}  {:>4}  {:>3}msg  {}  ", r.id_short, age, r.message_count, cwd_short);
    let excerpt_width = cols.saturating_sub(meta.len());
    let raw_excerpt = truncate_str(&r.excerpt, excerpt_width * 2); // allow some slop for ANSI tags

    // Resolve <<HL>>…<</HL>> placeholders.
    let excerpt = if is_selected {
        raw_excerpt.replace("<<HL>>", theme::BOLD).replace("<</HL>>", theme::RESET_BOLD)
    } else {
        raw_excerpt.replace("<<HL>>", theme::MATCH_HL).replace("<</HL>>", theme::RESET)
    };

    if is_selected {
        let _ = write!(
            out,
            "{rev}{meta}{excerpt}{reset}\x1b[K\r\n",
            rev = theme::REVERSE,
            reset = theme::RESET,
            meta = meta,
            excerpt = excerpt
        );
    } else {
        let _ = write!(out, "{meta}{excerpt}\x1b[K\r\n", meta = meta, excerpt = excerpt);
    }
}
