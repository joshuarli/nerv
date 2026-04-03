use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::agent::agent::{AgentTool, ToolResult};
use crate::agent::provider::CancelFlag;
use crate::agent::types::ToolDetails;
use crate::errors::ToolError;
use crate::tools::output_filter;

/// Text-reading commands whose display output is suppressed to nudge the
/// agent toward the dedicated read tool.
const TEXT_READING_COMMANDS: &[&str] = &["sed", "head", "tail", "awk"];

pub struct EpshTool {
    cwd: PathBuf,
}

impl EpshTool {
    pub fn new(cwd: PathBuf) -> Self {
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

        // Build a fresh shell per invocation.
        let mut shell = epsh::eval::Shell::new();
        shell.set_cwd(self.cwd.clone());
        shell.opts.errexit = true;
        shell.opts.nounset = true;

        // Inherit environment so tools like git, cargo, rustc find their config.
        for (k, v) in std::env::vars() {
            shell.set_var(&k, &v);
            shell.vars.export(&k);
        }

        // Output capture via sinks.
        let stdout_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let stderr_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        shell.set_stdout_sink(stdout_buf.clone());
        shell.set_stderr_sink(stderr_buf.clone());

        // Wire up cancellation.
        shell.set_cancel_flag(Arc::clone(cancel));

        // Timeout: a watchdog thread that waits on a condvar for the full
        // duration. If the main thread finishes first it notifies the condvar
        // and the watchdog exits immediately. If the deadline passes, the
        // watchdog sets the cancel flag.
        let timed_out = Arc::new(AtomicBool::new(false));
        let done_pair = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let cancel_for_timeout = Arc::clone(cancel);
        let timed_out_flag = Arc::clone(&timed_out);
        let done_for_watchdog = Arc::clone(&done_pair);
        let watchdog = std::thread::Builder::new()
            .name("epsh-timeout".into())
            .stack_size(64 * 1024)
            .spawn(move || {
                let (lock, cvar) = &*done_for_watchdog;
                let guard = lock.lock().unwrap();
                let (guard, result) = cvar
                    .wait_timeout_while(guard, Duration::from_secs(timeout_secs), |done| !*done)
                    .unwrap();
                if !*guard && result.timed_out() {
                    timed_out_flag.store(true, Ordering::Relaxed);
                    cancel_for_timeout.store(true, Ordering::Relaxed);
                }
            })
            .expect("failed to spawn timeout watchdog");

        let status = shell.run_program(&program);
        let exit_code = status.code();

        // Signal the watchdog to exit and wait for it.
        {
            let (lock, cvar) = &*done_pair;
            *lock.lock().unwrap() = true;
            cvar.notify_one();
        }
        let _ = watchdog.join();

        if timed_out.load(Ordering::Relaxed) {
            return ToolResult::error(format!("timed out after {}s", timeout_secs));
        }
        if cancel.load(Ordering::Relaxed) {
            return ToolResult::error("Interrupted");
        }

        // Collect captured output.
        let mut output = stdout_buf.lock().unwrap().clone();
        let stderr = stderr_buf.lock().unwrap();
        if !stderr.is_empty() {
            output.extend_from_slice(b"\n[stderr]\n");
            output.extend_from_slice(&stderr);
        }
        drop(stderr);

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
