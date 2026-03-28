use crate::tui::keys;
use crate::tui::tui::{CURSOR_MARKER, Component};
use crate::tui::utils::visible_width;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub struct Editor {
    /// Logical lines of text (split by \n).
    lines: Vec<String>,
    /// Cursor position: line index.
    cursor_line: usize,
    /// Cursor position: byte offset within the line (grapheme-aligned).
    cursor_col: usize,
    scroll_offset: usize,
    max_visible_lines: usize,
    undo_stack: Vec<(Vec<String>, usize, usize)>,
    undo_index: usize,
    kill_ring: Vec<String>,
    focused: bool,
    /// Paste storage: id → full pasted content.
    pastes: std::collections::HashMap<u32, String>,
    paste_counter: u32,
    /// Autocomplete candidates (set externally).
    completions: Vec<String>,
    /// Active autocomplete state: (filtered matches, selected index).
    autocomplete: Option<(Vec<String>, usize)>,
}

struct LayoutLine {
    text: String,
    has_cursor: bool,
    /// Byte offset within `text` where the cursor sits (if has_cursor).
    cursor_pos: usize,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

impl Editor {
    pub fn new() -> Self {
        Self {
            lines: vec![String::new()],
            cursor_line: 0,
            cursor_col: 0,
            scroll_offset: 0,
            max_visible_lines: 10,
            undo_stack: vec![(vec![String::new()], 0, 0)],
            undo_index: 0,
            kill_ring: Vec::new(),
            focused: true,
            pastes: std::collections::HashMap::new(),
            paste_counter: 0,
            completions: Vec::new(),
            autocomplete: None,
        }
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn set_text(&mut self, text: &str) {
        self.lines = text.split('\n').map(|s| s.to_string()).collect();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cursor_line = self.lines.len() - 1;
        self.cursor_col = self.lines[self.cursor_line].len();
        self.push_undo();
    }

    pub fn clear(&mut self) {
        self.lines = vec![String::new()];
        self.cursor_line = 0;
        self.cursor_col = 0;
        self.scroll_offset = 0;
    }

    /// Take the current text and clear the editor.
    /// Expands any paste markers back to their full content.
    pub fn take_text(&mut self) -> String {
        let mut text = self.text();

        // Expand paste markers: [paste #N +M lines] or [paste #N M chars]
        for (&id, content) in &self.pastes {
            let patterns = [
                format!("[paste #{} ", id), // matches both forms
            ];
            for pat in &patterns {
                if let Some(start) = text.find(pat)
                    && let Some(end) = text[start..].find(']')
                {
                    text.replace_range(start..start + end + 1, content);
                    break;
                }
            }
        }

        self.lines = vec![String::new()];
        self.cursor_line = 0;
        self.cursor_col = 0;
        self.scroll_offset = 0;
        self.undo_stack = vec![(vec![String::new()], 0, 0)];
        self.undo_index = 0;
        self.pastes.clear();
        self.paste_counter = 0;
        text
    }

    /// Set slash command completions.
    pub fn set_completions(&mut self, completions: Vec<String>) {
        self.completions = completions;
    }

    fn update_autocomplete(&mut self) {
        let text = &self.lines[0];
        if !text.starts_with('/') || self.cursor_line != 0 {
            self.autocomplete = None;
            return;
        }
        let prefix = text.to_lowercase();
        let matches: Vec<String> = self
            .completions
            .iter()
            .filter(|c| c.to_lowercase().starts_with(&prefix))
            .cloned()
            .collect();
        if matches.is_empty() {
            self.autocomplete = None;
        } else {
            self.autocomplete = Some((matches, 0));
        }
    }

    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    /// Handle a paste event. Large pastes (>10 lines or >1000 chars) are stored
    /// and replaced with a `[paste #N +M lines]` marker in the editor.
    pub fn insert_paste(&mut self, text: &str) {
        let line_count = text.lines().count();
        if line_count > 10 || text.len() > 1000 {
            self.paste_counter += 1;
            let id = self.paste_counter;
            self.pastes.insert(id, text.to_string());
            let marker = if line_count > 10 {
                format!("[paste #{} +{} lines]", id, line_count)
            } else {
                format!("[paste #{} {} chars]", id, text.len())
            };
            self.insert_char(&marker);
        } else {
            // Small paste — insert directly, handling newlines
            for (i, line) in text.split('\n').enumerate() {
                if i > 0 {
                    self.insert_newline();
                }
                if !line.is_empty() {
                    self.insert_char(line);
                }
            }
        }
    }

    fn current_line(&self) -> &str {
        &self.lines[self.cursor_line]
    }

    fn push_undo(&mut self) {
        const MAX_UNDO: usize = 50;
        self.undo_stack.truncate(self.undo_index + 1);
        self.undo_stack
            .push((self.lines.clone(), self.cursor_line, self.cursor_col));
        // Drop the oldest entry when over the cap so memory is bounded.
        if self.undo_stack.len() > MAX_UNDO {
            self.undo_stack.remove(0);
        }
        self.undo_index = self.undo_stack.len() - 1;
    }

    fn push_kill(&mut self, s: String) {
        const MAX_KILL: usize = 20;
        self.kill_ring.push(s);
        if self.kill_ring.len() > MAX_KILL {
            self.kill_ring.remove(0);
        }
    }

    fn undo(&mut self) {
        if self.undo_index > 0 {
            self.undo_index -= 1;
            let (lines, cl, cc) = self.undo_stack[self.undo_index].clone();
            self.lines = lines;
            self.cursor_line = cl;
            self.cursor_col = cc;
        }
    }

    // -- Grapheme-level cursor helpers --

    /// Grapheme count up to byte offset in a string.
    fn grapheme_count_at(s: &str, byte_offset: usize) -> usize {
        s[..byte_offset.min(s.len())].graphemes(true).count()
    }

    /// Byte offset of the nth grapheme in a string.
    fn byte_offset_of_grapheme(s: &str, n: usize) -> usize {
        s.grapheme_indices(true)
            .nth(n)
            .map(|(i, _)| i)
            .unwrap_or(s.len())
    }

    /// Number of graphemes in a string.
    fn grapheme_len(s: &str) -> usize {
        s.graphemes(true).count()
    }

    /// Move cursor left by one grapheme.
    fn move_left(&mut self) {
        if self.cursor_col > 0 {
            let gi = Self::grapheme_count_at(self.current_line(), self.cursor_col);
            self.cursor_col = Self::byte_offset_of_grapheme(self.current_line(), gi - 1);
        } else if self.cursor_line > 0 {
            self.cursor_line -= 1;
            self.cursor_col = self.current_line().len();
        }
    }

    /// Move cursor right by one grapheme.
    fn move_right(&mut self) {
        let line_len = self.current_line().len();
        if self.cursor_col < line_len {
            let gi = Self::grapheme_count_at(self.current_line(), self.cursor_col);
            self.cursor_col = Self::byte_offset_of_grapheme(self.current_line(), gi + 1);
        } else if self.cursor_line + 1 < self.lines.len() {
            self.cursor_line += 1;
            self.cursor_col = 0;
        }
    }

    fn move_home(&mut self) {
        self.cursor_col = 0;
    }

    fn move_end(&mut self) {
        self.cursor_col = self.current_line().len();
    }

    fn move_up(&mut self) {
        if self.cursor_line > 0 {
            let visual_col = Self::grapheme_count_at(self.current_line(), self.cursor_col);
            self.cursor_line -= 1;
            let target = visual_col.min(Self::grapheme_len(self.current_line()));
            self.cursor_col = Self::byte_offset_of_grapheme(self.current_line(), target);
        }
    }

    fn move_down(&mut self) {
        if self.cursor_line + 1 < self.lines.len() {
            let visual_col = Self::grapheme_count_at(self.current_line(), self.cursor_col);
            self.cursor_line += 1;
            let target = visual_col.min(Self::grapheme_len(self.current_line()));
            self.cursor_col = Self::byte_offset_of_grapheme(self.current_line(), target);
        }
    }

    /// Move cursor forward by one word.
    fn move_word_forward(&mut self) {
        let line = &self.lines[self.cursor_line];
        let rest = &line[self.cursor_col..];
        // Skip current word chars, then skip whitespace
        let mut chars = rest.char_indices();
        let mut found_non_word = false;
        for (i, ch) in &mut chars {
            if !ch.is_alphanumeric() && ch != '_' {
                found_non_word = true;
            } else if found_non_word {
                self.cursor_col += i;
                return;
            }
        }
        // Reached end of line — move to next line start
        if self.cursor_line + 1 < self.lines.len() {
            self.cursor_line += 1;
            self.cursor_col = 0;
        } else {
            self.cursor_col = line.len();
        }
    }

    /// Move cursor backward by one word.
    fn move_word_backward(&mut self) {
        let line = &self.lines[self.cursor_line];
        let before = &line[..self.cursor_col];
        // Walk backwards: skip whitespace/punctuation, then skip word chars
        let mut chars = before.char_indices().rev();
        let mut found_word = false;
        for (i, ch) in &mut chars {
            if ch.is_alphanumeric() || ch == '_' {
                found_word = true;
            } else if found_word {
                self.cursor_col = i + ch.len_utf8();
                return;
            }
        }
        if found_word {
            self.cursor_col = 0;
        } else if self.cursor_line > 0 {
            // At start of line — move to end of previous line
            self.cursor_line -= 1;
            self.cursor_col = self.lines[self.cursor_line].len();
        }
    }

    /// Kill word forward (Alt+D).
    fn kill_word_forward(&mut self) {
        let line = &self.lines[self.cursor_line];
        let rest = &line[self.cursor_col..];
        let mut end = self.cursor_col;
        let mut chars = rest.char_indices();
        let mut found_non_word = false;
        for (i, ch) in &mut chars {
            if !ch.is_alphanumeric() && ch != '_' {
                found_non_word = true;
            } else if found_non_word {
                end = self.cursor_col + i;
                break;
            }
            end = self.cursor_col + i + ch.len_utf8();
        }
        if end > self.cursor_col {
            let killed = self.lines[self.cursor_line][self.cursor_col..end].to_string();
            self.lines[self.cursor_line].drain(self.cursor_col..end);
            self.push_kill(killed);
            self.push_undo();
        }
    }

    /// Kill word backward (Alt+Backspace / Ctrl+W).
    fn kill_word_backward(&mut self) {
        let line = &self.lines[self.cursor_line];
        let before = &line[..self.cursor_col];
        let mut start = 0;
        let mut chars = before.char_indices().rev();
        let mut found_word = false;
        for (i, ch) in &mut chars {
            if ch.is_alphanumeric() || ch == '_' {
                found_word = true;
            } else if found_word {
                start = i + ch.len_utf8();
                break;
            }
        }
        if self.cursor_col > start {
            let killed = self.lines[self.cursor_line][start..self.cursor_col].to_string();
            self.lines[self.cursor_line].drain(start..self.cursor_col);
            self.cursor_col = start;
            self.push_kill(killed);
            self.push_undo();
        }
    }

    /// Transpose the two characters before the cursor (Ctrl+T).
    fn transpose_chars(&mut self) {
        let line = &self.lines[self.cursor_line];
        let gi = Self::grapheme_count_at(line, self.cursor_col);
        if gi < 2 {
            return;
        }
        let g1_start = Self::byte_offset_of_grapheme(line, gi - 2);
        let g1_end = Self::byte_offset_of_grapheme(line, gi - 1);
        let g2_end = Self::byte_offset_of_grapheme(line, gi);
        let g1 = line[g1_start..g1_end].to_string();
        let g2 = line[g1_end..g2_end].to_string();
        self.lines[self.cursor_line].replace_range(g1_start..g2_end, &format!("{}{}", g2, g1));
        self.push_undo();
    }

    fn insert_char(&mut self, ch: &str) {
        self.lines[self.cursor_line].insert_str(self.cursor_col, ch);
        self.cursor_col += ch.len();
        self.push_undo();
    }

    fn insert_newline(&mut self) {
        let rest = self.lines[self.cursor_line][self.cursor_col..].to_string();
        self.lines[self.cursor_line].truncate(self.cursor_col);
        self.cursor_line += 1;
        self.lines.insert(self.cursor_line, rest);
        self.cursor_col = 0;
        self.push_undo();
    }

    fn delete_backward(&mut self) {
        if self.cursor_col > 0 {
            let gi = Self::grapheme_count_at(self.current_line(), self.cursor_col);
            let prev = Self::byte_offset_of_grapheme(self.current_line(), gi - 1);
            self.lines[self.cursor_line].drain(prev..self.cursor_col);
            self.cursor_col = prev;
            self.push_undo();
        } else if self.cursor_line > 0 {
            // Join with previous line
            let current = self.lines.remove(self.cursor_line);
            self.cursor_line -= 1;
            self.cursor_col = self.lines[self.cursor_line].len();
            self.lines[self.cursor_line].push_str(&current);
            self.push_undo();
        }
    }

    fn delete_forward(&mut self) {
        let line_len = self.current_line().len();
        if self.cursor_col < line_len {
            let gi = Self::grapheme_count_at(self.current_line(), self.cursor_col);
            let next = Self::byte_offset_of_grapheme(self.current_line(), gi + 1);
            self.lines[self.cursor_line].drain(self.cursor_col..next);
            self.push_undo();
        } else if self.cursor_line + 1 < self.lines.len() {
            // Join with next line
            let next = self.lines.remove(self.cursor_line + 1);
            self.lines[self.cursor_line].push_str(&next);
            self.push_undo();
        }
    }

    fn kill_to_end(&mut self) {
        let line = &self.lines[self.cursor_line];
        if self.cursor_col < line.len() {
            let killed = line[self.cursor_col..].to_string();
            self.lines[self.cursor_line].truncate(self.cursor_col);
            self.push_kill(killed);
        } else if self.cursor_line + 1 < self.lines.len() {
            // Kill the newline — join with next line
            let next = self.lines.remove(self.cursor_line + 1);
            self.lines[self.cursor_line].push_str(&next);
            self.push_kill("\n".into());
        }
        self.push_undo();
    }

    fn kill_line_backward(&mut self) {
        if self.cursor_col > 0 {
            let killed = self.lines[self.cursor_line][..self.cursor_col].to_string();
            self.lines[self.cursor_line].drain(..self.cursor_col);
            self.cursor_col = 0;
            self.push_kill(killed);
        } else if self.cursor_line > 0 {
            let current = self.lines.remove(self.cursor_line);
            self.cursor_line -= 1;
            self.cursor_col = self.lines[self.cursor_line].len();
            self.lines[self.cursor_line].push_str(&current);
            self.push_kill("\n".into());
        }
        self.push_undo();
    }

    fn yank(&mut self) {
        if let Some(text) = self.kill_ring.last().cloned() {
            // Yanked text may contain newlines
            for (i, part) in text.split('\n').enumerate() {
                if i > 0 {
                    self.insert_newline();
                }
                if !part.is_empty() {
                    self.insert_char(part);
                }
            }
        }
    }

    // -- Layout (word wrapping + cursor tracking) --

    fn layout_text(&self, content_width: u16) -> Vec<LayoutLine> {
        let mut layout = Vec::new();

        if self.is_empty() {
            layout.push(LayoutLine {
                text: String::new(),
                has_cursor: true,
                cursor_pos: 0,
            });
            return layout;
        }

        for (line_idx, line) in self.lines.iter().enumerate() {
            let is_cursor_line = line_idx == self.cursor_line;
            let line_width = visible_width(line);

            if line_width <= content_width {
                // Fits in one layout line
                layout.push(LayoutLine {
                    text: line.clone(),
                    has_cursor: is_cursor_line,
                    cursor_pos: if is_cursor_line { self.cursor_col } else { 0 },
                });
            } else {
                // Word-wrap into chunks
                let chunks =
                    wrap_line_with_cursor(line, content_width, is_cursor_line, self.cursor_col);
                layout.extend(chunks);
            }
        }

        layout
    }

    /// Open $EDITOR with the current buffer, replace buffer with result.
    pub fn open_in_external_editor(&mut self) -> bool {
        let editor_cmd = std::env::var("EDITOR")
            .or_else(|_| std::env::var("VISUAL"))
            .unwrap_or_else(|_| "vi".into());

        let tmp_dir = std::env::temp_dir();
        let tmp_path = tmp_dir.join(format!("nerv-edit-{}.txt", std::process::id()));

        // Write current buffer to temp file, expanding paste markers
        let mut content = self.text();
        for (&id, paste_content) in &self.pastes {
            let pat = format!("[paste #{} ", id);
            if let Some(start) = content.find(&pat)
                && let Some(end) = content[start..].find(']')
            {
                content.replace_range(start..start + end + 1, paste_content);
            }
        }
        if std::fs::write(&tmp_path, &content).is_err() {
            return false;
        }

        let status = std::process::Command::new(&editor_cmd)
            .arg(&tmp_path)
            .stdin(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .status();

        let ok = match status {
            Ok(s) if s.success() => {
                if let Ok(content) = std::fs::read_to_string(&tmp_path) {
                    // Strip trailing newline that editors typically add
                    let content = content.trim_end_matches('\n').to_string();
                    self.set_text(&content);
                    true
                } else {
                    false
                }
            }
            _ => false,
        };

        let _ = std::fs::remove_file(&tmp_path);
        ok
    }
}

/// Wrap a single logical line into layout chunks, tracking cursor position.
fn wrap_line_with_cursor(
    line: &str,
    width: u16,
    is_cursor_line: bool,
    cursor_col: usize,
) -> Vec<LayoutLine> {
    let mut chunks = Vec::new();
    let graphemes: Vec<(usize, &str)> = line.grapheme_indices(true).collect();

    let mut chunk_start = 0;
    let mut chunk_width: u16 = 0;

    for &(byte_offset, grapheme) in &graphemes {
        let gw = UnicodeWidthStr::width(grapheme) as u16;

        if chunk_width + gw > width && chunk_width > 0 {
            // Emit current chunk
            let chunk_end = byte_offset;
            let text = line[chunk_start..chunk_end].to_string();

            let has_cursor = is_cursor_line && cursor_col >= chunk_start && cursor_col < chunk_end;
            let cursor_pos = if has_cursor {
                cursor_col - chunk_start
            } else {
                0
            };

            chunks.push(LayoutLine {
                text,
                has_cursor,
                cursor_pos,
            });

            chunk_start = byte_offset;
            chunk_width = 0;
        }

        chunk_width += gw;
    }

    // Last chunk
    let text = line[chunk_start..].to_string();
    let has_cursor = is_cursor_line && cursor_col >= chunk_start;
    let cursor_pos = if has_cursor {
        cursor_col - chunk_start
    } else {
        0
    };
    chunks.push(LayoutLine {
        text,
        has_cursor,
        cursor_pos,
    });

    chunks
}

impl Component for Editor {
    fn render(&self, width: u16) -> Vec<String> {
        let content_width = width.saturating_sub(2); // 1 char padding each side
        if content_width == 0 {
            return vec![];
        }

        let layout = self.layout_text(content_width);

        // Find cursor line in layout
        let cursor_layout_idx = layout.iter().position(|l| l.has_cursor).unwrap_or(0);

        // Adjust scroll to keep cursor visible
        let mut scroll = self.scroll_offset;
        if cursor_layout_idx < scroll {
            scroll = cursor_layout_idx;
        } else if cursor_layout_idx >= scroll + self.max_visible_lines {
            scroll = cursor_layout_idx - self.max_visible_lines + 1;
        }
        let max_scroll = layout.len().saturating_sub(self.max_visible_lines);
        scroll = scroll.min(max_scroll);

        let visible = &layout[scroll..(scroll + self.max_visible_lines).min(layout.len())];

        let border = "\x1b[38;5;240m";
        let reset = "\x1b[0m";
        let _hl = format!("{}─{}", border, reset);

        let mut result = Vec::new();

        // Top border
        if scroll > 0 {
            result.push(format!(
                "{}─── ↑ {} more {}{}",
                border,
                scroll,
                "─".repeat(width.saturating_sub(12 + digit_count(scroll)) as usize),
                reset,
            ));
        } else {
            result.push(format!("{}{}{}", border, "─".repeat(width as usize), reset));
        }

        // Content
        for layout_line in visible {
            let mut display = layout_line.text.clone();
            let line_width = visible_width(&display);

            if layout_line.has_cursor && self.focused {
                let before = &display[..layout_line.cursor_pos];
                let after = &display[layout_line.cursor_pos..];
                // Inject only the cursor marker — the hardware block cursor (set by
                // the TUI renderer via DECSCUSR + show) handles the visual display.
                // A second reverse-video highlight would fight the real cursor and
                // make it invisible in focused terminal panes.
                display = format!("{}{}{}", before, CURSOR_MARKER, after);
            }

            let padding = " ".repeat(content_width.saturating_sub(line_width) as usize);
            result.push(format!(" {}{}", display, padding));
        }

        // Bottom border
        let lines_below = layout.len().saturating_sub(scroll + visible.len());
        if lines_below > 0 {
            result.push(format!(
                "{}─── ↓ {} more {}{}",
                border,
                lines_below,
                "─".repeat(width.saturating_sub(12 + digit_count(lines_below)) as usize),
                reset,
            ));
        } else {
            result.push(format!("{}{}{}", border, "─".repeat(width as usize), reset));
        }

        result
    }

    fn handle_input(&mut self, input: &[u8]) -> bool {
        if !self.focused {
            return false;
        }

        // Tab: autocomplete slash commands
        if keys::matches_key(input, "tab") {
            if let Some((ref matches, ref mut idx)) = self.autocomplete {
                if !matches.is_empty() {
                    *idx = (*idx + 1) % matches.len();
                    let completion = matches[*idx % matches.len()].clone();
                    self.lines[0] = completion;
                    self.cursor_line = 0;
                    self.cursor_col = self.lines[0].len();
                    return true;
                }
            } else {
                self.update_autocomplete();
                if let Some((ref matches, _)) = self.autocomplete
                    && !matches.is_empty()
                {
                    let completion = matches[0].clone();
                    self.lines[0] = completion;
                    self.cursor_line = 0;
                    self.cursor_col = self.lines[0].len();
                    return true;
                }
            }
            return false;
        }

        // Escape dismisses autocomplete
        if keys::matches_key(input, "escape") && self.autocomplete.is_some() {
            self.autocomplete = None;
            return true;
        }

        // Any non-tab key clears autocomplete
        self.autocomplete = None;

        // Movement
        if keys::matches_key(input, "left") || keys::matches_key(input, "ctrl+b") {
            self.move_left();
            return true;
        }
        if keys::matches_key(input, "right") || keys::matches_key(input, "ctrl+f") {
            self.move_right();
            return true;
        }
        if keys::matches_key(input, "up") || keys::matches_key(input, "ctrl+p") {
            self.move_up();
            return true;
        }
        if keys::matches_key(input, "down") || keys::matches_key(input, "ctrl+n") {
            self.move_down();
            return true;
        }
        if keys::matches_key(input, "ctrl+a") || keys::matches_key(input, "home") {
            self.move_home();
            return true;
        }
        if keys::matches_key(input, "ctrl+e") || keys::matches_key(input, "end") {
            self.move_end();
            return true;
        }
        // Word movement (alt+f/b = Emacs, alt+right/left = macOS/GUI convention,
        // ctrl+right/left = common terminal convention for word skip)
        if keys::matches_key(input, "alt+f")
            || keys::matches_key(input, "alt+right")
            || keys::matches_key(input, "ctrl+right")
        {
            self.move_word_forward();
            return true;
        }
        if keys::matches_key(input, "alt+b")
            || keys::matches_key(input, "alt+left")
            || keys::matches_key(input, "ctrl+left")
        {
            self.move_word_backward();
            return true;
        }
        // Deletion
        if keys::matches_key(input, "backspace") {
            self.delete_backward();
            return true;
        }
        if keys::matches_key(input, "delete") || keys::matches_key(input, "ctrl+d") {
            self.delete_forward();
            return true;
        }
        if keys::matches_key(input, "ctrl+k") {
            self.kill_to_end();
            return true;
        }
        if keys::matches_key(input, "ctrl+u") {
            self.kill_line_backward();
            return true;
        }
        // Word kill
        if keys::matches_key(input, "alt+d") {
            self.kill_word_forward();
            return true;
        }
        if keys::matches_key(input, "alt+backspace") || keys::matches_key(input, "ctrl+w") {
            self.kill_word_backward();
            return true;
        }
        // Transpose
        if keys::matches_key(input, "ctrl+t") {
            self.transpose_chars();
            return true;
        }
        // Yank
        if keys::matches_key(input, "ctrl+y") {
            self.yank();
            return true;
        }
        if keys::matches_key(input, "ctrl+z") {
            self.undo();
            return true;
        }

        // Newline (from Shift+Enter)
        if input == b"\n" {
            self.insert_newline();
            return true;
        }

        // Printable characters
        if let Ok(s) = std::str::from_utf8(input)
            && !s.is_empty()
            && !input.contains(&0x1B)
            && input[0] >= 0x20
        {
            self.insert_char(s);
            return true;
        }

        false
    }
}

impl Editor {
    pub fn focused(&self) -> bool {
        self.focused
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }
}

fn digit_count(n: usize) -> u16 {
    if n == 0 {
        return 1;
    }
    (n as f64).log10().floor() as u16 + 1
}
