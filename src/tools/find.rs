use std::path::PathBuf;
use std::process::Command;

use super::truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, truncate_tail};
use crate::agent::agent::{AgentTool, ToolResult, UpdateCallback};
use crate::errors::ToolError;

pub struct FindTool {
    cwd: PathBuf,
}
impl FindTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
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
            return Err(ToolError::InvalidArguments {
                message: "pattern is required".into(),
            });
        }
        Ok(())
    }
    fn execute(&self, input: serde_json::Value, _update: UpdateCallback) -> ToolResult {
        let pattern = input["pattern"].as_str().unwrap_or("");
        let path = input["path"].as_str().unwrap_or(".");
        match Command::new("fd")
            .arg("--glob")
            .arg(pattern)
            .arg(path)
            .current_dir(&self.cwd)
            .output()
        {
            Ok(output) => {
                let tr = truncate_tail(&output.stdout, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES);
                if tr.content.is_empty() {
                    ToolResult {
                        content: "No files found".into(),
                        details: None,
                        is_error: false,
                    }
                } else {
                    ToolResult {
                        content: tr.content,
                        details: None,
                        is_error: false,
                    }
                }
            }
            Err(e) => ToolResult {
                content: format!("Error running fd: {}", e),
                details: None,
                is_error: true,
            },
        }
    }
}
