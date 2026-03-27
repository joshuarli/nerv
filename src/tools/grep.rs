use std::path::PathBuf;
use std::process::Command;

use super::truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, truncate_tail};
use crate::agent::agent::{AgentTool, ToolResult, UpdateCallback};
use crate::errors::ToolError;

pub struct GrepTool {
    cwd: PathBuf,
}
impl GrepTool {
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

impl AgentTool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }
    fn description(&self) -> &str {
        "Search file contents using ripgrep."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"},"include":{"type":"string"}},"required":["pattern"]})
    }
    fn validate(&self, input: &serde_json::Value) -> Result<(), ToolError> {
        if input.get("pattern").and_then(|v| v.as_str()).is_none() {
            return Err(ToolError::InvalidArguments {
                message: "pattern is required".into(),
            });
        }
        Ok(())
    }
    fn execute(&self, input: serde_json::Value, _update: UpdateCallback) -> ToolResult {
        let pattern = input["pattern"].as_str().unwrap_or("");
        let path = input["path"].as_str().unwrap_or(".");
        let resolved_path = self.resolve_path(path);
        let include = input["include"].as_str();
        let mut cmd = Command::new("rg");
        cmd.arg("--no-heading")
            .arg("--line-number")
            .arg("--color=never")
            .arg("--context=3")
            .arg(pattern)
            .arg(&resolved_path)
            .current_dir(&self.cwd);
        if let Some(glob) = include {
            cmd.arg("--glob").arg(glob);
        }
        match cmd.output() {
            Ok(output) => {
                if output.stdout.is_empty() && !output.status.success() {
                    return ToolResult::ok("No matches found");
                }
                let tr = truncate_tail(&output.stdout, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES);
                let match_count = tr.content.lines().count();
                let display = if tr.truncated {
                    format!("{} matches (truncated)", match_count)
                } else {
                    format!("{} matches", match_count)
                };
                ToolResult::ok_with_details(
                    tr.content,
                    serde_json::json!({"truncated": tr.truncated, "display": display}),
                )
            }
            Err(e) => ToolResult::error(format!("Error running rg: {}", e)),
        }
    }
}
