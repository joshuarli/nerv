use std::path::PathBuf;
use std::process::Command;

use super::truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, truncate_tail};
use crate::agent::agent::{AgentTool, ToolResult};
use crate::agent::provider::CancelFlag;
use crate::agent::types::ToolDetails;
use crate::errors::ToolError;

pub struct FindTool {
    cwd: PathBuf,
}
impl FindTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }
    fn resolve_path(&self, path: &str) -> String {
        crate::resolve_path(path, &self.cwd).to_string_lossy().into_owned()
    }
}

impl AgentTool for FindTool {
    fn name(&self) -> &str {
        "find"
    }
    fn is_readonly(&self) -> bool { true }
    fn description(&self) -> &str {
        "Find files by name pattern using fd."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type":"object","properties":{"pattern":{"type":"string","description":"Glob pattern to match filenames, e.g. '*.rs'"},"path":{"type":"string","description":"Directory to search (default: cwd)"}},"required":["pattern"],"additionalProperties":false})
    }
    fn validate(&self, input: &serde_json::Value) -> Result<(), ToolError> {
        if input.get("pattern").and_then(|v| v.as_str()).is_none() {
            let keys: Vec<&str> = input
                .as_object()
                .map(|m| m.keys().map(|s| s.as_str()).collect())
                .unwrap_or_default();
            return Err(ToolError::InvalidArguments {
                message: format!("pattern (string) is required (got keys: {})", keys.join(", ")),
            });
        }
        Ok(())
    }
    fn execute(
        &self,
        input: serde_json::Value,
        _cancel: &CancelFlag,
    ) -> ToolResult {
        let pattern = input["pattern"].as_str().unwrap_or("");
        let path = input["path"].as_str().unwrap_or(".");
        let resolved_path = self.resolve_path(path);
        let fd = match crate::fd() {
            Some(p) => p,
            None => return ToolResult::ok("fd is not installed"),
        };
        match Command::new(fd)
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
                    let file_count = content
                        .lines()
                        .filter(|l| !l.starts_with("[stderr]") && !l.is_empty())
                        .count();
                    let display = format!("{} files", file_count);
                    ToolResult::ok_with_details(content, ToolDetails { display: Some(display), ..Default::default() })
                }
            }
            Err(e) => ToolResult::error(format!("Error running fd: {}", e)),
        }
    }
}
