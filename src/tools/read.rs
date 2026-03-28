use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use super::truncate::{DEFAULT_MAX_LINES, truncate_head};
use crate::agent::agent::{AgentTool, ToolResult, UpdateCallback};
use crate::errors::ToolError;

struct ReadCacheEntry {
    mtime: SystemTime,
    line_count: usize,
}

pub struct ReadTool {
    cwd: PathBuf,
    cache: Mutex<std::collections::HashMap<PathBuf, ReadCacheEntry>>,
}

impl ReadTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            cache: Mutex::new(std::collections::HashMap::new()),
        }
    }
    fn resolve_path(&self, path: &str) -> PathBuf {
        let p = Path::new(path);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.cwd.join(p)
        }
    }
}

impl AgentTool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }
    fn description(&self) -> &str {
        "Read a file with line numbers. Use offset/limit to read specific sections of large files."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "offset": {"type": "integer", "description": "Start line (1-based). Default: 1"},
                "limit": {"type": "integer", "description": "Max lines to return. Default: all"}
            },
            "required": ["path"]
        })
    }
    fn validate(&self, input: &serde_json::Value) -> Result<(), ToolError> {
        if input.get("path").and_then(|v| v.as_str()).is_none() {
            let keys: Vec<&str> = input.as_object().map(|m| m.keys().map(|s| s.as_str()).collect()).unwrap_or_default();
            return Err(ToolError::InvalidArguments {
                message: format!("path (string) is required (got keys: {})", keys.join(", ")),
            });
        }
        Ok(())
    }
    fn execute(&self, input: serde_json::Value, _update: UpdateCallback) -> ToolResult {
        let path_str = input["path"].as_str().unwrap_or("");
        let abs_path = self.resolve_path(path_str);
        let offset = input.get("offset").and_then(|v| v.as_u64()).map(|v| v as usize);
        let limit = input.get("limit").and_then(|v| v.as_u64()).map(|v| v as usize);
        let has_range = offset.is_some() || limit.is_some();

        // Mtime cache: if file unchanged since last read AND no range specified, return short marker.
        if !has_range {
            if let Ok(meta) = std::fs::metadata(&abs_path) {
                if let Ok(mtime) = meta.modified() {
                    if let Ok(cache) = self.cache.lock() {
                        if let Some(entry) = cache.get(&abs_path) {
                            if entry.mtime == mtime {
                                let msg = format!(
                                    "[unchanged since last read: {} ({} lines)]",
                                    path_str, entry.line_count
                                );
                                return ToolResult::ok_with_details(
                                    msg,
                                    serde_json::json!({"display": format!("{} (unchanged)", path_str)}),
                                );
                            }
                        }
                    }
                }
            }
        }

        match std::fs::read(&abs_path) {
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                let total_lines = text.lines().count();

                let start = offset.unwrap_or(1).max(1) - 1; // convert 1-based to 0-based
                let end = if let Some(lim) = limit {
                    (start + lim).min(total_lines)
                } else {
                    total_lines
                };

                let width = digit_width(total_lines);
                let shown = end.saturating_sub(start);
                let mut content = String::with_capacity(shown * 40);
                for (i, line) in text.lines().enumerate() {
                    if i < start {
                        continue;
                    }
                    if i >= end {
                        break;
                    }
                    use std::fmt::Write;
                    if !content.is_empty() {
                        content.push('\n');
                    }
                    let _ = write!(content, "{:>w$}\t{}", i + 1, line, w = width);
                }

                // Apply truncation only when no explicit range was given
                let (mut content, truncated) = if has_range {
                    (content, false)
                } else {
                    truncate_head(&content, DEFAULT_MAX_LINES)
                };

                // Ensure trailing newline
                if !content.is_empty() && !content.ends_with('\n') {
                    content.push('\n');
                }

                let display = if total_lines == 0 {
                    format!("{} (empty)", path_str)
                } else if has_range {
                    format!("{} (lines {}-{} of {})", path_str, start + 1, start + shown, total_lines)
                } else if truncated {
                    format!("{} ({} lines, truncated)", path_str, total_lines)
                } else {
                    format!("{} ({} lines)", path_str, total_lines)
                };

                // Update cache with total line count
                if let Ok(meta) = std::fs::metadata(&abs_path) {
                    if let Ok(mtime) = meta.modified() {
                        if let Ok(mut cache) = self.cache.lock() {
                            cache.insert(
                                abs_path,
                                ReadCacheEntry { mtime, line_count: total_lines },
                            );
                        }
                    }
                }

                ToolResult::ok_with_details(content, serde_json::json!({"display": display}))
            }
            Err(e) => {
                if e.kind() == std::io::ErrorKind::NotFound {
                    ToolResult::error(format!("File not found: {} (cwd: {})", path_str, self.cwd.display()))
                } else {
                    ToolResult::error(format!("Error reading {}: {}", path_str, e))
                }
            }
        }
    }
}

fn digit_width(line_count: usize) -> usize {
    if line_count < 1000 { 3 }
    else if line_count < 10000 { 4 }
    else { 6 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::agent::AgentTool;
    use std::sync::Arc;

    fn make_tool(dir: &Path) -> Arc<ReadTool> {
        Arc::new(ReadTool::new(dir.to_path_buf()))
    }

    fn read(tool: &dyn AgentTool, path: &str) -> ToolResult {
        let cb: UpdateCallback = Arc::new(|_| {});
        tool.execute(serde_json::json!({"path": path}), cb)
    }

    fn read_range(tool: &dyn AgentTool, path: &str, offset: Option<u64>, limit: Option<u64>) -> ToolResult {
        let cb: UpdateCallback = Arc::new(|_| {});
        let mut args = serde_json::json!({"path": path});
        if let Some(o) = offset {
            args["offset"] = serde_json::json!(o);
        }
        if let Some(l) = limit {
            args["limit"] = serde_json::json!(l);
        }
        tool.execute(args, cb)
    }

    #[test]
    fn reads_full_file_with_line_numbers() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "aaa\nbbb\nccc\n").unwrap();
        let tool = make_tool(dir.path());
        let r = read(tool.as_ref(), "f.txt");
        assert!(!r.is_error);
        assert!(r.content.contains("  1\taaa"));
        assert!(r.content.contains("  3\tccc"));
    }

    #[test]
    fn offset_and_limit() {
        let dir = tempfile::tempdir().unwrap();
        let content: String = (1..=20).map(|i| format!("line {}\n", i)).collect();
        std::fs::write(dir.path().join("f.txt"), &content).unwrap();
        let tool = make_tool(dir.path());

        let r = read_range(tool.as_ref(), "f.txt", Some(5), Some(3));
        assert!(!r.is_error);
        assert!(r.content.contains("line 5"), "should start at line 5: {}", r.content);
        assert!(r.content.contains("line 7"), "should include line 7: {}", r.content);
        assert!(!r.content.contains("line 4"), "should not include line 4");
        assert!(!r.content.contains("line 8"), "should not include line 8");
    }

    #[test]
    fn offset_only() {
        let dir = tempfile::tempdir().unwrap();
        let content: String = (1..=10).map(|i| format!("line {}\n", i)).collect();
        std::fs::write(dir.path().join("f.txt"), &content).unwrap();
        let tool = make_tool(dir.path());

        let r = read_range(tool.as_ref(), "f.txt", Some(8), None);
        assert!(!r.is_error);
        assert!(r.content.contains("line 8"));
        assert!(r.content.contains("line 10"));
        assert!(!r.content.contains("line 7"));
    }

    #[test]
    fn limit_only() {
        let dir = tempfile::tempdir().unwrap();
        let content: String = (1..=10).map(|i| format!("line {}\n", i)).collect();
        std::fs::write(dir.path().join("f.txt"), &content).unwrap();
        let tool = make_tool(dir.path());

        let r = read_range(tool.as_ref(), "f.txt", None, Some(3));
        assert!(!r.is_error);
        assert!(r.content.contains("line 1"));
        assert!(r.content.contains("line 3"));
        assert!(!r.content.contains("line 4"));
    }

    #[test]
    fn offset_beyond_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "a\nb\n").unwrap();
        let tool = make_tool(dir.path());

        let r = read_range(tool.as_ref(), "f.txt", Some(100), Some(5));
        assert!(!r.is_error);
        assert!(r.content.is_empty() || r.content.trim().is_empty());
    }

    #[test]
    fn range_read_bypasses_mtime_cache() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "hello\nworld\n").unwrap();
        let tool = make_tool(dir.path());

        // First full read populates cache
        let _ = read(tool.as_ref(), "f.txt");
        // Second full read should hit cache
        let r2 = read(tool.as_ref(), "f.txt");
        assert!(r2.content.contains("unchanged"));
        // Range read should NOT hit cache
        let r3 = read_range(tool.as_ref(), "f.txt", Some(1), Some(1));
        assert!(!r3.content.contains("unchanged"));
        assert!(r3.content.contains("hello"));
    }

    #[test]
    fn mtime_cache_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "hello\n").unwrap();
        let tool = make_tool(dir.path());

        let r1 = read(tool.as_ref(), "f.txt");
        assert!(r1.content.contains("hello"));

        let r2 = read(tool.as_ref(), "f.txt");
        assert!(r2.content.contains("unchanged"), "should cache: {}", r2.content);
    }

    #[test]
    fn mtime_cache_invalidates_on_edit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, "original\n").unwrap();
        let tool = make_tool(dir.path());

        let r1 = read(tool.as_ref(), "f.txt");
        assert!(r1.content.contains("original"));

        std::fs::write(&path, "modified\n").unwrap();
        let r2 = read(tool.as_ref(), "f.txt");
        assert!(r2.content.contains("modified"), "should return new content: {}", r2.content);
        assert!(!r2.content.contains("unchanged"));
    }

    #[test]
    fn compact_line_numbers() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "a\nb\n").unwrap();
        let tool = make_tool(dir.path());
        let r = read(tool.as_ref(), "f.txt");
        // 2 lines → width 3
        assert!(r.content.starts_with("  1\t"), "got: {:?}", &r.content[..10.min(r.content.len())]);
    }

    #[test]
    fn file_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let tool = make_tool(dir.path());
        let r = read(tool.as_ref(), "nope.txt");
        assert!(r.is_error);
        assert!(r.content.contains("not found"));
    }
}
