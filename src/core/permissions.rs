//! Tool permission system.
//!
//! Auto-approves operations within the repo root. Prompts for confirmation
//! when tools access paths outside the repo or run potentially dangerous commands.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub enum Permission {
    /// Safe — auto-approve without prompting.
    Allow,
    /// Needs user confirmation. Contains a human-readable reason.
    Ask(String),
}

/// Check whether a tool call should be auto-approved or needs user confirmation.
pub fn check(tool: &str, args: &serde_json::Value, repo_root: Option<&Path>) -> Permission {
    match tool {
        "read" | "grep" | "find" | "ls" => check_read_tool(args, repo_root),
        "edit" | "write" => check_write_tool(tool, args, repo_root),
        "bash" => check_bash(args, repo_root),
        "memory" => Permission::Allow,
        _ => Permission::Ask(format!("unknown tool: {}", tool)),
    }
}

fn check_read_tool(args: &serde_json::Value, repo_root: Option<&Path>) -> Permission {
    let Some(path) = args["path"].as_str() else {
        return Permission::Allow;
    };
    if is_within_repo(path, repo_root) {
        Permission::Allow
    } else {
        Permission::Ask(format!("read outside repo: {}", path))
    }
}

fn check_write_tool(tool: &str, args: &serde_json::Value, repo_root: Option<&Path>) -> Permission {
    let Some(path) = args["path"].as_str() else {
        return Permission::Allow;
    };
    if is_within_repo(path, repo_root) {
        Permission::Allow
    } else {
        Permission::Ask(format!("{} outside repo: {}", tool, path))
    }
}

fn check_bash(args: &serde_json::Value, repo_root: Option<&Path>) -> Permission {
    let Some(cmd) = args["command"].as_str() else {
        return Permission::Allow;
    };

    // Dangerous commands that always need approval
    let dangerous = [
        "sudo ",
        "rm -rf /",
        "rm -rf ~",
        "mkfs",
        "dd if=",
        "> /dev/sd",
        "> /dev/disk",
        "chmod -R",
        "chown -R",
        "curl|sh",
        "curl|bash",
        "wget|sh",
        "wget|bash",
    ];
    for d in &dangerous {
        if cmd.contains(d) {
            return Permission::Ask(format!("dangerous command: {}", cmd));
        }
    }

    // Subshell/eval — can hide arbitrary commands
    if cmd.contains("$(") || cmd.contains('`') || cmd.contains("eval ") {
        return Permission::Ask(format!("bash uses subshell/eval: {}", truncate_cmd(cmd)));
    }

    // Using sed/head/tail/awk directly on files — should use the read tool instead
    // Don't flag pipes (e.g., `rg | head` is fine) or grep flags (e.g., --no-heading is fine)
    let has_pipe = cmd.contains('|');
    
    if !has_pipe {
        // sed with file path — should use read instead
        if cmd.contains("sed ") && (cmd.contains(".rs") || cmd.contains(".toml") || cmd.contains(".md") || cmd.contains("/")) {
            return Permission::Ask(format!("use the read tool instead of bash + sed: {}", truncate_cmd(cmd)));
        }
        // tail with file extension — should use read
        if (cmd.contains(" tail ") || cmd.starts_with("tail ")) && (cmd.contains(".rs") || cmd.contains(".toml") || cmd.contains(".md")) {
            return Permission::Ask(format!("use the read tool instead of bash + tail: {}", truncate_cmd(cmd)));
        }
        // head with file extension (not in flag like --no-heading)
        if (cmd.contains(" head ") || cmd.starts_with("head ")) && (cmd.contains(".rs") || cmd.contains(".toml") || cmd.contains(".md")) {
            return Permission::Ask(format!("use the read tool instead of bash + head: {}", truncate_cmd(cmd)));
        }
        // awk with file extension
        if (cmd.contains(" awk ") || cmd.starts_with("awk ")) && (cmd.contains(".rs") || cmd.contains(".toml") || cmd.contains(".md")) {
            return Permission::Ask(format!("use the read tool instead of bash + awk: {}", truncate_cmd(cmd)));
        }
    }

    // Check for paths outside repo in the command (including after redirects)
    if let Some(root) = repo_root {
        // Extract all tokens, including redirect targets
        let tokens = extract_path_tokens(cmd);
        let root_str = root.to_string_lossy();

        for token in &tokens {
            if token.starts_with('/') && !token.starts_with(root_str.as_ref()) {
                if is_safe_system_path(token) {
                    continue;
                }
                return Permission::Ask(format!("path outside repo: {}", token));
            }
            if token.starts_with("~/")
                && !token.starts_with(&format!("~/{}", root_str.trim_start_matches('/')))
            {
                if token.starts_with("~/.nerv")
                    || token.starts_with("~/.config")
                    || token.starts_with("~/.cargo")
                {
                    continue;
                }
                return Permission::Ask(format!("home path: {}", token));
            }
        }
    }

    Permission::Allow
}

/// Normalize a path by resolving `.` and `..` without touching the filesystem.
fn normalize_path(path: &Path) -> PathBuf {
    let mut out = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            c => out.push(c),
        }
    }
    out.iter().collect()
}

fn is_safe_system_path(path: &str) -> bool {
    path.starts_with("/dev/null")
        || path.starts_with("/dev/stderr")
        || path.starts_with("/dev/stdout")
        || path.starts_with("/tmp")
        || path.starts_with("/usr/bin")
        || path.starts_with("/usr/local")
        || path.starts_with("/bin")
        || path.starts_with("/opt")
        || path.starts_with("/proc/self")
        || path.starts_with("/etc/hosts")
}

/// Extract tokens that might be paths, including redirect targets.
fn extract_path_tokens(cmd: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    // Strip double-quoted and single-quoted strings before tokenizing — their
    // contents are argument values (commit messages, regex patterns, etc.) and
    // should never be interpreted as path tokens.
    let mut stripped = String::with_capacity(cmd.len());
    let mut chars = cmd.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' || c == '\'' {
            // Consume until matching closing quote (no escape handling needed)
            while let Some(inner) = chars.next() {
                if inner == c {
                    break;
                }
            }
            stripped.push(' '); // preserve token boundary
        } else {
            stripped.push(c);
        }
    }

    // Split on whitespace but also catch tokens after redirect operators
    let cleaned = stripped
        .replace(">>", " >> ")
        .replace(">", " > ")
        .replace("<", " < ");
    for token in cleaned.split_whitespace() {
        if token.starts_with('/') || token.starts_with("~/") {
            // Only treat a token as a path if it actually exists on the
            // filesystem. This rejects non-existent paths, regex patterns,
            // and other strings that happen to start with `/` or `~/`.
            let resolved = if let Some(rest) = token.strip_prefix("~/") {
                crate::home_dir().map(|h| h.join(rest))
            } else {
                Some(PathBuf::from(token))
            };
            if resolved.map(|p| p.exists()).unwrap_or(false) {
                tokens.push(token.to_string());
            }
        }
    }
    tokens
}

fn truncate_cmd(cmd: &str) -> String {
    if cmd.len() > 80 {
        format!("{}...", &cmd[..80])
    } else {
        cmd.to_string()
    }
}

fn is_within_repo(path: &str, repo_root: Option<&Path>) -> bool {
    let Some(root) = repo_root else {
        // No repo root detected — allow everything (no git repo = no boundary)
        return true;
    };

    let resolved = if path.starts_with('/') {
        PathBuf::from(path)
    } else if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = crate::home_dir() {
            home.join(rest)
        } else {
            return true;
        }
    } else {
        // Relative path — resolve against repo root to catch ../../../etc/passwd
        root.join(path)
    };

    // Normalize to resolve .. components without requiring the path to exist
    let resolved = normalize_path(&resolved);
    let root = normalize_path(root);
    resolved.starts_with(&root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> PathBuf {
        PathBuf::from("/Users/josh/d/pi2")
    }

    #[test]
    fn read_within_repo_allowed() {
        let args = serde_json::json!({"path": "src/main.rs"});
        assert_eq!(check("read", &args, Some(&repo())), Permission::Allow);
    }

    #[test]
    fn read_absolute_within_repo_allowed() {
        let args = serde_json::json!({"path": "/Users/josh/d/pi2/src/main.rs"});
        assert_eq!(check("read", &args, Some(&repo())), Permission::Allow);
    }

    #[test]
    fn read_outside_repo_asks() {
        let args = serde_json::json!({"path": "/etc/passwd"});
        assert!(matches!(
            check("read", &args, Some(&repo())),
            Permission::Ask(_)
        ));
    }

    #[test]
    fn write_within_repo_allowed() {
        let args = serde_json::json!({"path": "src/new_file.rs", "content": "hello"});
        assert_eq!(check("write", &args, Some(&repo())), Permission::Allow);
    }

    #[test]
    fn write_outside_repo_asks() {
        let args = serde_json::json!({"path": "/tmp/evil.sh", "content": "rm -rf /"});
        assert!(matches!(
            check("write", &args, Some(&repo())),
            Permission::Ask(_)
        ));
    }

    #[test]
    fn edit_outside_repo_asks() {
        let args =
            serde_json::json!({"path": "/Users/josh/.zshrc", "old_text": "a", "new_text": "b"});
        assert!(matches!(
            check("edit", &args, Some(&repo())),
            Permission::Ask(_)
        ));
    }

    #[test]
    fn bash_simple_command_allowed() {
        let args = serde_json::json!({"command": "cargo test"});
        assert_eq!(check("bash", &args, Some(&repo())), Permission::Allow);
    }

    #[test]
    fn bash_dangerous_command_asks() {
        let args = serde_json::json!({"command": "sudo rm -rf /"});
        assert!(matches!(
            check("bash", &args, Some(&repo())),
            Permission::Ask(_)
        ));
    }

    #[test]
    fn bash_outside_path_asks() {
        let args = serde_json::json!({"command": "cat /etc/passwd"});
        assert!(matches!(
            check("bash", &args, Some(&repo())),
            Permission::Ask(_)
        ));
    }

    #[test]
    fn bash_system_paths_allowed() {
        let args = serde_json::json!({"command": "ls /usr/bin/git"});
        assert_eq!(check("bash", &args, Some(&repo())), Permission::Allow);
    }

    #[test]
    fn bash_dev_null_allowed() {
        let args = serde_json::json!({"command": "echo test > /dev/null"});
        assert_eq!(check("bash", &args, Some(&repo())), Permission::Allow);
    }

    #[test]
    fn memory_always_allowed() {
        let args = serde_json::json!({"action": "list"});
        assert_eq!(check("memory", &args, Some(&repo())), Permission::Allow);
    }

    #[test]
    fn no_repo_root_allows_everything() {
        let args = serde_json::json!({"path": "/etc/passwd"});
        assert_eq!(check("read", &args, None), Permission::Allow);
    }

    #[test]
    fn relative_within_repo_allowed() {
        let args = serde_json::json!({"path": "src/../Cargo.toml"});
        assert_eq!(check("read", &args, Some(&repo())), Permission::Allow);
    }

    #[test]
    fn relative_escaping_repo_asks() {
        let args = serde_json::json!({"path": "../../etc/passwd"});
        assert!(matches!(
            check("read", &args, Some(&repo())),
            Permission::Ask(_)
        ));
    }

    #[test]
    fn bash_subshell_asks() {
        let args = serde_json::json!({"command": "cat $(find / -name secret)"});
        assert!(matches!(
            check("bash", &args, Some(&repo())),
            Permission::Ask(_)
        ));
    }

    #[test]
    fn bash_backtick_asks() {
        let args = serde_json::json!({"command": "cat `which evil`"});
        assert!(matches!(
            check("bash", &args, Some(&repo())),
            Permission::Ask(_)
        ));
    }

    #[test]
    fn bash_redirect_outside_repo_asks() {
        let args = serde_json::json!({"command": "echo evil > /etc/passwd"});
        assert!(matches!(
            check("bash", &args, Some(&repo())),
            Permission::Ask(_)
        ));
    }

    #[test]
    fn bash_pipe_to_tee_outside_asks() {
        let args = serde_json::json!({"command": "echo hi | tee /etc/passwd"});
        assert!(matches!(
            check("bash", &args, Some(&repo())),
            Permission::Ask(_)
        ));
    }

    #[test]
    fn bash_regex_pattern_double_slash_allowed() {
        // A regex argument like `//.*Value` starts with `/` but is not a path.
        // It must not trigger a permission prompt.
        let args = serde_json::json!({"command": r#"rg --color=never --no-heading -n "//.*Value" src/"#});
        assert_eq!(check("bash", &args, Some(&repo())), Permission::Allow);
    }

    #[test]
    fn bash_rg_search_in_src_allowed() {
        let args = serde_json::json!({"command": r#"rg --color=never --no-heading -n "serde_json" src/ --type rust | sort | head -12"#});
        assert_eq!(check("bash", &args, Some(&repo())), Permission::Allow);
    }

    #[test]
    fn bash_git_commit_with_double_slash_in_message_allowed() {
        // `//` appearing in a -m commit message must not be treated as an outside-repo path.
        let args = serde_json::json!({"command": r#"git commit -m "fix prompt for // patterns""#});
        assert_eq!(check("bash", &args, Some(&repo())), Permission::Allow);
    }

    #[test]
    fn bash_sed_for_reading_asks() {
        let args = serde_json::json!({"command": "sed -n '5,10p' src/main.rs"});
        assert!(matches!(
            check("bash", &args, Some(&repo())),
            Permission::Ask(_)
        ));
    }

    #[test]
    fn bash_head_for_reading_asks() {
        let args = serde_json::json!({"command": "head -n 20 src/main.rs"});
        assert!(matches!(
            check("bash", &args, Some(&repo())),
            Permission::Ask(_)
        ));
    }

    #[test]
    fn bash_tail_for_reading_asks() {
        let args = serde_json::json!({"command": "tail -n 50 Cargo.toml"});
        assert!(matches!(
            check("bash", &args, Some(&repo())),
            Permission::Ask(_)
        ));
    }

    #[test]
    fn bash_awk_for_reading_asks() {
        let args = serde_json::json!({"command": "awk '{print NR, $0}' src/lib.rs"});
        assert!(matches!(
            check("bash", &args, Some(&repo())),
            Permission::Ask(_)
        ));
    }
}

