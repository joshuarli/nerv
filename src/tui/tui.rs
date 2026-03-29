use std::io::Write;

use super::terminal::Terminal;
use super::utils::visible_width;

/// APC sequence used as a cursor position marker. Terminals ignore it;
/// TUI finds and strips it during rendering, then positions the hardware
/// cursor at that location for IME candidate window positioning.
pub const CURSOR_MARKER: &str = "\x1b_pi:c\x07";

pub trait Component {
    /// Render to lines for the given viewport width. Pure — no side effects.
    fn render(&self, width: u16) -> Vec<String>;

    /// Handle raw terminal bytes. Returns true if input was consumed.
    fn handle_input(&mut self, _input: &[u8]) -> bool {
        false
    }

    /// Invalidate cached render state (theme change, forced redraw).
    fn invalidate(&mut self) {}
}

pub struct Container {
    pub children: Vec<Box<dyn Component>>,
}

impl Default for Container {
    fn default() -> Self {
        Self::new()
    }
}

impl Container {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
        }
    }

    pub fn push(&mut self, child: Box<dyn Component>) {
        self.children.push(child);
    }

    pub fn len(&self) -> usize {
        self.children.len()
    }

    pub fn is_empty(&self) -> bool {
        self.children.is_empty()
    }

    pub fn pop(&mut self) -> Option<Box<dyn Component>> {
        self.children.pop()
    }

    pub fn clear(&mut self) {
        self.children.clear();
    }
}

impl Component for Container {
    fn render(&self, width: u16) -> Vec<String> {
        self.children.iter().flat_map(|c| c.render(width)).collect()
    }

    fn invalidate(&mut self) {
        for c in &mut self.children {
            c.invalidate();
        }
    }
}

pub struct TUI {
    terminal: Box<dyn Terminal>,
    previous_lines: Vec<String>,
    previous_width: u16,
    previous_height: u16,
    max_lines_rendered: usize,
    previous_viewport_top: usize,
    render_requested: bool,
    stopped: bool,
    /// Reusable byte buffer — avoids allocation per frame.
    write_buf: Vec<u8>,
    /// Number of lines already flushed to terminal scrollback.
    scrollback_flushed: usize,
    /// Lines at the bottom that are fixed UI (editor, statusbar, footer).
    /// These are never flushed to scrollback.
    pub fixed_bottom: usize,
}

impl TUI {
    pub fn new(terminal: Box<dyn Terminal>) -> Self {
        Self {
            terminal,
            previous_lines: Vec::new(),
            previous_width: 0,
            previous_height: 0,
            max_lines_rendered: 0,
            previous_viewport_top: 0,
            render_requested: true,
            stopped: false,
            write_buf: Vec::with_capacity(8192),
            scrollback_flushed: 0,
            fixed_bottom: 0,
        }
    }

    pub fn request_render(&mut self, full: bool) {
        self.render_requested = true;
        if full {
            self.previous_lines.clear();
        }
    }

    pub fn maybe_render(&mut self, root: &dyn Component) {
        if self.render_requested && !self.stopped {
            self.do_render(root);
            self.render_requested = false;
        }
    }

    pub fn terminal(&self) -> &dyn Terminal {
        &*self.terminal
    }

    pub fn terminal_mut(&mut self) -> &mut dyn Terminal {
        &mut *self.terminal
    }

    pub fn scrollback_flushed(&self) -> usize {
        self.scrollback_flushed
    }

    pub fn width(&self) -> u16 {
        self.terminal.columns()
    }

    pub fn height(&self) -> u16 {
        self.terminal.rows()
    }

    pub fn dump_scrollback(&mut self, text: &str) {
        self.terminal.dump_scrollback(text);
        self.previous_lines.clear();
        self.render_requested = true;
        self.scrollback_flushed = 0;
    }

    pub fn suspend(&mut self) {
        self.terminal.stop();
    }

    pub fn resume(&mut self) {
        self.terminal.restart();
        self.previous_lines.clear();
        self.render_requested = true;
        self.scrollback_flushed = 0;
    }

    fn do_render(&mut self, root: &dyn Component) {
        let width = self.terminal.columns();
        let height = self.terminal.rows();

        let mut new_lines = root.render(width);

        // Find and strip CURSOR_MARKER
        let mut cursor_pos: Option<(usize, u16)> = None;
        let viewport_top = new_lines.len().saturating_sub(height as usize);
        for (line_idx, line) in new_lines.iter_mut().enumerate() {
            if let Some(marker_pos) = line.find(CURSOR_MARKER) {
                let col = visible_width(&line[..marker_pos]);
                *line = line.replace(CURSOR_MARKER, "");
                if line_idx >= viewport_top {
                    cursor_pos = Some((line_idx - viewport_top, col));
                }
            }
        }

        // Append reset + hyperlink close to each non-empty line (before flush
        // so the scrollback copy has correct styling too).
        for line in &mut new_lines {
            if !line.is_empty() {
                line.push_str("\x1b[0m\x1b]8;;\x07");
            }
        }

        // Reuse buffer — clear but keep allocation
        self.write_buf.clear();
        let buf = &mut self.write_buf;

        // Synchronized output begin (DEC 2026) — wraps the flush AND the
        // viewport redraw so neither is visible as an intermediate state.
        buf.extend_from_slice(b"\x1b[?2026h");

        // Flush overflow to scrollback by printing the new lines at the top
        // of the screen and then scrolling them into the terminal's natural
        // scrollback buffer via newlines at the bottom row.  We never use
        // ED 2 (\x1b[2J) here because many terminals (Terminal.app, iTerm2)
        // treat that as "clear scrollback too", which is exactly what we want
        // to avoid.
        let scrollable_end = new_lines.len().saturating_sub(self.fixed_bottom);
        let flush_limit = viewport_top.min(scrollable_end);
        if flush_limit > self.scrollback_flushed {
            let delta = flush_limit - self.scrollback_flushed;

            // Step 1: overwrite the top `delta` rows of the screen with the
            // lines that should enter scrollback, starting at row 1.
            buf.extend_from_slice(b"\x1b[H");
            for line in &new_lines[self.scrollback_flushed..flush_limit] {
                buf.extend_from_slice(line.as_bytes());
                buf.extend_from_slice(b"\r\n");
            }

            // Step 2: move to the last row and emit newlines to scroll the
            // lines written in Step 1 into the terminal's scrollback buffer.
            // Each \r\n at the bottom row scrolls the screen up by one line.
            // If delta <= height, we need exactly `delta` newlines.
            // If delta > height, Step 1 already scrolled `delta - height`
            // lines into scrollback on its own (the cursor ran off the bottom
            // mid-print), so we only need `height` newlines to clear the rest.
            let scroll_count = delta.min(height as usize);
            let _ = write!(buf, "\x1b[{};1H", height);
            for _ in 0..scroll_count {
                buf.extend_from_slice(b"\r\n");
            }

            self.scrollback_flushed = flush_limit;
            // Force a full redraw so the viewport is repainted cleanly.
            self.previous_lines.clear();
        }

        // (Synchronized output begin was already emitted above)

        let need_full_render = width != self.previous_width
            || height != self.previous_height
            || self.previous_lines.is_empty();

        if need_full_render {
            Self::full_render(buf, &new_lines, height, viewport_top);
        } else {
            Self::diff_render(
                buf,
                &self.previous_lines,
                &new_lines,
                height,
                viewport_top,
                self.previous_viewport_top,
            );
        }

        // Position hardware cursor (block style)
        if let Some((row, col)) = cursor_pos {
            let _ = write!(buf, "\x1b[{};{}H", row + 1, col + 1);
            buf.extend_from_slice(b"\x1b[2 q\x1b[?25h"); // steady block + show
        } else {
            buf.extend_from_slice(b"\x1b[?25l");
        }

        // Synchronized output end
        buf.extend_from_slice(b"\x1b[?2026l");

        self.terminal.write_bytes(buf);

        self.previous_lines = new_lines;
        self.previous_width = width;
        self.previous_height = height;
        self.max_lines_rendered = self.max_lines_rendered.max(self.previous_lines.len());
        self.previous_viewport_top = viewport_top;
    }

    fn full_render(buf: &mut Vec<u8>, lines: &[String], height: u16, viewport_top: usize) {
        // \x1b[H  — move to home (1,1)
        // \x1b[J  — erase from cursor to end of screen (ED 0)
        // We intentionally do NOT use \x1b[2J (erase entire display) because
        // many terminals (Terminal.app, iTerm2) also clear the scrollback
        // buffer when they receive ED 2.
        buf.extend_from_slice(b"\x1b[H\x1b[J");

        let visible = &lines[viewport_top..];
        for (i, line) in visible.iter().take(height as usize).enumerate() {
            if i > 0 {
                buf.extend_from_slice(b"\r\n");
            }
            buf.extend_from_slice(line.as_bytes());
        }
    }

    fn diff_render(
        buf: &mut Vec<u8>,
        old_lines: &[String],
        new_lines: &[String],
        height: u16,
        viewport_top: usize,
        old_top: usize,
    ) {
        let h = height as usize;

        // Viewport shift → full redraw
        if viewport_top != old_top {
            Self::full_render(buf, new_lines, height, viewport_top);
            return;
        }

        for row in 0..h {
            let new_idx = viewport_top + row;
            let old_idx = old_top + row;
            let new_line = new_lines.get(new_idx).map(|s| s.as_str()).unwrap_or("");
            let old_line = old_lines.get(old_idx).map(|s| s.as_str()).unwrap_or("");

            if new_line == old_line {
                continue;
            }

            // Find common byte prefix
            let common = common_prefix_len(old_line, new_line);

            if common == 0 || old_line.is_empty() {
                // No common prefix or old was empty — rewrite full line
                let _ = write!(buf, "\x1b[{};1H\x1b[2K", row + 1);
                buf.extend_from_slice(new_line.as_bytes());
            } else if new_line.len() > old_line.len()
                && new_line.as_bytes().starts_with(old_line.as_bytes())
            {
                // Old is a prefix of new (streaming append) — just emit the tail.
                let old_content = strip_line_suffix(old_line);
                let col = visible_width(old_content);
                let _ = write!(buf, "\x1b[{};{}H", row + 1, col + 1);
                // Re-emit active ANSI state so appended text is styled correctly
                buf.extend_from_slice(last_sgr(old_content).as_bytes());
                let new_content = strip_line_suffix(new_line);
                if new_content.len() > old_content.len() {
                    buf.extend_from_slice(&new_content.as_bytes()[old_content.len()..]);
                }
                buf.extend_from_slice(b"\x1b[0m\x1b]8;;\x07");
            } else {
                // General case: common prefix, then divergence.
                let prefix = &new_line[..common];
                let col = visible_width(prefix);
                let _ = write!(buf, "\x1b[{};{}H\x1b[K", row + 1, col + 1);
                // Re-emit active ANSI state from the common prefix
                buf.extend_from_slice(last_sgr(prefix).as_bytes());
                buf.extend_from_slice(&new_line.as_bytes()[common..]);
            }
        }
    }
}

/// Find the length of the common byte prefix between two strings.
/// Backs up past any partial UTF-8 sequence or ANSI escape to avoid
/// emitting fragments the terminal interprets as plaintext.
fn common_prefix_len(a: &str, b: &str) -> usize {
    let ab = a.as_bytes();
    let bb = b.as_bytes();
    let limit = ab.len().min(bb.len());
    let mut i = 0;
    while i < limit && ab[i] == bb[i] {
        i += 1;
    }
    // Back up to a char boundary
    while i > 0 && (!a.is_char_boundary(i) || !b.is_char_boundary(i)) {
        i -= 1;
    }
    // Back up past any partial ANSI escape sequence.
    // Scan backwards for ESC (0x1B). If found, check whether the escape
    // is complete (terminated by a letter in 0x40..0x7E for CSI, or BEL/ST for OSC/APC).
    if i > 0 {
        let prefix = &ab[..i];
        if let Some(esc_pos) = memrchr(0x1B, prefix)
            && !escape_complete(&ab[esc_pos..i])
        {
            i = esc_pos;
        }
    }
    i
}

/// Find last occurrence of byte in slice.
fn memrchr(needle: u8, haystack: &[u8]) -> Option<usize> {
    haystack.iter().rposition(|&b| b == needle)
}

/// Check if the bytes starting at ESC form a complete escape sequence.
fn escape_complete(seq: &[u8]) -> bool {
    if seq.len() < 2 || seq[0] != 0x1B {
        return false;
    }
    match seq[1] {
        b'[' => {
            // CSI: terminated by 0x40..0x7E
            seq[2..].iter().any(|&b| (0x40..=0x7E).contains(&b))
        }
        b']' => {
            // OSC: terminated by BEL (0x07) or ST (ESC \)
            seq[2..].contains(&0x07) || seq.windows(2).any(|w| w == [0x1B, b'\\'])
        }
        b'_' => {
            // APC: terminated by BEL or ST
            seq[2..].contains(&0x07) || seq.windows(2).any(|w| w == [0x1B, b'\\'])
        }
        _ => true, // two-byte sequences (ESC + one char) are always complete
    }
}

/// Extract the last active SGR sequence from a string.
/// Returns the accumulated SGR state to re-emit before appending text.
fn last_sgr(s: &str) -> String {
    let mut result = String::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1B && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            // Find the end of this CSI sequence
            let start = i;
            i += 2;
            while i < bytes.len() && !((0x40..=0x7E).contains(&bytes[i])) {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'm' {
                let seq = &s[start..=i];
                // Check for reset
                let params = &s[start + 2..i];
                if params.is_empty() || params == "0" {
                    result.clear();
                } else {
                    result.push_str(seq);
                }
            }
            i += 1;
        } else {
            i += 1;
        }
    }
    result
}

/// Strip the trailing `\x1b[0m\x1b]8;;\x07` suffix we append to every line.
fn strip_line_suffix(s: &str) -> &str {
    const SUFFIX: &str = "\x1b[0m\x1b]8;;\x07";
    s.strip_suffix(SUFFIX).unwrap_or(s)
}
