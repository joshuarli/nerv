use std::path::PathBuf;

use crate::agent::agent::{AgentTool, ToolResult};
use crate::agent::provider::CancelFlag;
use crate::errors::ToolError;

pub struct WriteTool {
    cwd: PathBuf,
}

impl WriteTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }
    fn resolve_path(&self, path: &str) -> PathBuf {
        crate::resolve_path(path, &self.cwd)
    }
}

impl AgentTool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }
    fn description(&self) -> &str {
        "Write content to a file. Creates parent directories if needed."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type":"object","properties":{"path":{"type":"string","description":"Path to the file to write"},"content":{"type":"string","description":"Content to write"}},"required":["path","content"],"additionalProperties":false})
    }
    fn validate(&self, input: &serde_json::Value) -> Result<(), ToolError> {
        if input.get("path").and_then(|v| v.as_str()).is_none() {
            let keys: Vec<&str> = input
                .as_object()
                .map(|m| m.keys().map(|s| s.as_str()).collect())
                .unwrap_or_default();
            return Err(ToolError::InvalidArguments {
                message: format!("path (string) is required (got keys: {})", keys.join(", ")),
            });
        }
        if input.get("content").and_then(|v| v.as_str()).is_none() {
            let keys: Vec<&str> = input
                .as_object()
                .map(|m| m.keys().map(|s| s.as_str()).collect())
                .unwrap_or_default();
            return Err(ToolError::InvalidArguments {
                message: format!("content (string) is required (got keys: {})", keys.join(", ")),
            });
        }
        Ok(())
    }
    fn execute(&self, input: serde_json::Value, _cancel: &CancelFlag) -> ToolResult {
        let path_str = input["path"].as_str().unwrap_or("");
        let content = input["content"].as_str().unwrap_or("");
        let abs_path = self.resolve_path(path_str);
        if let Some(parent) = abs_path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            return ToolResult::error(format!("Error creating directories: {}", e));
        }
        match std::fs::write(&abs_path, content) {
            Ok(()) => ToolResult::ok(format!("Wrote {} bytes to {}", content.len(), path_str)),
            Err(e) => ToolResult::error(format!("Error writing {}: {}", path_str, e)),
        }
    }
}
