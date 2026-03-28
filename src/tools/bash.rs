use std::io::Read;
use std::path::PathBuf;
use std::process::Command;

use super::truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, truncate_tail};
use crate::agent::agent::{AgentTool, ToolResult, UpdateCallback};
use crate::agent::provider::CancelFlag;
use crate::errors::ToolError;
use std::sync::atomic::Ordering;

pub struct BashTool {
    cwd: PathBuf,
    shell: String,
}

impl BashTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            shell: "/bin/bash".into(),
        }
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
            let keys: Vec<&str> = input.as_object().map(|m| m.keys().map(|s| s.as_str()).collect()).unwrap_or_default();
            return Err(ToolError::InvalidArguments {
                message: format!("command (string) is required (got keys: {})", keys.join(", ")),
            });
        }
        Ok(())
    }
    fn execute(&self, input: serde_json::Value, update: UpdateCallback, cancel: &CancelFlag) -> ToolResult {
        let command = input["command"].as_str().unwrap_or("");
        let mut child = match Command::new(&self.shell)
            .arg("-euo")
            .arg("pipefail")
            .arg("-c")
            .arg(command)
            .current_dir(&self.cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                return ToolResult::error(format!("Failed to spawn: {}", e));
            }
        };

        // Drain stderr on a background thread to avoid pipe deadlock:
        // if the child fills the stderr buffer while we're blocked reading
        // stdout, both sides stall forever.
        let stderr_thread = child.stderr.take().map(|mut stderr| {
            std::thread::Builder::new()
                .name("nerv-bash-stderr".into())
                .stack_size(64 * 1024)
                .spawn(move || {
                    let mut buf = Vec::new();
                    let _ = stderr.read_to_end(&mut buf);
                    buf
                })
                .expect("failed to spawn bash stderr thread")
        });

        let mut output = Vec::new();
        if let Some(mut stdout) = child.stdout.take() {
            let mut buf = [0u8; 8192];
            loop {
                // Check cancel flag — if set, kill the child and abort.
                if cancel.load(Ordering::Relaxed) {
                    let _ = child.kill();
                    return ToolResult::error("Interrupted");
                }
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
        if let Some(stderr_buf) = stderr_thread.and_then(|t| t.join().ok()) {
            if !stderr_buf.is_empty() {
                output.extend_from_slice(b"\n[stderr]\n");
                output.extend_from_slice(&stderr_buf);
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

        // Suppress display for bare sed/head/tail/awk file reads — the system prompt tells
        // agents these produce no TUI output, incentivising them to use the read tool instead.
        // Only suppress when there's no pipe (rg | head is fine; head file.rs is not).
        let is_text_reading = !command.contains('|') && {
            let c = command.trim_start();
            c.starts_with("sed ")
                || c.starts_with("head ")
                || c.starts_with("tail ")
                || c.starts_with("awk ")
                || c.contains(" sed ")
                || c.contains(" head ")
                || c.contains(" tail ")
                || c.contains(" awk ")
        };

        let line_count = content.lines().count();
        let display = if is_text_reading {
            // No display preview for text-reading tools — agent should use the read tool instead
            None
        } else if exit_code != Some(0) {
            Some(format!("exit {} ({} lines)", exit_code.unwrap_or(-1), line_count))
        } else if line_count > 5 {
            // Show first 3 lines + count for long output
            let preview: String = content.lines().take(3).collect::<Vec<_>>().join("\n");
            Some(format!("{}\n  ... ({} lines)", preview, line_count))
        } else {
            Some(content.clone())
        };

        let mut details = serde_json::json!({"exit_code": exit_code, "truncated": tr.truncated});
        if let Some(disp) = display {
            details["display"] = serde_json::json!(disp);
        }
        if exit_code != Some(0) {
            ToolResult { content, details: Some(details), is_error: true }
        } else {
            ToolResult::ok_with_details(content, details)
        }
    }
}
