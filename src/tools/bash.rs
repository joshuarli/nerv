use std::io::Read;
use std::path::PathBuf;
use std::process::Command;

use super::truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, truncate_tail};
use crate::agent::agent::{AgentTool, ToolResult, UpdateCallback};
use crate::errors::ToolError;

pub struct BashTool {
    cwd: PathBuf,
    shell: String,
}

impl BashTool {
    pub fn new(cwd: PathBuf) -> Self {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        Self { cwd, shell }
    }
}

impl AgentTool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }
    fn description(&self) -> &str {
        "Execute a bash command and return its output."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type":"object","properties":{"command":{"type":"string"},"timeout":{"type":"integer"}},"required":["command"]})
    }
    fn validate(&self, input: &serde_json::Value) -> Result<(), ToolError> {
        if input.get("command").and_then(|v| v.as_str()).is_none() {
            return Err(ToolError::InvalidArguments {
                message: "command is required".into(),
            });
        }
        Ok(())
    }
    fn execute(&self, input: serde_json::Value, update: UpdateCallback) -> ToolResult {
        let command = input["command"].as_str().unwrap_or("");
        let mut child = match Command::new(&self.shell)
            .arg("-c")
            .arg(command)
            .current_dir(&self.cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                return ToolResult {
                    content: format!("Failed to spawn: {}", e),
                    details: None,
                    is_error: true,
                };
            }
        };

        let mut output = Vec::new();
        if let Some(mut stdout) = child.stdout.take() {
            let mut buf = [0u8; 8192];
            loop {
                match stdout.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        output.extend_from_slice(&buf[..n]);
                        update(String::from_utf8_lossy(&buf[..n]).to_string());
                    }
                    Err(_) => break,
                }
            }
        }
        if let Some(mut stderr) = child.stderr.take() {
            let mut buf = Vec::new();
            let _ = stderr.read_to_end(&mut buf);
            if !buf.is_empty() {
                output.extend_from_slice(b"\n[stderr]\n");
                output.extend_from_slice(&buf);
            }
        }

        let status = child.wait().ok();
        let exit_code = status.and_then(|s| s.code());
        let tr = truncate_tail(&output, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES);
        let content = if exit_code != Some(0) {
            format!("{}\n[exit code: {}]", tr.content, exit_code.unwrap_or(-1))
        } else {
            tr.content
        };
        ToolResult {
            content,
            details: Some(serde_json::json!({"exit_code": exit_code, "truncated": tr.truncated})),
            is_error: exit_code != Some(0),
        }
    }
}
