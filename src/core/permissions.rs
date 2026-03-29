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
    check_with_allowed_dirs(tool, args, repo_root, &[])
}

/// Like [`check`], but also auto-approves any path that falls within one of
/// the user-granted `allowed_dirs` (populated by the "allow directory" prompt
/// response).
pub fn check_with_allowed_dirs(
    tool: &str,
    args: &serde_json::Value,
    repo_root: Option<&Path>,
    allowed_dirs: &[PathBuf],
) -> Permission {
    // If the user has granted a directory, auto-approve paths inside it.
    if !allowed_dirs.is_empty() {
        if let Some(path) = path_for_args(tool, args) {
            let resolved = resolve_path(&path, repo_root);
            let resolved = normalize_path(&resolved);
            for dir in allowed_dirs {
                let dir = normalize_path(dir);
                if resolved.starts_with(&dir) {
                    return Permission::Allow;
                }
            }
        }
    }

    match tool {
        "read" | "grep" | "find" | "ls" | "symbols" | "codemap" => check_read_tool(args, repo_root),
        "edit" | "write" => check_write_tool(tool, args, repo_root),
        "bash" => check_bash(args, repo_root),
        "memory" => Permission::Allow,
        _ => Permission::Ask(format!("unknown tool: {}", tool)),
    }
}

/// Extract the primary filesystem path from a tool call's args, if present.
/// Used to determine whether a path falls within a user-granted directory.
pub fn path_for_args(tool: &str, args: &serde_json::Value) -> Option<String> {
    match tool {
        "read" | "edit" | "write" | "grep" | "find" | "ls" | "symbols" | "codemap" => {
            args["path"].as_str().map(|s| s.to_string())
        }
        // bash commands are not paths — returning None disables the "allow dir"
        // shortcut so pressing 'a' on a bash prompt falls back to a simple allow.
        "bash" => None,
        _ => None,
    }
}

/// Resolve a raw path string to an absolute PathBuf (without touching filesystem).
fn resolve_path(path: &str, repo_root: Option<&Path>) -> PathBuf {
    if path.starts_with('/') {
        PathBuf::from(path)
    } else if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = crate::home_dir() {
            home.join(rest)
        } else {
            PathBuf::from(path)
        }
    } else {
        // Relative — resolve against repo root if available.
        if let Some(root) = repo_root {
            root.join(path)
        } else {
            PathBuf::from(path)
        }
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

    // eval — can execute arbitrary constructed strings
    if cmd.contains("eval ") {
        return Permission::Ask(format!("bash uses eval: {}", cmd));
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
        || is_safe_home_path(path)
}

fn is_safe_home_path(path: &str) -> bool {
    let Some(home) = crate::home_dir() else {
        return false;
    };
    let home = home.to_string_lossy();
    // Same directories exempted for ~/ prefixed paths (check_bash line 97-101)
    for dir in &[".nerv", ".config", ".cargo"] {
        if path.starts_with(&format!("{}/{}", home, dir)) {
            return true;
        }
    }
    false
}

/// Extract tokens that might be paths, including redirect targets.
fn extract_path_tokens(cmd: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    // Strip heredoc body — everything after a `<< EOF` or `<< 'EOF'` marker is
    // literal content and must not be scanned for path tokens.
    let cmd = if let Some(pos) = cmd.find("<<") {
        // Find end of the `<<` line; heredoc body starts after the first newline.
        if let Some(nl) = cmd[pos..].find('\n') {
            &cmd[..pos + nl]
        } else {
            &cmd[..pos]
        }
    } else {
        cmd
    };
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
    fn bash_eval_asks() {
        let args = serde_json::json!({"command": "eval \"$HOSTILE\""});
        assert!(matches!(
            check("bash", &args, Some(&repo())),
            Permission::Ask(_)
        ));
    }

    #[test]
    fn bash_for_loop_allowed() {
        let cmd = "for t in agent btw session integration; do\n  result=$(cargo test --test $t 2>&1 | tail -1)\n  echo \"$t: $result\"\ndone";
        let args = serde_json::json!({"command": cmd});
        assert_eq!(check("bash", &args, Some(&repo())), Permission::Allow);
    }

    #[test]
    fn bash_backtick_command_allowed() {
        // Backticks are common in scripts; path checks still apply.
        let args = serde_json::json!({"command": "echo `date`"});
        assert_eq!(check("bash", &args, Some(&repo())), Permission::Allow);
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
    fn bash_git_commit_with_cd_prefix_allowed() {
        // `cd /repo && git add -A && git commit -m "..."` must be allowed when
        // the cd target is the repo root.  Also covers the case where the
        // commit message has a stray trailing `"` (double-quote at end of the
        // outer shell string), which confuses the quote-stripper.
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        // Normal form
        let cmd = format!(
            "cd {} && git add -A && git commit -m \"fix: some message\"",
            root.display()
        );
        let args = serde_json::json!({"command": cmd});
        assert_eq!(check("bash", &args, Some(&root)), Permission::Allow);

        // Stray trailing `"` — the raw string the /commit skill sometimes emits
        let cmd2 = format!(
            "cd {} && git add -A && git commit -m \"fix: some message\"\"",
            root.display()
        );
        let args2 = serde_json::json!({"command": cmd2});
        assert_eq!(check("bash", &args2, Some(&root)), Permission::Allow);
    }

    #[test]
    fn bash_path_for_args_returns_none() {
        // bash commands are not paths — path_for_args must return None so that
        // pressing 'a' on a bash permission prompt doesn't push the entire
        // command string into allowed_dirs (which caused repeat prompts).
        let args = serde_json::json!({"command": "git add -A && git commit -m \"msg\""});
        assert_eq!(super::path_for_args("bash", &args), None);
    }

    #[test]
    fn bash_heredoc_to_tmp_allowed() {
        // heredoc body contains `//` (a Rust comment) which is a valid filesystem
        // path on macOS but must not be scanned — only the redirect target matters.
        let cmd = "cat > /tmp/test.rs << 'EOF'\n// a rust comment\nfn main() {}\nEOF";
        let args = serde_json::json!({"command": cmd});
        assert_eq!(check("bash", &args, Some(&repo())), Permission::Allow);
    }

    #[test]
    fn bash_absolute_nerv_dir_allowed() {
        // ~/.nerv is the app's data directory — absolute paths into it should be
        // safe just like the ~/ prefixed form.
        let home = crate::home_dir().unwrap();
        let db = format!("sqlite3 {}/.nerv/sessions.db '.tables'", home.display());
        let args = serde_json::json!({"command": db});
        assert_eq!(check("bash", &args, Some(&repo())), Permission::Allow);
    }

    #[test]
    fn bash_absolute_cargo_dir_allowed() {
        let home = crate::home_dir().unwrap();
        let cmd = format!("cat {}/.cargo/config.toml", home.display());
        let args = serde_json::json!({"command": cmd});
        assert_eq!(check("bash", &args, Some(&repo())), Permission::Allow);
    }

    #[test]
    fn bash_absolute_config_dir_allowed() {
        let home = crate::home_dir().unwrap();
        let cmd = format!("cat {}/.config/some-tool/config.yml", home.display());
        let args = serde_json::json!({"command": cmd});
        assert_eq!(check("bash", &args, Some(&repo())), Permission::Allow);
    }

    #[test]
    fn allowed_dir_grants_access_to_path_within_it() {
        let allowed = PathBuf::from("/Users/josh/external");
        let args = serde_json::json!({"path": "/Users/josh/external/foo.rs"});
        assert_eq!(
            check_with_allowed_dirs("read", &args, Some(&repo()), &[allowed]),
            Permission::Allow
        );
    }

    #[test]
    fn allowed_dir_does_not_grant_access_outside_it() {
        let allowed = PathBuf::from("/Users/josh/external");
        let args = serde_json::json!({"path": "/etc/passwd"});
        assert!(matches!(
            check_with_allowed_dirs("read", &args, Some(&repo()), &[allowed]),
            Permission::Ask(_)
        ));
    }

    #[test]
    fn allowed_dir_empty_falls_back_to_normal_check() {
        let args = serde_json::json!({"path": "/etc/passwd"});
        assert!(matches!(
            check_with_allowed_dirs("read", &args, Some(&repo()), &[]),
            Permission::Ask(_)
        ));
    }

    #[test]
    fn bash_absolute_other_home_dir_asks() {
        // Arbitrary home subdirs (not in the safe list) should still prompt.
        // We create a real temp file under $HOME so that extract_path_tokens
        // can verify the path exists on disk (it skips non-existent paths to
        // avoid treating non-path strings as paths).
        let home = crate::home_dir().unwrap();
        let tmp_file = home.join(".zsh_nerv_perm_test");
        std::fs::write(&tmp_file, "").unwrap();
        let cmd = format!("cat {}", tmp_file.display());
        let args = serde_json::json!({"command": cmd});
        let result = check("bash", &args, Some(&repo()));
        let _ = std::fs::remove_file(&tmp_file);
        assert!(matches!(result, Permission::Ask(_)));
    }
}
