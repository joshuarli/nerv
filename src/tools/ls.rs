use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use crate::agent::agent::{AgentTool, ToolResult, UpdateCallback};
use crate::agent::provider::CancelFlag;
use crate::agent::types::ToolDetails;
use crate::errors::ToolError;

pub struct LsTool {
    cwd: PathBuf,
}
impl LsTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }
    fn resolve_path(&self, path: &str) -> PathBuf {
        // Expand ~ at the start of the path
        if let Some(rest) = path.strip_prefix('~')
            && let Some(home) = crate::home_dir()
        {
            return home.join(rest.trim_start_matches('/'));
        }
        if path.starts_with('/') { PathBuf::from(path) } else { self.cwd.join(path) }
    }
}

const DEFAULT_DEPTH: usize = 2;
const MAX_DEPTH: usize = 5;
const MAX_ENTRIES: usize = 2000;

/// Render a directory tree like `eza --tree -L{depth}`.
///
/// Entries at each level are sorted: directories first, then files, both
/// alphabetically.  Hidden files (dot-files) are skipped.
fn render_tree(root: &Path, depth: usize, out: &mut String, entry_count: &mut usize) {
    out.push_str(&root.to_string_lossy());
    out.push('\n');
    render_dir(root, "", 0, depth, out, entry_count);
}

fn render_dir(
    dir: &Path,
    prefix: &str,
    current_depth: usize,
    max_depth: usize,
    out: &mut String,
    entry_count: &mut usize,
) {
    if current_depth >= max_depth || *entry_count >= MAX_ENTRIES {
        return;
    }

    let mut entries: Vec<(bool, PathBuf)> = match fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| {
                // Skip hidden entries
                e.file_name().to_str().map(|s| !s.starts_with('.')).unwrap_or(false)
            })
            .map(|e| {
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                (is_dir, e.path())
            })
            .collect(),
        Err(_) => return,
    };

    // Dirs first, then files; alphabetical within each group
    entries.sort_by(|a, b| {
        b.0.cmp(&a.0).then_with(|| {
            a.1.file_name().unwrap_or(OsStr::new("")).cmp(b.1.file_name().unwrap_or(OsStr::new("")))
        })
    });

    let len = entries.len();
    for (i, (is_dir, path)) in entries.into_iter().enumerate() {
        if *entry_count >= MAX_ENTRIES {
            break;
        }
        let last = i + 1 == len;
        let connector = if last { "└── " } else { "├── " };
        let name = path.file_name().unwrap_or(OsStr::new("")).to_string_lossy();
        out.push_str(prefix);
        out.push_str(connector);
        out.push_str(&name);
        out.push('\n');
        *entry_count += 1;

        if is_dir {
            let child_prefix = format!("{}{}", prefix, if last { "    " } else { "│   " });
            render_dir(&path, &child_prefix, current_depth + 1, max_depth, out, entry_count);
        }
    }
}

impl AgentTool for LsTool {
    fn name(&self) -> &str {
        "ls"
    }
    fn description(&self) -> &str {
        "List directory contents as a tree."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Directory to list (default: cwd)"},
                "depth": {"type": "integer", "description": "Max depth to recurse (default: 2, max: 5)"}
            },
            "required": [],
            "additionalProperties": false
        })
    }
    fn validate(&self, _input: &serde_json::Value) -> Result<(), ToolError> {
        Ok(())
    }
    fn execute(
        &self,
        input: serde_json::Value,
        _update: UpdateCallback,
        _cancel: &CancelFlag,
    ) -> ToolResult {
        let path_str = input["path"].as_str().unwrap_or(".");
        let resolved = self.resolve_path(path_str);
        let depth = input["depth"]
            .as_u64()
            .map(|d| (d as usize).clamp(1, MAX_DEPTH))
            .unwrap_or(DEFAULT_DEPTH);

        if !resolved.exists() {
            return ToolResult::error(format!("path not found: {}", resolved.display()));
        }
        if !resolved.is_dir() {
            return ToolResult::error(format!("not a directory: {}", resolved.display()));
        }

        let mut content = String::new();
        let mut entry_count = 0usize;
        render_tree(&resolved, depth, &mut content, &mut entry_count);

        let display = format!("{} ({} entries)", path_str, entry_count);
        ToolResult::ok_with_details(content, ToolDetails { display: Some(display), ..Default::default() })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    fn tree(dir: &Path) -> String {
        tree_depth(dir, DEFAULT_DEPTH)
    }

    fn tree_depth(dir: &Path, depth: usize) -> String {
        let mut out = String::new();
        let mut count = 0usize;
        render_tree(dir, depth, &mut out, &mut count);
        out
    }

    fn setup() -> TempDir {
        let tmp = TempDir::new().unwrap();
        let r = tmp.path();
        fs::create_dir(r.join("src")).unwrap();
        fs::write(r.join("src/main.rs"), "").unwrap();
        fs::write(r.join("src/lib.rs"), "").unwrap();
        fs::create_dir(r.join("src/tools")).unwrap();
        fs::write(r.join("src/tools/read.rs"), "").unwrap();
        fs::write(r.join("Cargo.toml"), "").unwrap();
        fs::write(r.join(".gitignore"), "").unwrap(); // hidden — should not appear
        tmp
    }

    #[test]
    fn test_tree_structure() {
        let tmp = setup();
        let out = tree(tmp.path());
        // Root line present
        assert!(out.starts_with(tmp.path().to_str().unwrap()), "should start with root path");
        // Known files/dirs present
        assert!(out.contains("Cargo.toml"));
        assert!(out.contains("src"));
        assert!(out.contains("main.rs"));
        assert!(out.contains("lib.rs"));
        // Depth-2 subdir contents appear
        assert!(out.contains("tools"));
        // Hidden files excluded
        assert!(!out.contains(".gitignore"));
    }

    #[test]
    fn test_depth_limit() {
        let tmp = setup();
        let out = tree(tmp.path());
        // tools/ is depth-1, read.rs inside it is depth-2 — should NOT appear
        // (DEFAULT_DEPTH=2 means we recurse into depth-1 dirs but not depth-2
        // dirs)
        assert!(!out.contains("read.rs"), "depth-2 file contents should be hidden");
    }

    #[test]
    fn test_depth_3_shows_nested() {
        let tmp = setup();
        let out = tree_depth(tmp.path(), 3);
        // With depth=3, read.rs inside src/tools/ should now appear
        assert!(out.contains("read.rs"), "depth-3 should show files nested 2 levels deep");
    }

    #[test]
    fn test_depth_1_hides_files() {
        let tmp = setup();
        let out = tree_depth(tmp.path(), 1);
        // With depth=1, only top-level entries appear — files inside src/ should not
        assert!(out.contains("src"), "src dir should appear at depth 1");
        assert!(!out.contains("main.rs"), "files inside src/ should be hidden at depth 1");
    }

    #[test]
    fn test_sort_order() {
        let tmp = setup();
        let out = tree(tmp.path());
        // src/ (dir) should appear before Cargo.toml (file)
        let src_pos = out.find("src").unwrap();
        let cargo_pos = out.find("Cargo.toml").unwrap();
        assert!(src_pos < cargo_pos, "directories should sort before files");
    }

    #[test]
    fn test_tree_connectors() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.txt"), "").unwrap();
        fs::write(tmp.path().join("b.txt"), "").unwrap();
        let out = tree(tmp.path());
        // last entry uses └──, earlier entries use ├──
        assert!(out.contains("├── a.txt"));
        assert!(out.contains("└── b.txt"));
    }

    #[test]
    fn test_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let out = tree(tmp.path());
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 1, "empty dir should only have root line");
    }

    #[test]
    fn test_tool_not_found() {
        let tool = LsTool::new(std::env::current_dir().unwrap());
        let cancel = crate::agent::provider::new_cancel_flag();
        let cb: crate::agent::agent::UpdateCallback = std::sync::Arc::new(|_| {});
        let result =
            tool.execute(serde_json::json!({"path": "/nonexistent_path_xyz"}), cb, &cancel);
        assert!(result.content.contains("not found") || result.content.contains("error"));
    }
}
