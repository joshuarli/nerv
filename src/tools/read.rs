use std::path::{Path, PathBuf};

use super::truncate::{DEFAULT_MAX_LINES, truncate_head};
use crate::agent::agent::{AgentTool, ToolResult, UpdateCallback};
use crate::errors::ToolError;

pub struct ReadTool {
    cwd: PathBuf,
}

impl ReadTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
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
        let limit = input["limit"].as_u64().unwrap_or(DEFAULT_MAX_LINES as u64) as usize;
        let abs_path = self.resolve_path(path_str);
        match std::fs::read(&abs_path) {
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                let lines: Vec<&str> = text.lines().collect();
                let start = offset.min(lines.len());
                let end = (start + limit).min(lines.len());
                let numbered: Vec<String> = lines[start..end]
                    .iter()
                    .enumerate()
                    .map(|(i, line)| format!("{:>6}\t{}", start + i + 1, line))
                    .collect();
                let content = numbered.join("\n");
                let (content, _) = truncate_head(&content, DEFAULT_MAX_LINES);
                ToolResult {
                    content,
                    details: None,
                    is_error: false,
                }
            }
            Err(e) => ToolResult {
                content: format!("Error reading {}: {}", path_str, e),
                details: None,
                is_error: true,
            },
        }
    }
}
