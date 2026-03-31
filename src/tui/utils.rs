use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnsiState {
    Normal,
    Escape,
    Csi,
    Osc,
    Apc,
}

fn is_csi_final(b: u8) -> bool {
    (0x40..=0x7E).contains(&b)
}

fn is_osc_terminator(bytes: &[u8], i: usize) -> bool {
    if bytes[i] == 0x07 {
        return true;
    }
    // ST = ESC + backslash
    if bytes[i] == 0x5C && i > 0 && bytes[i - 1] == 0x1B {
        return true;
    }
    false
}

/// Display width of a string, skipping ANSI escape sequences.
pub fn visible_width(s: &str) -> u16 {
    let bytes = s.as_bytes();
    let mut width: u16 = 0;
    let mut state = AnsiState::Normal;
    let mut i = 0;
    let mut normal_start = 0;

    while i < bytes.len() {
        match state {
            AnsiState::Normal => {
                if bytes[i] == 0x1B {
                    // Measure the normal text we've accumulated
                    if normal_start < i {
                        let segment = &s[normal_start..i];
                        width = width.saturating_add(grapheme_width(segment));
                    }
                    state = AnsiState::Escape;
                    i += 1;
                } else {
                    i += 1;
                }
            }
            AnsiState::Escape => {
                match bytes[i] {
                    b'[' => {
                        state = AnsiState::Csi;
                        i += 1;
                    }
                    b']' => {
                        state = AnsiState::Osc;
                        i += 1;
                    }
                    b'_' => {
                        state = AnsiState::Apc;
                        i += 1;
                    }
                    // Two-byte sequences (e.g., ESC ( B)
                    _ => {
                        state = AnsiState::Normal;
                        i += 1;
                        normal_start = i;
                    }
                }
            }
            AnsiState::Csi => {
                if is_csi_final(bytes[i]) {
                    state = AnsiState::Normal;
                    i += 1;
                    normal_start = i;
                } else {
                    i += 1;
                }
            }
            AnsiState::Osc => {
                if is_osc_terminator(bytes, i) {
                    state = AnsiState::Normal;
                    i += 1;
                    normal_start = i;
                } else {
                    i += 1;
                }
            }
            AnsiState::Apc => {
                if bytes[i] == 0x07 || (bytes[i] == 0x5C && i > 0 && bytes[i - 1] == 0x1B) {
                    state = AnsiState::Normal;
                    i += 1;
                    normal_start = i;
                } else {
                    i += 1;
                }
            }
        }
    }

    // Remaining normal text
    if normal_start < bytes.len() && state == AnsiState::Normal {
        let segment = &s[normal_start..];
        width = width.saturating_add(grapheme_width(segment));
    }

    width
}

/// Width of a text segment (no ANSI) measured by grapheme clusters.
fn grapheme_width(s: &str) -> u16 {
    s.graphemes(true).map(|g| UnicodeWidthStr::width(g) as u16).sum()
}

/// Truncate to at most `max_width` visible columns, preserving ANSI escapes.
/// Appends "…" if truncated.
pub fn truncate_to_width(s: &str, max_width: u16) -> String {
    if visible_width(s) <= max_width {
        return s.to_string();
    }

    let ellipsis_width: u16 = 1; // "…" is 1 column
    let target = max_width.saturating_sub(ellipsis_width);
    let mut result = String::with_capacity(s.len());
    let mut width: u16 = 0;
    let mut state = AnsiState::Normal;
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut normal_start = 0;

    while i < bytes.len() {
        match state {
            AnsiState::Normal => {
                if bytes[i] == 0x1B {
                    // Flush accumulated normal text up to grapheme boundary
                    if normal_start < i {
                        let segment = &s[normal_start..i];
                        for g in segment.graphemes(true) {
                            let gw = UnicodeWidthStr::width(g) as u16;
                            if width + gw > target {
                                result.push('…');
                                return result;
                            }
                            result.push_str(g);
                            width += gw;
                        }
                    }
                    state = AnsiState::Escape;
                    normal_start = i;
                    i += 1;
                } else {
                    i += 1;
                }
            }
            AnsiState::Escape => match bytes[i] {
                b'[' => {
                    state = AnsiState::Csi;
                    i += 1;
                }
                b']' => {
                    state = AnsiState::Osc;
                    i += 1;
                }
                b'_' => {
                    state = AnsiState::Apc;
                    i += 1;
                }
                _ => {
                    result.push_str(&s[normal_start..=i]);
                    state = AnsiState::Normal;
                    i += 1;
                    normal_start = i;
                }
            },
            AnsiState::Csi => {
                if is_csi_final(bytes[i]) {
                    // Include the full escape sequence
                    result.push_str(&s[normal_start..=i]);
                    state = AnsiState::Normal;
                    i += 1;
                    normal_start = i;
                } else {
                    i += 1;
                }
            }
            AnsiState::Osc => {
                if is_osc_terminator(bytes, i) {
                    result.push_str(&s[normal_start..=i]);
                    state = AnsiState::Normal;
                    i += 1;
                    normal_start = i;
                } else {
                    i += 1;
                }
            }
            AnsiState::Apc => {
                if bytes[i] == 0x07 || (bytes[i] == 0x5C && i > 0 && bytes[i - 1] == 0x1B) {
                    result.push_str(&s[normal_start..=i]);
                    state = AnsiState::Normal;
                    i += 1;
                    normal_start = i;
                } else {
                    i += 1;
                }
            }
        }
    }

    // Remaining normal text
    if normal_start < bytes.len() && state == AnsiState::Normal {
        let segment = &s[normal_start..];
        for g in segment.graphemes(true) {
            let gw = UnicodeWidthStr::width(g) as u16;
            if width + gw > target {
                result.push('…');
                return result;
            }
            result.push_str(g);
            width += gw;
        }
    }

    result
}

/// Wrap a pre-styled string (may contain ANSI escapes) by character columns,
/// producing continuation lines when a line exceeds `width`. Used for code
/// blocks where word-wrapping would break indentation and syntax.
pub fn char_wrap_with_ansi(s: &str, width: u16) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let mut lines: Vec<String> = Vec::new();
    let mut current_line = String::new();
    let mut current_width: u16 = 0;
    let mut active_sgr = String::new();
    let bytes = s.as_bytes();
    let mut state = AnsiState::Normal;
    let mut i = 0;
    let mut normal_start = 0;

    let flush_char_segment = |segment: &str,
                               lines: &mut Vec<String>,
                               current_line: &mut String,
                               current_width: &mut u16,
                               active_sgr: &str| {
        for g in segment.graphemes(true) {
            let gw = UnicodeWidthStr::width(g) as u16;
            if *current_width + gw > width {
                current_line.push_str("\x1b[0m");
                lines.push(std::mem::take(current_line));
                current_line.push_str(active_sgr);
                *current_width = 0;
            }
            current_line.push_str(g);
            *current_width += gw;
        }
    };

    while i < bytes.len() {
        match state {
            AnsiState::Normal => {
                if bytes[i] == 0x1B {
                    if normal_start < i {
                        let segment = &s[normal_start..i];
                        flush_char_segment(
                            segment,
                            &mut lines,
                            &mut current_line,
                            &mut current_width,
                            &active_sgr,
                        );
                    }
                    state = AnsiState::Escape;
                    normal_start = i;
                    i += 1;
                } else {
                    i += 1;
                }
            }
            AnsiState::Escape => match bytes[i] {
                b'[' => {
                    state = AnsiState::Csi;
                    i += 1;
                }
                b']' => {
                    state = AnsiState::Osc;
                    i += 1;
                }
                b'_' => {
                    state = AnsiState::Apc;
                    i += 1;
                }
                _ => {
                    current_line.push_str(&s[normal_start..=i]);
                    state = AnsiState::Normal;
                    i += 1;
                    normal_start = i;
                }
            },
            AnsiState::Csi => {
                if is_csi_final(bytes[i]) {
                    let seq = &s[normal_start..=i];
                    current_line.push_str(seq);
                    if bytes[i] == b'm' {
                        update_sgr_state(&mut active_sgr, seq);
                    }
                    state = AnsiState::Normal;
                    i += 1;
                    normal_start = i;
                } else {
                    i += 1;
                }
            }
            AnsiState::Osc => {
                if is_osc_terminator(bytes, i) {
                    current_line.push_str(&s[normal_start..=i]);
                    state = AnsiState::Normal;
                    i += 1;
                    normal_start = i;
                } else {
                    i += 1;
                }
            }
            AnsiState::Apc => {
                if bytes[i] == 0x07 || (bytes[i] == 0x5C && i > 0 && bytes[i - 1] == 0x1B) {
                    current_line.push_str(&s[normal_start..=i]);
                    state = AnsiState::Normal;
                    i += 1;
                    normal_start = i;
                } else {
                    i += 1;
                }
            }
        }
    }

    if normal_start < bytes.len() && state == AnsiState::Normal {
        let segment = &s[normal_start..];
        flush_char_segment(
            segment,
            &mut lines,
            &mut current_line,
            &mut current_width,
            &active_sgr,
        );
    }

    lines.push(current_line);
    lines
}

/// Wrap text to lines of at most `width` columns, preserving ANSI state across
/// line breaks. Breaks at word boundaries (spaces); falls back to character-wrap
/// only for words wider than the column limit.
pub fn wrap_text_with_ansi(s: &str, width: u16) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }

    let mut lines: Vec<String> = Vec::new();
    let mut current_line = String::new();
    let mut current_width: u16 = 0;
    // Track active ANSI SGR state to carry across line breaks.
    let mut active_sgr = String::new();

    // Flush a plain-text segment with word-wrap. Words are split on spaces;
    // the space itself is emitted only when it fits on the current line.
    let flush_segment = |segment: &str,
                         lines: &mut Vec<String>,
                         current_line: &mut String,
                         current_width: &mut u16,
                         active_sgr: &str| {
        for part in segment.split('\n') {
            // Each `part` is a line of input separated by \n.
            // Split into space-delimited tokens; we'll re-join with spaces.
            let mut first_word = true;
            for word in part.split(' ') {
                let word_w = UnicodeWidthStr::width(word) as u16;

                if first_word {
                    first_word = false;
                    // Word fits on current line (possibly starting at col 0).
                    if *current_width + word_w <= width {
                        current_line.push_str(word);
                        *current_width += word_w;
                    } else if word_w > width {
                        // Word is wider than the line — character-wrap it.
                        for g in word.graphemes(true) {
                            let gw = UnicodeWidthStr::width(g) as u16;
                            if *current_width + gw > width {
                                current_line.push_str("\x1b[0m");
                                lines.push(std::mem::take(current_line));
                                current_line.push_str(active_sgr);
                                *current_width = 0;
                            }
                            current_line.push_str(g);
                            *current_width += gw;
                        }
                    } else {
                        // Wrap before the word.
                        current_line.push_str("\x1b[0m");
                        lines.push(std::mem::take(current_line));
                        current_line.push_str(active_sgr);
                        *current_width = 0;
                        current_line.push_str(word);
                        *current_width += word_w;
                    }
                } else {
                    // Subsequent words: prefix with a space.
                    // Space + word fits?
                    if *current_width + 1 + word_w <= width {
                        current_line.push(' ');
                        current_line.push_str(word);
                        *current_width += 1 + word_w;
                    } else {
                        // Wrap, then emit word without leading space.
                        current_line.push_str("\x1b[0m");
                        lines.push(std::mem::take(current_line));
                        current_line.push_str(active_sgr);
                        *current_width = 0;
                        if word_w > width {
                            // Character-wrap oversized word.
                            for g in word.graphemes(true) {
                                let gw = UnicodeWidthStr::width(g) as u16;
                                if *current_width + gw > width {
                                    current_line.push_str("\x1b[0m");
                                    lines.push(std::mem::take(current_line));
                                    current_line.push_str(active_sgr);
                                    *current_width = 0;
                                }
                                current_line.push_str(g);
                                *current_width += gw;
                            }
                        } else {
                            current_line.push_str(word);
                            *current_width += word_w;
                        }
                    }
                }
            }
            // Explicit newline from the input `\n` split — break here.
            current_line.push_str("\x1b[0m");
            lines.push(std::mem::take(current_line));
            current_line.push_str(active_sgr);
            *current_width = 0;
        }
        // Undo the final spurious newline added by the split loop above.
        // The last element of split('\n') corresponds to text *after* the last \n
        // (or the whole string when there is no \n). We always push a line for it
        // inside the loop, which means the last push was one too many.
        // Recover it as the in-progress line.
        *current_line = lines.pop().unwrap_or_default();
        // Re-apply active SGR prefix if we just replaced current_line.
        // (lines.pop() gave us the raw content without the SGR prefix the loop
        //  put there, because we push_str(active_sgr) *after* taking the line.
        //  Actually, the take() + active_sgr push means current_line already starts
        //  with active_sgr when we pop the last entry. The popped line is the last
        //  push from inside the loop, which was taken from current_line and is
        //  therefore correctly SGR-prefixed. We restore it verbatim.)
        // Recompute current_width from the restored line (strip ANSI for counting).
        *current_width = visible_width(current_line);
    };

    let bytes = s.as_bytes();
    let mut state = AnsiState::Normal;
    let mut i = 0;
    let mut normal_start = 0;

    while i < bytes.len() {
        match state {
            AnsiState::Normal => {
                if bytes[i] == 0x1B {
                    if normal_start < i {
                        let segment = &s[normal_start..i];
                        flush_segment(segment, &mut lines, &mut current_line, &mut current_width, &active_sgr);
                    }
                    state = AnsiState::Escape;
                    normal_start = i;
                    i += 1;
                } else {
                    i += 1;
                }
            }
            AnsiState::Escape => match bytes[i] {
                b'[' => { state = AnsiState::Csi; i += 1; }
                b']' => { state = AnsiState::Osc; i += 1; }
                b'_' => { state = AnsiState::Apc; i += 1; }
                _ => {
                    current_line.push_str(&s[normal_start..=i]);
                    state = AnsiState::Normal;
                    i += 1;
                    normal_start = i;
                }
            },
            AnsiState::Csi => {
                if is_csi_final(bytes[i]) {
                    let seq = &s[normal_start..=i];
                    current_line.push_str(seq);
                    if bytes[i] == b'm' {
                        update_sgr_state(&mut active_sgr, seq);
                    }
                    state = AnsiState::Normal;
                    i += 1;
                    normal_start = i;
                } else {
                    i += 1;
                }
            }
            AnsiState::Osc => {
                if is_osc_terminator(bytes, i) {
                    current_line.push_str(&s[normal_start..=i]);
                    state = AnsiState::Normal;
                    i += 1;
                    normal_start = i;
                } else {
                    i += 1;
                }
            }
            AnsiState::Apc => {
                if bytes[i] == 0x07 || (bytes[i] == 0x5C && i > 0 && bytes[i - 1] == 0x1B) {
                    current_line.push_str(&s[normal_start..=i]);
                    state = AnsiState::Normal;
                    i += 1;
                    normal_start = i;
                } else {
                    i += 1;
                }
            }
        }
    }

    // Flush any remaining normal text.
    if normal_start < bytes.len() && state == AnsiState::Normal {
        let segment = &s[normal_start..];
        flush_segment(segment, &mut lines, &mut current_line, &mut current_width, &active_sgr);
    }

    lines.push(current_line);
    lines
}

/// Update tracked SGR state. Reset on \x1b[0m, otherwise accumulate.
fn update_sgr_state(state: &mut String, seq: &str) {
    // Check for reset: \x1b[0m or \x1b[m
    let params = &seq[2..seq.len() - 1]; // strip \x1b[ and m
    if params.is_empty() || params == "0" {
        state.clear();
    } else {
        // Accumulate — not perfect, but good enough for common cases
        state.push_str(seq);
    }
}

/// Slice a string by column range [start, end), preserving ANSI escapes.
/// `strict`: if true, excludes a wide char that straddles the end boundary.
pub fn slice_by_column(s: &str, start: u16, end: u16, strict: bool) -> String {
    let mut result = String::new();
    let mut col: u16 = 0;
    let mut state = AnsiState::Normal;
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut normal_start = 0;

    while i < bytes.len() {
        match state {
            AnsiState::Normal => {
                if bytes[i] == 0x1B {
                    if normal_start < i {
                        let segment = &s[normal_start..i];
                        for g in segment.graphemes(true) {
                            let gw = UnicodeWidthStr::width(g) as u16;
                            if col + gw > start && col < end {
                                if strict && col + gw > end {
                                    // Wide char straddles end — skip
                                } else {
                                    result.push_str(g);
                                }
                            }
                            col += gw;
                        }
                    }
                    state = AnsiState::Escape;
                    normal_start = i;
                    i += 1;
                } else {
                    i += 1;
                }
            }
            AnsiState::Escape => match bytes[i] {
                b'[' => {
                    state = AnsiState::Csi;
                    i += 1;
                }
                b']' => {
                    state = AnsiState::Osc;
                    i += 1;
                }
                b'_' => {
                    state = AnsiState::Apc;
                    i += 1;
                }
                _ => {
                    result.push_str(&s[normal_start..=i]);
                    state = AnsiState::Normal;
                    i += 1;
                    normal_start = i;
                }
            },
            AnsiState::Csi => {
                if is_csi_final(bytes[i]) {
                    // Always include ANSI sequences
                    result.push_str(&s[normal_start..=i]);
                    state = AnsiState::Normal;
                    i += 1;
                    normal_start = i;
                } else {
                    i += 1;
                }
            }
            AnsiState::Osc => {
                if is_osc_terminator(bytes, i) {
                    result.push_str(&s[normal_start..=i]);
                    state = AnsiState::Normal;
                    i += 1;
                    normal_start = i;
                } else {
                    i += 1;
                }
            }
            AnsiState::Apc => {
                if bytes[i] == 0x07 || (bytes[i] == 0x5C && i > 0 && bytes[i - 1] == 0x1B) {
                    result.push_str(&s[normal_start..=i]);
                    state = AnsiState::Normal;
                    i += 1;
                    normal_start = i;
                } else {
                    i += 1;
                }
            }
        }
    }

    // Remaining normal text
    if normal_start < bytes.len() && state == AnsiState::Normal {
        let segment = &s[normal_start..];
        for g in segment.graphemes(true) {
            let gw = UnicodeWidthStr::width(g) as u16;
            if col + gw > start && col < end {
                if strict && col + gw > end {
                    // skip
                } else {
                    result.push_str(g);
                }
            }
            col += gw;
        }
    }

    result
}

/// Segments extracted from a string by column range.
pub struct Segments {
    pub before: String,
    pub between: String,
    pub after: String,
}

/// Extract (before, between, after) column segments in one pass.
pub fn extract_segments(s: &str, start: u16, end: u16, _after_hint: u16) -> Segments {
    Segments {
        before: slice_by_column(s, 0, start, false),
        between: slice_by_column(s, start, end, false),
        after: slice_by_column(s, end, u16::MAX, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_width() {
        assert_eq!(visible_width("hello"), 5);
        assert_eq!(visible_width(""), 0);
        assert_eq!(visible_width("abc"), 3);
    }

    #[test]
    fn ansi_escape_width() {
        assert_eq!(visible_width("\x1b[31mhello\x1b[0m"), 5);
        assert_eq!(visible_width("\x1b[1;34mtest\x1b[0m"), 4);
    }

    #[test]
    fn cjk_width() {
        // CJK characters are 2 columns wide
        assert_eq!(visible_width("你好"), 4);
        assert_eq!(visible_width("a你b"), 4);
    }

    #[test]
    fn truncate_short_string() {
        assert_eq!(truncate_to_width("hello", 10), "hello");
    }

    #[test]
    fn truncate_long_string() {
        assert_eq!(truncate_to_width("hello world", 6), "hello…");
    }

    #[test]
    fn truncate_with_ansi() {
        let s = "\x1b[31mhello world\x1b[0m";
        let result = truncate_to_width(s, 6);
        assert_eq!(visible_width(&result), 6);
        assert!(result.contains("…"));
    }

    #[test]
    fn wrap_simple() {
        let lines = wrap_text_with_ansi("hello world", 5);
        assert_eq!(lines.len(), 3);
        assert_eq!(visible_width(&lines[0]), 5);
    }

    #[test]
    fn wrap_newlines() {
        let lines = wrap_text_with_ansi("a\nb", 80);
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn slice_columns() {
        let result = slice_by_column("hello world", 6, 11, false);
        assert_eq!(result, "world");
    }

    #[test]
    fn cursor_marker_zero_width() {
        let marker = "\x1b_pi:c\x07";
        assert_eq!(visible_width(marker), 0);
        assert_eq!(visible_width(&format!("hello{marker}world")), 10);
    }
}
