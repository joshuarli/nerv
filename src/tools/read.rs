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

        // Check mtime cache: if file unchanged since last full read, return short marker.
        // Only applies to full-file reads (offset 0, no explicit limit).
        if offset == 0 && explicit_limit.is_none() {
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
                let line_count = text.lines().count();
                // For small files (< 300 lines), return the whole file even if
                // the model requested a smaller limit. Prevents multi-chunk reads.
                let limit = if line_count <= 300 && offset == 0 {
                    line_count
                } else {
                    explicit_limit.unwrap_or(DEFAULT_MAX_LINES)
                };
                let start = offset.min(line_count);
                let end = (start + limit).min(line_count);
                let n = end - start;
                // Build numbered output in one allocation
                let mut content = String::with_capacity(n * 40);
                for (i, line) in text.lines().skip(start).take(n).enumerate() {
                    use std::fmt::Write;
                    if i > 0 {
                        content.push('\n');
                    }
                    let _ = write!(content, "{:>6}\t{}", start + i + 1, line);
                }
                let (content, truncated) = truncate_head(&content, DEFAULT_MAX_LINES);
                let display = if n == 0 {
                    format!("{} (empty)", path_str)
                } else if truncated {
                    format!("{} (lines {}-{}, truncated)", path_str, start + 1, end)
                } else {
                    format!("{} ({} lines)", path_str, n)
                };

                // Update cache with this file's mtime (full reads only)
                if offset == 0 && explicit_limit.is_none() {
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
