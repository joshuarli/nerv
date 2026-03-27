use std::path::PathBuf;
use std::process::Command;

use super::truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, truncate_tail};
use crate::agent::agent::{AgentTool, ToolResult, UpdateCallback};
use crate::errors::ToolError;

pub struct LsTool {
    cwd: PathBuf,
}
impl LsTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }
}

impl AgentTool for LsTool {
    fn name(&self) -> &str {
        "ls"
    }
    fn description(&self) -> &str {
        "List directory contents using eza."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type":"object","properties":{"path":{"type":"string"}},"required":[]})
    }
    fn validate(&self, _input: &serde_json::Value) -> Result<(), ToolError> {
        Ok(())
    }
    fn execute(&self, input: serde_json::Value, _update: UpdateCallback) -> ToolResult {
        let path = input["path"].as_str().unwrap_or(".");
        match Command::new("eza")
            .arg("--tree")
            .arg("-L2")
            .arg("--icons=never")
            .arg(path)
            .current_dir(&self.cwd)
            .output()
        {
            Ok(output) => {
                let tr = truncate_tail(&output.stdout, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES);
                let display = format!("{} ({} entries)", path, tr.content.lines().count());
                ToolResult::ok_with_details(tr.content, serde_json::json!({"display": display}))
            }
            Err(e) => ToolResult::error(format!("Error running eza: {}", e)),
        }
    }
}
