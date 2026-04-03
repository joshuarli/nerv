/// Display utilities for the interactive mode.
use super::theme;
use crate::str::StrExt as _;

/// Colorize a single line of tool result output (diffs, errors, context).
pub fn render_tool_result_line(line: &str, is_error: bool) -> String {
    if is_error {
        return format!("{}{}{}", theme::ERROR, line, theme::RESET);
    }
    if line.starts_with('+') && !line.starts_with("+++") {
        return format!("{}{}{}", theme::DIFF_ADD, line, theme::RESET);
    }
    if line.starts_with('-') && !line.starts_with("---") {
        return format!("{}{}{}", theme::DIFF_DEL, line, theme::RESET);
    }
    if line.starts_with("@@") {
        return format!("{}{}{}", theme::DIFF_HUNK, line, theme::RESET);
    }
    if line.starts_with("---") || line.starts_with("+++") {
        return format!("{}{}{}", theme::BOLD, line, theme::RESET);
    }
    format!("{}{}{}", theme::MUTED, line, theme::RESET)
}

/// Format tool call display for ToolExecutionStart (with ANSI color).
pub fn format_tool_call(name: &str, args: &serde_json::Value) -> String {
    let tn = theme::TOOL_NAME;
    let tp = theme::TOOL_PATH;
    let r = theme::RESET;
    let d = theme::DIM;

    match name {
        "read" => {
            let path = args["path"].as_str().unwrap_or("?");
            let mut s = format!("{}read{} {}{}{}", tn, r, tp, path, r);
            if let Some(off) = args.get("offset").and_then(|v| v.as_u64()) {
                s.push_str(&format!(" {}(from line {}){}", d, off, r));
            }
            if let Some(lim) = args.get("limit").and_then(|v| v.as_u64()) {
                s.push_str(&format!(" {}(limit {}){}", d, lim, r));
            }
            s
        }
        "edit" => format!("{}edit{} {}{}{}", tn, r, tp, args["path"].as_str().unwrap_or("?"), r),
        "write" => format!("{}write{} {}{}{}", tn, r, tp, args["path"].as_str().unwrap_or("?"), r),
        "epsh" => {
            let cmd = args["command"].as_str().unwrap_or("?");
            format!("{}${} {}{}", tn, r, cmd, r)
        }
        "grep" => {
            let pattern = args["pattern"].as_str().unwrap_or("?");
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            format!("{}grep{} {} {}{}{}", tn, r, pattern, tp, path, r)
        }
        "find" => format!("{}find{} {}", tn, r, args["pattern"].as_str().unwrap_or("?")),
        "ls" => format!(
            "{}ls{} {}{}{}",
            tn,
            r,
            tp,
            args.get("path").and_then(|v| v.as_str()).unwrap_or("."),
            r
        ),
        _ => format!("{}{}{} {}", tn, name, r, args),
    }
}

/// Format a SystemTime as a human-readable relative age.
pub fn format_age(modified: &std::time::SystemTime) -> String {
    let Ok(elapsed) = modified.elapsed() else {
        return "?".into();
    };
    let secs = elapsed.as_secs();
    if secs < 60 {
        "now".into()
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else if secs < 604800 {
        format!("{}d", secs / 86400)
    } else if secs < 2592000 {
        format!("{}w", secs / 604800)
    } else {
        format!("{}mo", secs / 2592000)
    }
}

/// Shorten a path relative to home or repo root.
pub fn shorten_path(path: &str, home: &str, repo_root: Option<&str>) -> String {
    if let Some(root) = repo_root {
        if path == root {
            return ".".to_string();
        }
        if let Some(rel) = path.strip_prefix(root) {
            return rel.trim_start_matches('/').to_string();
        }
    }
    if !home.is_empty() && path.starts_with(home) {
        return format!("~{}", &path[home.len()..]);
    }
    path.to_string()
}

/// Format a token count as a compact human-readable string (e.g. "1.2k", "3M").
pub fn fmt_tokens(n: u64) -> String {
    if n == 0 {
        "0".into()
    } else if n < 1_000 {
        n.to_string()
    } else if n < 10_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else if n < 1_000_000 {
        format!("{}k", n / 1_000)
    } else if n < 10_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else {
        format!("{}M", n / 1_000_000)
    }
}

/// Format a dollar cost as a compact string (4, 3, or 2 decimal places).
pub fn fmt_cost(dollars: f64) -> String {
    if dollars < 0.01 {
        format!("{:.4}", dollars)
    } else if dollars < 1.0 {
        format!("{:.3}", dollars)
    } else {
        format!("{:.2}", dollars)
    }
}

/// Truncate a string at a char boundary.
pub fn truncate_str(s: &str, max: usize) -> &str {
    s.truncate_bytes(max)
}
