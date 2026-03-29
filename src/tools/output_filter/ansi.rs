/// Strip ANSI escape sequences and bare carriage returns from text.
///
/// Removes:
/// - CSI sequences: ESC [ ... final-byte  (colors, cursor movement, etc.)
/// - OSC sequences: ESC ] ... ST/BEL      (window title, hyperlinks, etc.)
/// - Other ESC-X two-byte sequences
/// - Bare \r (progress-bar overwrite lines)
///
/// Returns a `Cow::Borrowed` when the input contains no escape sequences or
/// carriage returns, avoiding any allocation on the common clean-input path.
pub fn strip_ansi(text: &str) -> std::borrow::Cow<'_, str> {
    let bytes = text.as_bytes();

    // Fast path: scan for ESC or \r before touching an allocator.
    let needs_strip = bytes.iter().any(|&b| b == 0x1b || b == b'\r');
    if !needs_strip {
        return std::borrow::Cow::Borrowed(text);
    }

    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            // ESC starts an escape sequence
            0x1b => {
                i += 1;
                if i >= bytes.len() {
                    break;
                }
                match bytes[i] {
                    // CSI: ESC [ ... <final 0x40-0x7E>
                    b'[' => {
                        i += 1;
                        while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                            i += 1;
                        }
                        i += 1; // consume final byte
                    }
                    // OSC: ESC ] ... ST (ESC \) or BEL (0x07)
                    b']' => {
                        i += 1;
                        while i < bytes.len() {
                            if bytes[i] == 0x07 {
                                i += 1;
                                break;
                            }
                            if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                                i += 2;
                                break;
                            }
                            i += 1;
                        }
                    }
                    // All other ESC-X: skip the second byte
                    _ => {
                        i += 1;
                    }
                }
            }
            // Carriage return: skip (progress-bar lines write \r to overwrite)
            b'\r' => {
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }

    // SAFETY: we only ever copy bytes from a valid UTF-8 string unchanged,
    // and ESC sequences / \r are ASCII so stripping them cannot split a
    // multi-byte code-point.  from_utf8 should never fail here; we use the
    // lossy variant only as a safety net.
    match std::str::from_utf8(&out) {
        Ok(s) => std::borrow::Cow::Owned(s.to_owned()),
        Err(_) => std::borrow::Cow::Owned(String::from_utf8_lossy(&out).into_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_color_codes() {
        assert_eq!(strip_ansi("\x1b[32mgreen\x1b[0m"), "green");
    }

    #[test]
    fn strips_osc_hyperlink() {
        let s = "\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\";
        assert_eq!(strip_ansi(s), "link");
    }

    #[test]
    fn strips_osc_bel_terminated() {
        // OSC terminated with BEL (0x07) instead of ST
        let s = "\x1b]0;window title\x07plain";
        assert_eq!(strip_ansi(s), "plain");
    }

    #[test]
    fn strips_carriage_return() {
        assert_eq!(strip_ansi("foo\rbar"), "foobar");
    }

    #[test]
    fn strips_crlf_to_lf() {
        // \r\n → \n (the \r is dropped, \n is kept)
        assert_eq!(strip_ansi("line1\r\nline2\r\n"), "line1\nline2\n");
    }

    #[test]
    fn plain_text_unchanged_borrowed() {
        let s = "hello world\n";
        // Should return Borrowed — no allocation
        assert!(matches!(strip_ansi(s), std::borrow::Cow::Borrowed(_)));
        assert_eq!(strip_ansi(s), "hello world\n");
    }

    #[test]
    fn cargo_progress_bar() {
        // Typical cargo build line with bold + color reset
        let s = "\x1b[1m\x1b[32mCompiling\x1b[0m nerv v0.1.0";
        assert_eq!(strip_ansi(s), "Compiling nerv v0.1.0");
    }

    #[test]
    fn bare_esc_no_second_byte() {
        // ESC at very end of input — should not panic
        let s = "text\x1b";
        assert_eq!(strip_ansi(s), "text");
    }

    #[test]
    fn other_esc_two_byte() {
        // ESC M (reverse index) — two-byte sequence, skip both
        let s = "a\x1bMb";
        assert_eq!(strip_ansi(s), "ab");
    }

    #[test]
    fn multibyte_utf8_preserved() {
        // Japanese text with ANSI colour — multi-byte chars must not be mangled
        let s = "\x1b[31m日本語\x1b[0m";
        assert_eq!(strip_ansi(s), "日本語");
    }

    #[test]
    fn empty_string() {
        assert_eq!(strip_ansi(""), "");
    }
}
