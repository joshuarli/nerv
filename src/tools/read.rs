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
        "Read an entire file with line numbers. Use bash + sed for specific line ranges."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]})
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
        let abs_path = self.resolve_path(path_str);

        // Mtime cache: if file unchanged since last read, return short marker.
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

        match std::fs::read(&abs_path) {
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                let line_count = text.lines().count();

                let width = digit_width(line_count);
                let mut content = String::with_capacity(line_count * 40);
                for (i, line) in text.lines().enumerate() {
                    use std::fmt::Write;
                    if i > 0 {
                        content.push('\n');
                    }
                    let _ = write!(content, "{:>w$}\t{}", i + 1, line, w = width);
                }
                let (content, truncated) = truncate_head(&content, DEFAULT_MAX_LINES);
                let display = if line_count == 0 {
                    format!("{} (empty)", path_str)
                } else if truncated {
                    format!("{} ({} lines, truncated)", path_str, line_count)
                } else {
                    format!("{} ({} lines)", path_str, line_count)
                };

                // Update cache
                if let Ok(meta) = std::fs::metadata(&abs_path) {
                    if let Ok(mtime) = meta.modified() {
                        if let Ok(mut cache) = self.cache.lock() {
                            cache.insert(
                                abs_path,
                                ReadCacheEntry { mtime, line_count },
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

    #[test]
    fn no_offset_or_limit_params() {
        // The schema should not expose offset or limit
        let tool = ReadTool::new(PathBuf::from("."));
        let schema = tool.parameters_schema();
        let props = schema.get("properties").unwrap();
        assert!(props.get("offset").is_none(), "offset should not be in schema");
        assert!(props.get("limit").is_none(), "limit should not be in schema");
    }
}
