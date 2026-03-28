use std::path::PathBuf;
use std::process::Command;

use super::truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, truncate_tail};
use crate::agent::agent::{AgentTool, ToolResult, UpdateCallback};
use crate::agent::provider::CancelFlag;
use crate::errors::ToolError;

pub struct FindTool {
    cwd: PathBuf,
}
impl FindTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }
    fn resolve_path(&self, path: &str) -> String {
        // Expand ~ at the start of the path
        if let Some(rest) = path.strip_prefix('~') {
            if let Some(home) = crate::home_dir() {
                return home.join(rest.trim_start_matches('/')).to_string_lossy().to_string();
            }
        }
        if path.starts_with('/') {
            path.to_string()
        } else {
            self.cwd.join(path).to_string_lossy().to_string()
        }
    }
}

impl AgentTool for FindTool {
    fn name(&self) -> &str {
        "find"
    }
    fn description(&self) -> &str {
        "Find files by name pattern using fd."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"}},"required":["pattern"]})
    }
    fn validate(&self, input: &serde_json::Value) -> Result<(), ToolError> {
        if input.get("pattern").and_then(|v| v.as_str()).is_none() {
            let keys: Vec<&str> = input.as_object().map(|m| m.keys().map(|s| s.as_str()).collect()).unwrap_or_default();
            return Err(ToolError::InvalidArguments {
                message: format!("pattern (string) is required (got keys: {})", keys.join(", ")),
            });
        }
        Ok(())
    }
    fn execute(&self, input: serde_json::Value, _update: UpdateCallback, _cancel: &CancelFlag) -> ToolResult {
        let pattern = input["pattern"].as_str().unwrap_or("");
        let path = input["path"].as_str().unwrap_or(".");
        let resolved_path = self.resolve_path(path);
        match Command::new("fd")
            .arg("--color=never")
            .arg("--show-errors")
            .arg("--glob")
            .arg(pattern)
            .arg(&resolved_path)
            .current_dir(&self.cwd)
            .output()
        {
            Ok(output) => {
                let tr = truncate_tail(&output.stdout, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let mut content = tr.content;
                if !stderr.trim().is_empty() {
                    if !content.is_empty() {
                        content.push('\n');
                    }
                    content.push_str("[stderr]\n");
                    content.push_str(stderr.trim());
                }
                if content.trim().is_empty() {
                    ToolResult::ok("No files found")
                } else {
                    let file_count = content.lines()
                        .filter(|l| !l.starts_with("[stderr]") && !l.is_empty())
                        .count();
                    let display = format!("{} files", file_count);
                    ToolResult::ok_with_details(content, serde_json::json!({"display": display}))
                }
            }
            Err(e) => ToolResult::error(format!("Error running fd: {}", e)),
        }
    }
}
