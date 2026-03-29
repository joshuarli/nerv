/// Collapse runs of consecutive duplicate lines into `line (×N)`.
///
/// Only collapses identical non-empty lines that repeat 3+ times in a row,
/// to avoid mangling normal output that happens to have two identical lines.
///
/// Writes directly into a pre-allocated `String` to avoid the intermediate
/// `Vec<String>` + `join` that the naive approach uses.  Returns
/// `Cow::Borrowed` when the input needs no changes (common fast path).
pub fn dedup_lines(text: &str) -> std::borrow::Cow<str> {
    // Fast path: scan for any 3+ run before touching the allocator.
    if !has_dedup_run(text) {
        return std::borrow::Cow::Borrowed(text);
    }

    let trailing_newline = text.ends_with('\n');
    // Capacity guess: deduplicated output is always <= input.
    let mut out = String::with_capacity(text.len());
    let mut prev: &str = "";
    let mut count: usize = 0;
    let mut first = true;

    for line in text.lines() {
        if line == prev {
            count += 1;
        } else {
            if !first {
                flush_into(&mut out, prev, count);
                out.push('\n');
            }
            prev = line;
            count = 1;
            first = false;
        }
    }
    if !first {
        flush_into(&mut out, prev, count);
    }
    if trailing_newline {
        out.push('\n');
    }

    std::borrow::Cow::Owned(out)
}

/// Returns true iff any non-empty line repeats 3+ times consecutively.
fn has_dedup_run(text: &str) -> bool {
    let mut prev = "";
    let mut count = 0usize;
    for line in text.lines() {
        if line.is_empty() {
            prev = "";
            count = 0;
            continue;
        }
        if line == prev {
            count += 1;
            if count >= 3 {
                return true;
            }
        } else {
            prev = line;
            count = 1;
        }
    }
    false
}

fn flush_into(out: &mut String, line: &str, count: usize) {
    if count >= 3 && !line.is_empty() {
        use std::fmt::Write as _;
        out.push_str(line);
        // Write " (×N)" directly into the String — avoids a format! allocation.
        let _ = write!(out, " (×{count})");
    } else {
        for i in 0..count {
            if i > 0 {
                out.push('\n');
            }
            out.push_str(line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_dedup_for_short_runs_borrowed() {
        let text = "a\na\nb\n";
        // Must return Borrowed — no allocation on unchanged input
        assert!(matches!(dedup_lines(text), std::borrow::Cow::Borrowed(_)));
        assert_eq!(dedup_lines(text), text);
    }

    #[test]
    fn exactly_two_not_deduped() {
        let text = "error: foo\nerror: foo\ndone\n";
        assert_eq!(dedup_lines(text), text, "2-run must not be collapsed");
    }

    #[test]
    fn exactly_three_deduped() {
        let text = "warn\nwarn\nwarn\ndone\n";
        let result = dedup_lines(text);
        assert!(result.contains("(×3)"), "got: {result}");
        assert!(!result.contains("warn\nwarn"), "got: {result}");
    }

    #[test]
    fn dedup_four() {
        let text = "error: foo\nerror: foo\nerror: foo\nerror: foo\ndone\n";
        let result = dedup_lines(text);
        assert!(result.contains("(×4)"), "got: {result}");
        assert!(!result.contains("error: foo\nerror: foo"), "got: {result}");
    }

    #[test]
    fn empty_lines_not_deduped() {
        let text = "\n\n\n\n";
        assert_eq!(dedup_lines(text), text, "empty lines should not be collapsed");
    }

    #[test]
    fn no_trailing_newline_preserved() {
        let text = "x\nx\nx";
        let result = dedup_lines(text);
        assert!(result.contains("(×3)"), "got: {result}");
        assert!(!result.ends_with('\n'), "no trailing newline in input means none in output");
    }

    #[test]
    fn run_at_end_of_input() {
        let text = "ok\nfail\nfail\nfail\n";
        let result = dedup_lines(text);
        assert!(result.contains("(×3)"), "got: {result}");
        assert!(result.contains("ok"), "got: {result}");
    }

    #[test]
    fn mixed_runs() {
        // Two separate runs — both should be collapsed independently
        let text = "a\na\na\nb\nb\nb\nb\nc\n";
        let result = dedup_lines(text);
        assert!(result.contains("a (×3)"), "got: {result}");
        assert!(result.contains("b (×4)"), "got: {result}");
        assert!(result.contains("c"), "got: {result}");
    }

    #[test]
    fn empty_input() {
        assert_eq!(dedup_lines(""), "");
    }
}
