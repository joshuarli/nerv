use std::path::PathBuf;

pub const DEFAULT_MAX_BYTES: usize = 200_000;
pub const DEFAULT_MAX_LINES: usize = 3_000;

pub struct TruncationResult {
    pub content: String,
    pub truncated: bool,
    pub original_bytes: usize,
    pub full_output_path: Option<PathBuf>,
}

/// Truncate keeping the tail (last N bytes / lines).
pub fn truncate_tail(data: &[u8], max_bytes: usize, max_lines: usize) -> TruncationResult {
    let original_bytes = data.len();
    let text = String::from_utf8_lossy(data);

    if data.len() <= max_bytes {
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() <= max_lines {
            return TruncationResult {
                content: text.into_owned(),
                truncated: false,
                original_bytes,
                full_output_path: None,
            };
        }

        // Truncate by lines
        let kept: Vec<&str> = lines[lines.len() - max_lines..].to_vec();
        let omitted = lines.len() - max_lines;
        let content = format!(
            "[{} lines omitted, showing last {}]\n{}",
            omitted,
            max_lines,
            kept.join("\n")
        );
        return TruncationResult {
            content,
            truncated: true,
            original_bytes,
            full_output_path: None,
        };
    }

    // Truncate by bytes
    let start = data.len() - max_bytes;
    // Find a UTF-8 boundary
    let start = (start..data.len())
        .find(|&i| std::str::from_utf8(&data[i..]).is_ok())
        .unwrap_or(start);

    let tail = String::from_utf8_lossy(&data[start..]);
    let omitted_bytes = start;
    let content = format!(
        "[{} bytes omitted, showing last {}]\n{}",
        omitted_bytes, max_bytes, tail
    );

    TruncationResult {
        content,
        truncated: true,
        original_bytes,
        full_output_path: None,
    }
}

/// Truncate keeping the head (first N lines).
pub fn truncate_head(text: &str, max_lines: usize) -> (String, bool) {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= max_lines {
        return (text.to_string(), false);
    }
    let kept = &lines[..max_lines];
    let omitted = lines.len() - max_lines;
    let content = format!("{}\n[{} more lines omitted]", kept.join("\n"), omitted);
    (content, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_truncation_needed() {
        let r = truncate_tail(b"hello\nworld", 100, 100);
        assert!(!r.truncated);
        assert_eq!(r.content, "hello\nworld");
    }

    #[test]
    fn truncate_by_lines() {
        let data = "a\nb\nc\nd\ne".as_bytes();
        let r = truncate_tail(data, 10000, 3);
        assert!(r.truncated);
        assert!(r.content.contains("c\nd\ne"));
        assert!(r.content.contains("2 lines omitted"));
    }

    #[test]
    fn truncate_by_bytes() {
        let data = vec![b'x'; 1000];
        let r = truncate_tail(&data, 100, 10000);
        assert!(r.truncated);
        assert!(r.content.contains("bytes omitted"));
    }

    #[test]
    fn head_truncation() {
        let (result, truncated) = truncate_head("a\nb\nc\nd\ne", 3);
        assert!(truncated);
        assert!(result.contains("a\nb\nc"));
        assert!(result.contains("2 more lines"));
    }
}
