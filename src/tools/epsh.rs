use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::agent::agent::{AgentTool, ToolResult};
use crate::agent::provider::CancelFlag;
use crate::agent::types::ToolDetails;
use crate::errors::ToolError;
use crate::tools::output_filter;

/// Text-reading commands whose display output is suppressed to nudge the
/// agent toward the dedicated read tool.
const TEXT_READING_COMMANDS: &[&str] = &["sed", "head", "tail", "awk"];

/// Hard cap on output bytes. When exceeded, the shell is cancelled to prevent
/// runaway commands from bloating memory. Set above the output gate threshold
/// (50KB) so the normal gate handles moderate cases; this catches extremes.
const OUTPUT_CAP_BYTES: usize = 1_024 * 1_024; // 1 MB

/// A Write adapter that stops accepting bytes after a limit and signals
/// cancellation via an AtomicBool.
struct CappedBuffer {
    buf: Vec<u8>,
    cap: usize,
    cancel: Arc<AtomicBool>,
    capped: bool,
}

impl CappedBuffer {
    fn new(cap: usize, cancel: Arc<AtomicBool>) -> Self {
        Self { buf: Vec::new(), cap, cancel, capped: false }
    }
}

impl Write for CappedBuffer {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if self.capped {
            return Ok(data.len()); // Silently discard.
        }
        let remaining = self.cap.saturating_sub(self.buf.len());
        if remaining == 0 {
            self.capped = true;
            self.cancel.store(true, Ordering::Relaxed);
            return Ok(data.len());
        }
        let take = data.len().min(remaining);
        self.buf.extend_from_slice(&data[..take]);
        if take < data.len() {
            self.capped = true;
            self.cancel.store(true, Ordering::Relaxed);
        }
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub struct EpshTool {
    cwd: PathBuf,
}

impl EpshTool {
    pub fn new(cwd: PathBuf) -> Self {
        // Anchor the shell to the git root so the model's relative paths always
        // resolve correctly regardless of which subdirectory nerv was launched from.
        let cwd = crate::find_repo_root(&cwd).unwrap_or(cwd);
        Self { cwd }
    }
}

impl AgentTool for EpshTool {
    fn name(&self) -> &str {
        "epsh"
    }
    fn description(&self) -> &str {
        "Execute a POSIX shell command and return its output."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "POSIX shell command to execute"
                },
                "timeout": {
                    "type": "integer",
                    "description": "Timeout in seconds (default: 120, max: 600)"
                }
            },
            "required": ["command"],
            "additionalProperties": false
        })
    }
    fn prompt_guidelines(&self) -> Vec<String> {
        vec![
            "Commands run in a POSIX shell. Bash extensions are not available: no [[ ]], no arrays, no process substitution <(), no brace expansion {a,b}, no <<<.".into(),
            // The shell is always spawned with cwd set to the project git root — never use `cd`.
            // `cd` to the project root or any absolute path is wrong and wastes a round-trip.
            format!(
                "`epsh` always starts in the project root (`{}`). Never `cd` — use relative paths.",
                self.cwd.display()
            ),
        ]
    }
    fn validate(&self, input: &serde_json::Value) -> Result<(), ToolError> {
        if input.get("command").and_then(|v| v.as_str()).is_none() {
            let keys: Vec<&str> = input
                .as_object()
                .map(|m| m.keys().map(|s| s.as_str()).collect())
                .unwrap_or_default();
            return Err(ToolError::InvalidArguments {
                message: format!("command (string) is required (got keys: {})", keys.join(", ")),
            });
        }
        Ok(())
    }
    fn execute(&self, input: serde_json::Value, cancel: &CancelFlag) -> ToolResult {
        let command = input["command"].as_str().unwrap_or("");
        let timeout_secs = input.get("timeout")
            .and_then(|v| v.as_u64())
            .unwrap_or(120)
            .min(600);

        // Parse first -- syntax errors are caught before any execution.
        let mut parser = epsh::parser::Parser::new(command);
        let program = match parser.parse() {
            Ok(p) => p,
            Err(e) => return ToolResult::error(format!("syntax error: {e}")),
        };

        let is_text_reading = is_bare_text_reader(&program);

        // Output capture via capped sinks. Cancels the shell if output exceeds
        // OUTPUT_CAP_BYTES to prevent runaway commands from bloating memory.
        let stdout_buf: Arc<Mutex<CappedBuffer>> =
            Arc::new(Mutex::new(CappedBuffer::new(OUTPUT_CAP_BYTES, Arc::clone(cancel))));
        let stderr_buf: Arc<Mutex<CappedBuffer>> =
            Arc::new(Mutex::new(CappedBuffer::new(OUTPUT_CAP_BYTES, Arc::clone(cancel))));

        // Fresh shell per invocation. Builder inherits process env by default.
        let mut shell = epsh::eval::Shell::builder()
            .cwd(self.cwd.clone())
            .errexit(true)
            .nounset(true)
            .cancel_flag(Arc::clone(cancel))
            .stdout_sink(stdout_buf.clone())
            .stderr_sink(stderr_buf.clone())
            .timeout(std::time::Duration::from_secs(timeout_secs))
            .build();

        let status = shell.run_program(&program);
        let exit_code = status.code();

        // Collect captured output.
        let stdout_lock = stdout_buf.lock().unwrap();
        let output_capped = stdout_lock.capped;
        let mut output = stdout_lock.buf.clone();
        drop(stdout_lock);
        let stderr_lock = stderr_buf.lock().unwrap();
        if !stderr_lock.buf.is_empty() {
            output.extend_from_slice(b"\n[stderr]\n");
            output.extend_from_slice(&stderr_lock.buf);
        }
        drop(stderr_lock);

        // Exit code 130 = killed by signal. Distinguish cause:
        // - cancel flag not set → timeout (shell's internal deadline fired)
        // - cancel flag set + output not capped → user interrupt
        // - cancel flag set + output capped → output cap triggered cancel
        if exit_code == 130 && !cancel.load(Ordering::Relaxed) {
            return ToolResult::error(format!("timed out after {}s", timeout_secs));
        }
        if cancel.load(Ordering::Relaxed) && !output_capped {
            return ToolResult::error("Interrupted");
        }
        if output_capped {
            output.extend_from_slice(b"\n[output truncated: exceeded 1MB cap]");
        }

        let raw = String::from_utf8_lossy(&output);

        // Apply the output filter pipeline (ANSI strip, dedup, JSON schema, language filters).
        let filtered = output_filter::filter_bash_output(command, &raw);

        let content = if exit_code != 0 {
            format!("{}\n[exit code: {}]", filtered, exit_code)
        } else {
            filtered.into_owned()
        };

        let line_count = content.lines().count();
        let display = if is_text_reading {
            None
        } else if exit_code != 0 {
            Some(format!("exit {} ({} lines)", exit_code, line_count))
        } else if line_count > 5 {
            let preview: String = content.lines().take(3).collect::<Vec<_>>().join("\n");
            Some(format!("{}\n  ... ({} lines)", preview, line_count))
        } else {
            Some(content.clone())
        };

        let details = ToolDetails { display, filtered: true, exit_code: Some(exit_code), diff: None };
        if exit_code != 0 {
            ToolResult { content, details: Some(details), is_error: true }
        } else {
            ToolResult::ok_with_details(content, details)
        }
    }
}

/// Check via the AST whether the command is a bare text-reading tool (sed, head,
/// tail, awk) without pipes. These get their display suppressed to nudge the
/// agent toward the dedicated read tool.
fn is_bare_text_reader(program: &epsh::ast::Program) -> bool {
    use epsh::ast::Command;
    // Must be a single top-level simple command (not a pipeline, sequence, etc.)
    if program.commands.len() != 1 {
        return false;
    }
    match &program.commands[0] {
        Command::Simple { args, .. } => {
            let Some(name_word) = args.first() else { return false };
            let Some(name) = word_to_literal(name_word) else { return false };
            let base = name.rsplit('/').next().unwrap_or(&name);
            TEXT_READING_COMMANDS.contains(&base)
        }
        _ => false,
    }
}

/// Extract a fully-literal string from a word (no expansions).
fn word_to_literal(word: &epsh::ast::Word) -> Option<String> {
    use epsh::ast::WordPart;
    let mut out = String::new();
    for part in &word.parts {
        match part {
            WordPart::Literal(s) => out.push_str(s),
            _ => return None,
        }
    }
    Some(out)
}
