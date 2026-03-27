//! Tool execution tests — each tool tested with real filesystem ops.

use std::path::PathBuf;
use std::sync::Arc;

use nerv::agent::agent::{AgentTool, UpdateCallback};
use nerv::tools::*;
use tempfile::TempDir;

fn noop_update() -> UpdateCallback {
    Arc::new(|_| {})
}

#[test]
fn read_tool_returns_numbered_lines() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("test.txt");
    std::fs::write(&file, "line1\nline2\nline3\n").unwrap();

    let tool = ReadTool::new(tmp.path().to_path_buf());
    let result = tool.execute(serde_json::json!({"path": "test.txt"}), noop_update());

    assert!(!result.is_error);
    assert!(result.content.contains("line1"));
    assert!(result.content.contains("line2"));
    // Should have line numbers
    assert!(result.content.contains("\t"));
}

#[test]
fn read_tool_offset_and_limit() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("test.txt");
    std::fs::write(&file, "a\nb\nc\nd\ne\n").unwrap();

    let tool = ReadTool::new(tmp.path().to_path_buf());
    let result = tool.execute(
        serde_json::json!({"path": "test.txt", "offset": 2, "limit": 2}),
        noop_update(),
    );

    assert!(!result.is_error);
    assert!(result.content.contains("c"));
    assert!(result.content.contains("d"));
    assert!(!result.content.contains("\ta\n"));
}

#[test]
fn read_tool_nonexistent_file() {
    let tmp = TempDir::new().unwrap();
    let tool = ReadTool::new(tmp.path().to_path_buf());
    let result = tool.execute(
        serde_json::json!({"path": "nonexistent.txt"}),
        noop_update(),
    );

    assert!(result.is_error);
    assert!(result.content.contains("Error"));
}

#[test]
fn write_tool_creates_file_and_dirs() {
    let tmp = TempDir::new().unwrap();
    let tool = WriteTool::new(tmp.path().to_path_buf());
    let result = tool.execute(
        serde_json::json!({"path": "sub/dir/file.txt", "content": "hello world"}),
        noop_update(),
    );

    assert!(!result.is_error);
    let content = std::fs::read_to_string(tmp.path().join("sub/dir/file.txt")).unwrap();
    assert_eq!(content, "hello world");
}

#[test]
fn edit_tool_exact_match_replacement() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("test.rs");
    std::fs::write(&file, "fn main() {\n    println!(\"old\");\n}\n").unwrap();

    let mutation_queue = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mutation_queue);
    let result = tool.execute(
        serde_json::json!({
            "path": "test.rs",
            "old_text": "println!(\"old\")",
            "new_text": "println!(\"new\")"
        }),
        noop_update(),
    );

    assert!(!result.is_error);
    let content = std::fs::read_to_string(&file).unwrap();
    assert!(content.contains("println!(\"new\")"));
    assert!(!content.contains("println!(\"old\")"));
    // Should return a diff
    assert!(result.content.contains("---"));
}

#[test]
fn edit_tool_rejects_ambiguous_match() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("test.txt");
    std::fs::write(&file, "foo\nbar\nfoo\n").unwrap();

    let mutation_queue = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mutation_queue);
    let result = tool.execute(
        serde_json::json!({
            "path": "test.txt",
            "old_text": "foo",
            "new_text": "baz"
        }),
        noop_update(),
    );

    assert!(result.is_error);
    assert!(result.content.contains("2 times"));
}

#[test]
fn edit_tool_not_found() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("test.txt");
    std::fs::write(&file, "hello world\n").unwrap();

    let mutation_queue = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mutation_queue);
    let result = tool.execute(
        serde_json::json!({
            "path": "test.txt",
            "old_text": "does not exist",
            "new_text": "replacement"
        }),
        noop_update(),
    );

    assert!(result.is_error);
    assert!(result.content.contains("not found"));
}

#[test]
fn edit_tool_preserves_crlf_line_endings() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("test.txt");
    std::fs::write(&file, "line1\r\nline2\r\nline3\r\n").unwrap();

    let mutation_queue = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mutation_queue);
    let result = tool.execute(
        serde_json::json!({
            "path": "test.txt",
            "old_text": "line2",
            "new_text": "replaced"
        }),
        noop_update(),
    );

    assert!(!result.is_error);
    let content = std::fs::read(&file).unwrap();
    // Should preserve CRLF
    assert!(content.windows(2).any(|w| w == b"\r\n"));
    assert!(String::from_utf8_lossy(&content).contains("replaced"));
}

#[test]
fn bash_tool_runs_command() {
    let tmp = TempDir::new().unwrap();
    let tool = BashTool::new(tmp.path().to_path_buf());
    let result = tool.execute(serde_json::json!({"command": "echo hello"}), noop_update());

    assert!(!result.is_error, "bash failed: {}", result.content);
    assert!(result.content.contains("hello"));
}

#[test]
fn bash_tool_reports_nonzero_exit() {
    let tmp = TempDir::new().unwrap();
    let tool = BashTool::new(tmp.path().to_path_buf());
    let result = tool.execute(serde_json::json!({"command": "exit 42"}), noop_update());

    assert!(result.is_error);
    assert!(result.content.contains("42"));
}

#[test]
fn tool_validation_rejects_missing_required_fields() {
    let tool = ReadTool::new(PathBuf::from("/tmp"));
    let result = tool.validate(&serde_json::json!({}));
    assert!(result.is_err());

    let result = tool.validate(&serde_json::json!({"path": "test.txt"}));
    assert!(result.is_ok());
}
