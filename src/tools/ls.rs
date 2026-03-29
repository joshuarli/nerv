use std::path::PathBuf;
use std::process::Command;

use super::truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, truncate_tail};
use crate::agent::agent::{AgentTool, ToolResult, UpdateCallback};
use crate::agent::provider::CancelFlag;
use crate::errors::ToolError;

pub struct LsTool {
    cwd: PathBuf,
}
impl LsTool {
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

impl AgentTool for LsTool {
    fn name(&self) -> &str {
        "ls"
    }
    fn description(&self) -> &str {
        "List directory contents as a tree."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type":"object","properties":{"path":{"type":"string","description":"Directory to list (default: cwd)"}},"required":[],"additionalProperties":false})
    }
    fn validate(&self, _input: &serde_json::Value) -> Result<(), ToolError> {
        Ok(())
    }
    fn execute(&self, input: serde_json::Value, _update: UpdateCallback, _cancel: &CancelFlag) -> ToolResult {
        let path = input["path"].as_str().unwrap_or(".");
        let resolved_path = self.resolve_path(path);

        // Prefer eza: tree output, respects .gitignore.
        let output = Command::new("eza")
            .arg("--tree")
            .arg("-L2")
            .arg("--icons=never")
            .arg("--color=never")
            .arg("--no-quotes")
            .arg("--git-ignore")
            .arg(&resolved_path)
            .current_dir(&self.cwd)
            .output();

        let (stdout_bytes, stderr_str) = match output {
            Ok(o) => (o.stdout, String::from_utf8_lossy(&o.stderr).into_owned()),
            Err(_) => {
                // eza not available — fall back to `find -maxdepth 2`
                match Command::new("find")
                    .arg(&resolved_path)
                    .arg("-maxdepth")
                    .arg("2")
                    .arg("-not")
                    .arg("-path")
                    .arg("*/.*")
                    .current_dir(&self.cwd)
                    .output()
                {
                    Ok(o) => (o.stdout, String::from_utf8_lossy(&o.stderr).into_owned()),
                    Err(e) => return ToolResult::error(format!("ls fallback failed: {}", e)),
                }
            }
        };

        let tr = truncate_tail(&stdout_bytes, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES);
        let mut content = tr.content;
        if !stderr_str.trim().is_empty() {
            if !content.is_empty() {
                content.push('\n');
            }
            content.push_str("[stderr]\n");
            content.push_str(stderr_str.trim());
        }
        let entry_count = content.lines()
            .filter(|l| !l.starts_with("[stderr]") && !l.is_empty())
            .count();
        let display = format!("{} ({} entries)", path, entry_count);
        ToolResult::ok_with_details(content, serde_json::json!({"display": display}))
    }
}
