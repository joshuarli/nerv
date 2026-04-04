use std::path::PathBuf;
use std::process::Command;

use super::truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, truncate_tail};
use crate::agent::agent::{AgentTool, ToolResult};
use crate::agent::provider::CancelFlag;
use crate::agent::types::ToolDetails;
use crate::errors::ToolError;
use crate::str::StrExt as _;

const GREP_MAX_LINE_LENGTH: usize = 500;
const GREP_ALLOWED_KEYS: &[&str] = &[
    "pattern",
    "path",
    "file",
    "glob",
    "include",
    "ignore_case",
    "literal",
    "context",
    "limit",
    "files_with_matches",
    "count",
];

pub struct GrepTool {
    cwd: PathBuf,
}
impl GrepTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }
    fn resolve_path(&self, path: &str) -> String {
        crate::resolve_path(path, &self.cwd).to_string_lossy().into_owned()
    }
}

impl AgentTool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }
    fn is_readonly(&self) -> bool {
        true
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
                "file": {"type": "string", "description": "Deprecated alias for `path` (prefer `path`)"},
                "glob": {"type": "string", "description": "Filter files by glob, e.g. '*.ts' or '**/*.spec.ts'"},
                "ignore_case": {"type": "boolean", "description": "Case-insensitive search (default: false)"},
                "literal": {"type": "boolean", "description": "Treat pattern as literal string, not regex (default: false)"},
                "context": {"type": "integer", "description": "Lines of context before and after each match (default: 3)"},
                "limit": {"type": "integer", "description": "Max matches to return (default: 100)"},
                "files_with_matches": {"type": "boolean", "description": "Only print filenames of files containing matches (default: false)"},
                "count": {"type": "boolean", "description": "Only print match count per file (default: false)"}
            },
            "required": ["pattern"],
            "additionalProperties": false
        })
    }

    fn prompt_guidelines(&self) -> Vec<String> {
        vec!["For scoped searches, pass `grep.path` (not `grep.file`).".into()]
    }

    fn validate(&self, input: &serde_json::Value) -> Result<(), ToolError> {
        let Some(obj) = input.as_object() else {
            return Err(ToolError::InvalidArguments {
                message: "arguments must be an object".into(),
            });
        };
        let mut unknown: Vec<&str> =
            obj.keys().map(|k| k.as_str()).filter(|k| !GREP_ALLOWED_KEYS.contains(k)).collect();
        if !unknown.is_empty() {
            unknown.sort_unstable();
            return Err(ToolError::InvalidArguments {
                message: format!(
                    "unknown argument(s): {} (allowed: {})",
                    unknown.join(", "),
                    GREP_ALLOWED_KEYS.join(", ")
                ),
            });
        }
        if input.get("pattern").and_then(|v| v.as_str()).is_none() {
            let keys: Vec<&str> = input
                .as_object()
                .map(|m| m.keys().map(|s| s.as_str()).collect())
                .unwrap_or_default();
            return Err(ToolError::InvalidArguments {
                message: format!("pattern (string) is required (got keys: {})", keys.join(", ")),
            });
        }
        let path = input.get("path").and_then(|v| v.as_str());
        let file = input.get("file").and_then(|v| v.as_str());
        if let (Some(path), Some(file)) = (path, file)
            && path != file
        {
            return Err(ToolError::InvalidArguments {
                message: "use only one path selector: `path` or legacy `file`".into(),
            });
        }
        Ok(())
    }
    fn execute(&self, input: serde_json::Value, _cancel: &CancelFlag) -> ToolResult {
        let pattern = input["pattern"].as_str().unwrap_or("");
        let path_from_path = input.get("path").and_then(|v| v.as_str());
        let path_from_file = input.get("file").and_then(|v| v.as_str());
        let legacy_file_alias_used = path_from_path.is_none() && path_from_file.is_some();
        let path = path_from_path.or(path_from_file).unwrap_or(".");
        let legacy_warning = if legacy_file_alias_used {
            Some("[warning] `grep.file` is deprecated; use `grep.path`.")
        } else {
            None
        };
        let resolved_path = self.resolve_path(path);
        let glob = input.get("glob").and_then(|v| v.as_str());
        // Legacy param name
        let include = input.get("include").and_then(|v| v.as_str());
        let ignore_case = input.get("ignore_case").and_then(|v| v.as_bool()).unwrap_or(false);
        let literal = input.get("literal").and_then(|v| v.as_bool()).unwrap_or(false);
        let context = input.get("context").and_then(|v| v.as_u64()).unwrap_or(3);
        let limit = input.get("limit").and_then(|v| v.as_u64()).unwrap_or(100) as usize;
        // Output mode flags — mutually exclusive; files_with_matches takes precedence
        // over count
        let files_with_matches =
            input.get("files_with_matches").and_then(|v| v.as_bool()).unwrap_or(false);
        let count_mode = input.get("count").and_then(|v| v.as_bool()).unwrap_or(false);

        let rg = match crate::rg() {
            Some(p) => p,
            None => return ToolResult::ok("rg (ripgrep) is not installed"),
        };
        let mut cmd = Command::new(rg);
        cmd.arg("--color=never").arg(format!("--max-count={}", limit)).current_dir(&self.cwd);

        if files_with_matches {
            // -l: only filenames; no context lines or line numbers needed
            cmd.arg("--files-with-matches");
        } else if count_mode {
            // -c: match count per file; no context lines or line numbers
            cmd.arg("--count");
        } else {
            // Normal mode: annotated matches with context
            cmd.arg("--no-heading").arg("--line-number").arg(format!("--context={}", context));
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

        // Pass relative paths as-is (current_dir is already set).
        // Resolving to absolute breaks ripgrep's --glob matching.
        let search_path = if path.starts_with('~') || path.starts_with('/') {
            resolved_path
        } else {
            path.to_string()
        };
        cmd.arg(pattern).arg(&search_path);

        match cmd.output() {
            Ok(output) => {
                if output.stdout.is_empty() && !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    if !stderr.trim().is_empty() {
                        return ToolResult::error(format!("rg: {}", stderr.trim()));
                    }
                    return if let Some(w) = legacy_warning {
                        ToolResult::ok(format!("{w}\nNo matches found"))
                    } else {
                        ToolResult::ok("No matches found")
                    };
                }
                let tr = truncate_tail(&output.stdout, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES);

                // Truncate long lines to keep grep output compact (not needed in files/count
                // modes since those lines are always short, but harmless to
                // apply uniformly)
                let mut content = truncate_long_lines(&tr.content, GREP_MAX_LINE_LENGTH);
                // Surface any warnings/errors rg emitted on stderr
                let stderr = String::from_utf8_lossy(&output.stderr);
                if !stderr.trim().is_empty() {
                    content.push_str("\n[stderr]\n");
                    content.push_str(stderr.trim());
                }
                if let Some(w) = legacy_warning {
                    if content.is_empty() {
                        content.push_str(w);
                    } else {
                        content = format!("{w}\n{content}");
                    }
                }
                let content = content;

                let display = if files_with_matches {
                    let file_count = tr.content.lines().filter(|l| !l.is_empty()).count();
                    if tr.truncated {
                        format!("{} files (truncated)", file_count)
                    } else {
                        format!("{} files", file_count)
                    }
                } else if count_mode {
                    // Each line is "file:N"; sum the N values
                    let total: u64 = tr
                        .content
                        .lines()
                        .filter_map(|l| l.rfind(':').and_then(|i| l[i + 1..].parse::<u64>().ok()))
                        .sum();
                    if tr.truncated {
                        format!("{} matches across files (truncated)", total)
                    } else {
                        format!("{} matches across files", total)
                    }
                } else {
                    let match_count = tr
                        .content
                        .lines()
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
                    ToolDetails { display: Some(display), ..Default::default() },
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
            out.push_str(line.truncate_bytes(max_chars));
            out.push_str("...");
        } else {
            out.push_str(line);
        }
    }
    // Preserve trailing newline from subprocess output
    if text.ends_with('\n') && !out.ends_with('\n') {
        out.push('\n');
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

    #[test]
    fn validate_accepts_legacy_file_alias() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tool = GrepTool::new(tmp.path().to_path_buf());
        let result = tool.validate(&serde_json::json!({"pattern": "foo", "file": "src"}));
        assert!(result.is_ok(), "legacy alias should be accepted");
    }

    #[test]
    fn validate_rejects_conflicting_path_and_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tool = GrepTool::new(tmp.path().to_path_buf());
        let err = tool
            .validate(&serde_json::json!({"pattern": "foo", "path": "src", "file": "tests"}))
            .unwrap_err();
        assert!(err.to_string().contains("use only one path selector"), "{}", err);
    }

    #[test]
    fn validate_rejects_unknown_argument() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tool = GrepTool::new(tmp.path().to_path_buf());
        let err = tool.validate(&serde_json::json!({"pattern": "foo", "bogus": true})).unwrap_err();
        assert!(err.to_string().contains("unknown argument"), "{}", err);
    }
}
