use std::path::PathBuf;
use std::process::Command;

use super::truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, truncate_tail};
use crate::agent::agent::{AgentTool, ToolResult, UpdateCallback};
use crate::errors::ToolError;

const GREP_MAX_LINE_LENGTH: usize = 500;

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
        "Search file contents using ripgrep. Respects .gitignore."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Regex pattern to search for"},
                "path": {"type": "string", "description": "Directory or file to search (default: cwd)"},
                "glob": {"type": "string", "description": "Filter files by glob, e.g. '*.ts' or '**/*.spec.ts'"},
                "ignore_case": {"type": "boolean", "description": "Case-insensitive search (default: false)"},
                "literal": {"type": "boolean", "description": "Treat pattern as literal string, not regex (default: false)"},
                "context": {"type": "integer", "description": "Lines of context before and after each match (default: 3)"},
                "limit": {"type": "integer", "description": "Max matches to return (default: 100)"},
                "files_with_matches": {"type": "boolean", "description": "Only print filenames of files containing matches (default: false)"},
                "count": {"type": "boolean", "description": "Only print match count per file (default: false)"}
            },
            "required": ["pattern"]
        })
    }
    fn validate(&self, input: &serde_json::Value) -> Result<(), ToolError> {
        if input.get("pattern").and_then(|v| v.as_str()).is_none() {
            let keys: Vec<&str> = input.as_object().map(|m| m.keys().map(|s| s.as_str()).collect()).unwrap_or_default();
            return Err(ToolError::InvalidArguments {
                message: format!("pattern (string) is required (got keys: {})", keys.join(", ")),
            });
        }
        Ok(())
    }
    fn execute(&self, input: serde_json::Value, _update: UpdateCallback) -> ToolResult {
        let pattern = input["pattern"].as_str().unwrap_or("");
        let path = input["path"].as_str().unwrap_or(".");
        let resolved_path = self.resolve_path(path);
        let glob = input.get("glob").and_then(|v| v.as_str());
        // Legacy param name
        let include = input.get("include").and_then(|v| v.as_str());
        let ignore_case = input.get("ignore_case").and_then(|v| v.as_bool()).unwrap_or(false);
        let literal = input.get("literal").and_then(|v| v.as_bool()).unwrap_or(false);
        let context = input.get("context").and_then(|v| v.as_u64()).unwrap_or(3);
        let limit = input.get("limit").and_then(|v| v.as_u64()).unwrap_or(100) as usize;
        // Output mode flags — mutually exclusive; files_with_matches takes precedence over count
        let files_with_matches = input.get("files_with_matches").and_then(|v| v.as_bool()).unwrap_or(false);
        let count_mode = input.get("count").and_then(|v| v.as_bool()).unwrap_or(false);

        let mut cmd = Command::new("rg");
        cmd.arg("--color=never")
            .arg(format!("--max-count={}", limit))
            .current_dir(&self.cwd);

        if files_with_matches {
            // -l: only filenames; no context lines or line numbers needed
            cmd.arg("--files-with-matches");
        } else if count_mode {
            // -c: match count per file; no context lines or line numbers
            cmd.arg("--count");
        } else {
            // Normal mode: annotated matches with context
            cmd.arg("--no-heading")
                .arg("--line-number")
                .arg(format!("--context={}", context));
        }

        if ignore_case {
            cmd.arg("--ignore-case");
        }
        if literal {
            cmd.arg("--fixed-strings");
        }
        if let Some(g) = glob.or(include) {
            cmd.arg("--glob").arg(g);
        }

        cmd.arg(pattern).arg(&resolved_path);

        match cmd.output() {
            Ok(output) => {
                if output.stdout.is_empty() && !output.status.success() {
                    return ToolResult::ok("No matches found");
                }
                let tr = truncate_tail(&output.stdout, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES);

                // Truncate long lines to keep grep output compact (not needed in files/count modes
                // since those lines are always short, but harmless to apply uniformly)
                let content = truncate_long_lines(&tr.content, GREP_MAX_LINE_LENGTH);

                let display = if files_with_matches {
                    let file_count = content.lines().filter(|l| !l.is_empty()).count();
                    if tr.truncated {
                        format!("{} files (truncated)", file_count)
                    } else {
                        format!("{} files", file_count)
                    }
                } else if count_mode {
                    // Each line is "file:N"; sum the N values
                    let total: u64 = content.lines()
                        .filter_map(|l| l.rfind(':').and_then(|i| l[i+1..].parse::<u64>().ok()))
                        .sum();
                    if tr.truncated {
                        format!("{} matches across files (truncated)", total)
                    } else {
                        format!("{} matches across files", total)
                    }
                } else {
                    let match_count = content.lines()
                        .filter(|l| !l.starts_with("--") && !l.is_empty())
                        .count();
                    if tr.truncated {
                        format!("{} matches (truncated)", match_count)
                    } else {
                        format!("{} matches", match_count)
                    }
                };
                ToolResult::ok_with_details(
                    content,
                    serde_json::json!({"truncated": tr.truncated, "display": display}),
                )
            }
            Err(e) => ToolResult::error(format!("Error running rg: {}", e)),
        }
    }
}

fn truncate_long_lines(text: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.lines().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if line.len() > max_chars {
            out.push_str(&line[..max_chars]);
            out.push_str("...");
        } else {
            out.push_str(line);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_long_lines_preserves_short() {
        assert_eq!(truncate_long_lines("short\nline", 100), "short\nline");
    }

    #[test]
    fn truncate_long_lines_cuts() {
        let long = "x".repeat(600);
        let result = truncate_long_lines(&long, 500);
        assert!(result.ends_with("..."));
        assert_eq!(result.len(), 503); // 500 + "..."
    }
}
