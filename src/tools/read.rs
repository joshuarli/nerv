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
    /// Cache of path → (mtime, line_count) for unchanged-file detection.
    cache: Mutex<std::collections::HashMap<PathBuf, ReadCacheEntry>>,
}

impl ReadTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            cache: Mutex::new(std::collections::HashMap::new()),
        }
    }
    fn update_cache(&self, abs_path: &Path, line_count: usize) {
        if let Ok(meta) = std::fs::metadata(abs_path) {
            if let Ok(mtime) = meta.modified() {
                if let Ok(mut cache) = self.cache.lock() {
                    cache.insert(
                        abs_path.to_path_buf(),
                        ReadCacheEntry { mtime, line_count },
                    );
                }
            }
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
        "Read file contents with line numbers."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type":"object","properties":{"path":{"type":"string"},"offset":{"type":"integer"},"limit":{"type":"integer"}},"required":["path"]})
    }
    fn validate(&self, input: &serde_json::Value) -> Result<(), ToolError> {
        if input.get("path").and_then(|v| v.as_str()).is_none() {
            return Err(ToolError::InvalidArguments {
                message: "path is required".into(),
            });
        }
        Ok(())
    }
    fn execute(&self, input: serde_json::Value, _update: UpdateCallback) -> ToolResult {
        let path_str = input["path"].as_str().unwrap_or("");
        let offset = input["offset"].as_u64().unwrap_or(0) as usize;
        let explicit_limit = input.get("limit").and_then(|v| v.as_u64()).map(|v| v as usize);
        let abs_path = self.resolve_path(path_str);

        // Check mtime cache. For files under AUTO_SIZE_LINES we always return the
        // full file (ignoring offset/limit), so the cache applies regardless of offset.
        // For larger files, only cache offset=0 reads.
        {
            if let Ok(meta) = std::fs::metadata(&abs_path) {
                if let Ok(mtime) = meta.modified() {
                    if let Ok(cache) = self.cache.lock() {
                        if let Some(entry) = cache.get(&abs_path) {
                            let full_file = offset == 0 || entry.line_count <= AUTO_SIZE_LINES;
                            if full_file && entry.mtime == mtime {
                                let msg = format!(
                                    "[unchanged since last read: {} ({} lines)]",
                                    path_str, entry.line_count
                                );
                                return ToolResult::ok_with_details(
                                    msg,
                                    serde_json::json!({"display": format!("{} (unchanged)", path_str)}),
                                );
                            }
                            // Changed or partial read of large file — fall through
                        }
                    }
                }
            }
        }

        match std::fs::read(&abs_path) {
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                let line_count = text.lines().count();
                let result = format_file_content(path_str, &text, offset, explicit_limit);

                // Update cache when we returned the full file
                if offset == 0 || line_count <= AUTO_SIZE_LINES {
                    self.update_cache(&abs_path, line_count);
                }

                result
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

/// Format file content with line numbers for the model.
/// Auto-size threshold: files under this many lines are always returned in full,
/// ignoring offset and limit. Most source files fit. Only generated code,
/// vendored deps, or data files should ever chunk.
const AUTO_SIZE_LINES: usize = 2000;

fn format_file_content(path_str: &str, text: &str, offset: usize, explicit_limit: Option<usize>) -> ToolResult {
    let line_count = text.lines().count();
    // For files under AUTO_SIZE_LINES, return the whole file regardless of
    // offset/limit. The model often sends small limits (120) out of habit,
    // causing 10+ chunked reads of a 1200-line file. Override that.
    let (start, limit) = if line_count <= AUTO_SIZE_LINES {
        (0, line_count)
    } else {
        (offset.min(line_count), explicit_limit.unwrap_or(DEFAULT_MAX_LINES))
    };
    let end = (start + limit).min(line_count);
    let n = end - start;

    let width = digit_width(line_count);
    let mut content = String::with_capacity(n * 40);
    for (i, line) in text.lines().skip(start).take(n).enumerate() {
        use std::fmt::Write;
        if i > 0 {
            content.push('\n');
        }
        let _ = write!(content, "{:>w$}\t{}", start + i + 1, line, w = width);
    }
    let (content, truncated) = truncate_head(&content, DEFAULT_MAX_LINES);
    let display = if n == 0 {
        format!("{} (empty)", path_str)
    } else if truncated {
        format!("{} (lines {}-{}, truncated)", path_str, start + 1, end)
    } else {
        format!("{} ({} lines)", path_str, n)
    };
    ToolResult::ok_with_details(content, serde_json::json!({"display": display}))
}

/// Minimum digit width for line numbers (e.g., 4 for files up to 9999 lines).
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

    fn read_call(tool: &dyn AgentTool, path: &str) -> ToolResult {
        let cb: UpdateCallback = Arc::new(|_| {});
        tool.execute(serde_json::json!({"path": path}), cb)
    }

    fn read_call_with_limit(tool: &dyn AgentTool, path: &str, limit: u64) -> ToolResult {
        let cb: UpdateCallback = Arc::new(|_| {});
        tool.execute(serde_json::json!({"path": path, "limit": limit}), cb)
    }

    #[test]
    fn mtime_cache_returns_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "line1\nline2\nline3\n").unwrap();

        let tool = make_tool(dir.path());

        // First read — should return full content
        let r1 = read_call(tool.as_ref(), "test.txt");
        assert!(!r1.is_error);
        assert!(r1.content.contains("line1"), "first read should have content");

        // Second read (same file, unchanged) — should return cache hit
        let r2 = read_call(tool.as_ref(), "test.txt");
        assert!(!r2.is_error);
        assert!(
            r2.content.contains("unchanged"),
            "second read of unchanged file should be cached: {}",
            r2.content
        );

        // Modify the file — cache should invalidate
        std::fs::write(&file, "modified\n").unwrap();
        let r3 = read_call(tool.as_ref(), "test.txt");
        assert!(!r3.is_error);
        assert!(
            r3.content.contains("modified"),
            "read after modification should return new content"
        );
    }

    #[test]
    fn mtime_cache_with_explicit_limit() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("small.txt");
        std::fs::write(&file, "a\nb\nc\n").unwrap();

        let tool = make_tool(dir.path());

        // First read with no limit
        let r1 = read_call(tool.as_ref(), "small.txt");
        assert!(r1.content.contains("a"));

        // Second read with explicit limit — should still hit cache
        let r2 = read_call_with_limit(tool.as_ref(), "small.txt", 2);
        assert!(
            r2.content.contains("unchanged"),
            "explicit limit should still hit cache for unchanged file: {}",
            r2.content
        );
    }

    #[test]
    fn auto_size_returns_full_small_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("small.py");
        let content = (0..50).map(|i| format!("line {}", i)).collect::<Vec<_>>().join("\n");
        std::fs::write(&file, &content).unwrap();

        let tool = make_tool(dir.path());

        // Read with limit=10 — should still get all 50 lines (auto-size)
        let r = read_call_with_limit(tool.as_ref(), "small.py", 10);
        assert!(!r.is_error);
        assert!(
            r.content.contains("line 49"),
            "auto-size should return full file for small files: last line missing"
        );
    }

    #[test]
    fn auto_size_ignores_offset_for_medium_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("medium.rs");
        // 500 lines — under AUTO_SIZE_LINES
        let content = (0..500).map(|i| format!("fn line_{}() {{}}", i)).collect::<Vec<_>>().join("\n");
        std::fs::write(&file, &content).unwrap();

        let tool = make_tool(dir.path());

        // Read with offset=200, limit=100 — should still get the full file
        let cb: UpdateCallback = Arc::new(|_| {});
        let r = tool.execute(serde_json::json!({"path": "medium.rs", "offset": 200, "limit": 100}), cb);
        assert!(!r.is_error);
        assert!(r.content.contains("line_0"), "should contain first line despite offset=200");
        assert!(r.content.contains("line_499"), "should contain last line despite limit=100");
    }

    #[test]
    fn cache_hits_on_offset_read_of_auto_sized_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("medium.rs");
        let content = (0..500).map(|i| format!("fn line_{}() {{}}", i)).collect::<Vec<_>>().join("\n");
        std::fs::write(&file, &content).unwrap();

        let tool = make_tool(dir.path());

        // First read — full file, populates cache
        let r1 = read_call(tool.as_ref(), "medium.rs");
        assert!(r1.content.contains("line_0"));

        // Second read with offset=200 — should hit cache (file unchanged, under auto-size)
        let cb: UpdateCallback = Arc::new(|_| {});
        let r2 = tool.execute(serde_json::json!({"path": "medium.rs", "offset": 200, "limit": 50}), cb);
        assert!(
            r2.content.contains("unchanged"),
            "offset read of unchanged auto-sized file should hit cache: {}",
            r2.content
        );
    }

    #[test]
    fn modified_file_returns_full_content() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("code.py");
        let original: String = (0..30).map(|i| format!("line {}\n", i)).collect();
        std::fs::write(&file, &original).unwrap();

        let tool = make_tool(dir.path());

        // First read — caches
        let r1 = read_call(tool.as_ref(), "code.py");
        assert!(r1.content.contains("line 0"));

        // Modify line 15
        let modified: String = (0..30)
            .map(|i| {
                if i == 15 { "MODIFIED\n".into() } else { format!("line {}\n", i) }
            })
            .collect();
        std::fs::write(&file, &modified).unwrap();

        // Second read — must return FULL new content (not cached, not partial)
        let r2 = read_call(tool.as_ref(), "code.py");
        assert!(!r2.is_error);
        assert!(r2.content.contains("MODIFIED"), "should contain modified line");
        assert!(r2.content.contains("line 0"), "should contain line 0");
        assert!(r2.content.contains("line 29"), "should contain line 29");
        assert!(!r2.content.contains("unchanged"), "should not say unchanged");
    }

    #[test]
    fn compact_line_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("short.txt");
        std::fs::write(&file, "hello\nworld\n").unwrap();

        let tool = make_tool(dir.path());
        let r = read_call(tool.as_ref(), "short.txt");
        // File has 2 lines → digit_width = 3, so format is "  1\t..."
        assert!(
            r.content.starts_with("  1\t"),
            "compact line numbers for small files: {:?}",
            &r.content[..20.min(r.content.len())]
        );
    }
}
