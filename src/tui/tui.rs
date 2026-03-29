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
    /// All lines from the last rendered frame (chat + fixed UI).
    previous_lines: Vec<String>,
    render_requested: bool,
    stopped: bool,
    /// Reusable byte buffer — avoids allocation per frame.
    write_buf: Vec<u8>,
}

impl TUI {
    pub fn new(terminal: Box<dyn Terminal>) -> Self {
        Self {
            terminal,
            previous_lines: Vec::new(),
            render_requested: true,
            stopped: false,
            write_buf: Vec::with_capacity(8192),
        }
    }

    pub fn request_render(&mut self, full: bool) {
        self.render_requested = true;
        if full {
            // Clear previous frame so do_render takes the full-repaint path.
            self.previous_lines.clear();
        }
    }

    pub fn maybe_render(&mut self, root: &dyn Component, fixed_bottom_lines: usize) {
        if self.render_requested && !self.stopped {
            self.do_render(root, fixed_bottom_lines);
            self.render_requested = false;
        }
    }

    pub fn terminal(&self) -> &dyn Terminal {
        &*self.terminal
    }

    pub fn terminal_mut(&mut self) -> &mut dyn Terminal {
        &mut *self.terminal
    }

    pub fn width(&self) -> u16 {
        self.terminal.columns()
    }

    pub fn height(&self) -> u16 {
        self.terminal.rows()
    }

    pub fn dump_scrollback(&mut self, text: &str) {
        self.terminal.dump_scrollback(text);
        self.render_requested = true;
    }

    pub fn suspend(&mut self) {
        self.terminal.stop();
    }

    pub fn resume(&mut self) {
        self.terminal.restart();
        self.request_render(true); // force full repaint after resume
    }

    fn do_render(&mut self, root: &dyn Component, fixed_bottom_lines: usize) {
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

        // Append reset + hyperlink close to each non-empty line.
        for line in &mut new_lines {
            if !line.is_empty() {
                line.push_str("\x1b[0m\x1b]8;;\x07");
            }
        }

        let old_total = self.previous_lines.len();
        let new_total = new_lines.len();
        let old_top = old_total.saturating_sub(height as usize);

        self.write_buf.clear();
        let buf = &mut self.write_buf;

        buf.extend_from_slice(b"\x1b[?2026h"); // synchronized output begin

        if old_total == 0 || new_total < old_total {
            // First render or line count shrank: full repaint visible area
            Self::full_render(buf, &new_lines, height, viewport_top);
        } else if new_total > old_total {
            // Content grew: scroll new chat lines into scrollback, then update visible rows
            let chat_limit = new_lines.len() - fixed_bottom_lines;
            let new_chat_lines = viewport_top.min(chat_limit);
            if new_chat_lines > old_top {
                // Position at bottom row and write new lines, letting them scroll into history
                let _ = write!(buf, "\x1b[{};1H", height);
                for line in &new_lines[old_top..new_chat_lines] {
                    buf.extend_from_slice(b"\r\n");
                    buf.extend_from_slice(line.as_bytes());
                }
            }
            Self::diff_render(buf, &self.previous_lines, &new_lines, height, viewport_top, old_top);
        } else {
            // Content unchanged: just update rows that changed
            Self::diff_render(buf, &self.previous_lines, &new_lines, height, viewport_top, old_top);
        }

        // Position hardware cursor
        if let Some((row, col)) = cursor_pos {
            let _ = write!(buf, "\x1b[{};{}H", row + 1, col + 1);
            buf.extend_from_slice(b"\x1b[2 q\x1b[?25h"); // steady block + show
        } else {
            buf.extend_from_slice(b"\x1b[?25l");
        }

        buf.extend_from_slice(b"\x1b[?2026l"); // synchronized output end

        self.previous_lines = new_lines;
        self.terminal.write_bytes(buf);
    }

    fn full_render(buf: &mut Vec<u8>, lines: &[String], height: u16, viewport_top: usize) {
        let visible = &lines[viewport_top..];
        for (i, line) in visible.iter().take(height as usize).enumerate() {
            let _ = write!(buf, "\x1b[{};1H", i + 1);
            buf.extend_from_slice(line.as_bytes());
            buf.extend_from_slice(b"\x1b[K"); // clear to end of line
        }
        // Clear rows below content
        for i in visible.len()..height as usize {
            let _ = write!(buf, "\x1b[{};1H\x1b[K", i + 1);
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

        // Viewport shifted — fall back to full redraw.
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

            let common = common_prefix_len(old_line, new_line);

            if common == 0 || old_line.is_empty() {
                // Rewrite full line
                let _ = write!(buf, "\x1b[{};1H\x1b[2K", row + 1);
                buf.extend_from_slice(new_line.as_bytes());
            } else if new_line.len() > old_line.len()
                && new_line.as_bytes().starts_with(old_line.as_bytes())
            {
                // Old is a prefix of new (streaming append) — emit the tail only.
                let old_content = strip_line_suffix(old_line);
                let col = visible_width(old_content);
                let _ = write!(buf, "\x1b[{};{}H", row + 1, col + 1);
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
                buf.extend_from_slice(last_sgr(prefix).as_bytes());
                buf.extend_from_slice(&new_line.as_bytes()[common..]);
            }
        }
    }
}

/// Find the length of the common byte prefix between two strings,
/// snapped back to a char boundary and behind any partial ANSI escape.
fn common_prefix_len(a: &str, b: &str) -> usize {
    let ab = a.as_bytes();
    let bb = b.as_bytes();
    let limit = ab.len().min(bb.len());
    let mut i = 0;
    while i < limit && ab[i] == bb[i] {
        i += 1;
    }
    while i > 0 && (!a.is_char_boundary(i) || !b.is_char_boundary(i)) {
        i -= 1;
    }
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

fn memrchr(needle: u8, haystack: &[u8]) -> Option<usize> {
    haystack.iter().rposition(|&b| b == needle)
}

fn escape_complete(seq: &[u8]) -> bool {
    if seq.len() < 2 || seq[0] != 0x1B {
        return false;
    }
    match seq[1] {
        b'[' => seq[2..].iter().any(|&b| (0x40..=0x7E).contains(&b)),
        b']' | b'_' => {
            seq[2..].contains(&0x07) || seq.windows(2).any(|w| w == [0x1B, b'\\'])
        }
        _ => true,
    }
}

/// Extract the last active SGR sequence from a string, to re-emit before appending.
fn last_sgr(s: &str) -> String {
    let mut result = String::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1B && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            let start = i;
            i += 2;
            while i < bytes.len() && !((0x40..=0x7E).contains(&bytes[i])) {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'm' {
                let seq = &s[start..=i];
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

/// Strip the trailing `\x1b[0m\x1b]8;;\x07` suffix appended to every line.
fn strip_line_suffix(s: &str) -> &str {
    const SUFFIX: &str = "\x1b[0m\x1b]8;;\x07";
    s.strip_suffix(SUFFIX).unwrap_or(s)
}
