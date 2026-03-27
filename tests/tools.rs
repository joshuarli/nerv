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

// ── Multi-edit tests ──

#[test]
fn edit_multi_basic() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("test.rs");
    std::fs::write(&file, "fn alpha() {}\nfn beta() {}\nfn gamma() {}\n").unwrap();

    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mq);
    let result = tool.execute(
        serde_json::json!({
            "path": "test.rs",
            "edits": [
                {"old_text": "fn alpha()", "new_text": "fn one()"},
                {"old_text": "fn beta()", "new_text": "fn two()"},
                {"old_text": "fn gamma()", "new_text": "fn three()"}
            ]
        }),
        noop_update(),
    );

    assert!(!result.is_error, "multi-edit failed: {}", result.content);
    let content = std::fs::read_to_string(&file).unwrap();
    assert!(content.contains("fn one()"));
    assert!(content.contains("fn two()"));
    assert!(content.contains("fn three()"));
    assert!(!content.contains("fn alpha()"));
}

#[test]
fn edit_multi_out_of_order() {
    // Edits listed in reverse file order — should still work
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("test.txt");
    std::fs::write(&file, "aaa\nbbb\nccc\n").unwrap();

    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mq);
    let result = tool.execute(
        serde_json::json!({
            "path": "test.txt",
            "edits": [
                {"old_text": "ccc", "new_text": "CCC"},
                {"old_text": "aaa", "new_text": "AAA"}
            ]
        }),
        noop_update(),
    );

    assert!(!result.is_error, "failed: {}", result.content);
    let content = std::fs::read_to_string(&file).unwrap();
    assert_eq!(content, "AAA\nbbb\nCCC\n");
}

#[test]
fn edit_multi_overlap_rejected() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("test.txt");
    std::fs::write(&file, "abcdef\n").unwrap();

    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mq);
    let result = tool.execute(
        serde_json::json!({
            "path": "test.txt",
            "edits": [
                {"old_text": "abcd", "new_text": "ABCD"},
                {"old_text": "cdef", "new_text": "CDEF"}
            ]
        }),
        noop_update(),
    );

    assert!(result.is_error);
    assert!(result.content.contains("overlap"), "expected overlap error: {}", result.content);
    // File should be unchanged
    let content = std::fs::read_to_string(&file).unwrap();
    assert_eq!(content, "abcdef\n");
}

#[test]
fn edit_multi_not_found() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("test.txt");
    std::fs::write(&file, "hello world\n").unwrap();

    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mq);
    let result = tool.execute(
        serde_json::json!({
            "path": "test.txt",
            "edits": [
                {"old_text": "hello", "new_text": "hi"},
                {"old_text": "does not exist", "new_text": "x"}
            ]
        }),
        noop_update(),
    );

    assert!(result.is_error);
    assert!(result.content.contains("not found"), "{}", result.content);
    // File should be unchanged — preflight catches it
    let content = std::fs::read_to_string(&file).unwrap();
    assert_eq!(content, "hello world\n");
}

#[test]
fn edit_multi_preserves_crlf() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("test.txt");
    std::fs::write(&file, "aaa\r\nbbb\r\nccc\r\n").unwrap();

    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mq);
    let result = tool.execute(
        serde_json::json!({
            "path": "test.txt",
            "edits": [
                {"old_text": "aaa", "new_text": "AAA"},
                {"old_text": "ccc", "new_text": "CCC"}
            ]
        }),
        noop_update(),
    );

    assert!(!result.is_error, "failed: {}", result.content);
    let bytes = std::fs::read(&file).unwrap();
    let content = String::from_utf8_lossy(&bytes);
    assert!(content.contains("AAA\r\n"));
    assert!(content.contains("CCC\r\n"));
}

#[test]
fn edit_multi_returns_diff() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("test.txt");
    std::fs::write(&file, "foo\nbar\nbaz\n").unwrap();

    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mq);
    let result = tool.execute(
        serde_json::json!({
            "path": "test.txt",
            "edits": [
                {"old_text": "foo", "new_text": "FOO"},
                {"old_text": "baz", "new_text": "BAZ"}
            ]
        }),
        noop_update(),
    );

    assert!(!result.is_error);
    assert!(result.content.contains("-foo"), "diff missing -foo: {}", result.content);
    assert!(result.content.contains("+FOO"), "diff missing +FOO: {}", result.content);
    assert!(result.content.contains("-baz"), "diff missing -baz: {}", result.content);
    assert!(result.content.contains("+BAZ"), "diff missing +BAZ: {}", result.content);
    assert!(result.content.contains("Applied 2 edits"));
}

#[test]
fn edit_multi_single_edit_in_array() {
    // Single edit via the edits array should work identically to old_text/new_text
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("test.txt");
    std::fs::write(&file, "hello world\n").unwrap();

    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mq);
    let result = tool.execute(
        serde_json::json!({
            "path": "test.txt",
            "edits": [
                {"old_text": "hello", "new_text": "goodbye"}
            ]
        }),
        noop_update(),
    );

    assert!(!result.is_error, "failed: {}", result.content);
    let content = std::fs::read_to_string(&file).unwrap();
    assert_eq!(content, "goodbye world\n");
}

#[test]
fn edit_multi_adjacent_edits() {
    // Edits on consecutive lines, no overlap
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("test.txt");
    std::fs::write(&file, "line1\nline2\nline3\nline4\n").unwrap();

    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mq);
    let result = tool.execute(
        serde_json::json!({
            "path": "test.txt",
            "edits": [
                {"old_text": "line1\n", "new_text": "LINE1\n"},
                {"old_text": "line2\n", "new_text": "LINE2\n"},
                {"old_text": "line4\n", "new_text": "LINE4\n"}
            ]
        }),
        noop_update(),
    );

    assert!(!result.is_error, "failed: {}", result.content);
    let content = std::fs::read_to_string(&file).unwrap();
    assert_eq!(content, "LINE1\nLINE2\nline3\nLINE4\n");
}

#[test]
fn edit_multi_multiline_replacements() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("test.rs");
    std::fs::write(
        &file,
        "fn foo() {\n    old_body_1();\n}\n\nfn bar() {\n    old_body_2();\n}\n",
    )
    .unwrap();

    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mq);
    let result = tool.execute(
        serde_json::json!({
            "path": "test.rs",
            "edits": [
                {"old_text": "    old_body_1();", "new_text": "    new_body_1();\n    extra_line();"},
                {"old_text": "    old_body_2();", "new_text": "    new_body_2();"}
            ]
        }),
        noop_update(),
    );

    assert!(!result.is_error, "failed: {}", result.content);
    let content = std::fs::read_to_string(&file).unwrap();
    assert!(content.contains("new_body_1();\n    extra_line();"));
    assert!(content.contains("new_body_2();"));
}

#[test]
fn edit_validates_both_modes_rejected() {
    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(PathBuf::from("/tmp"), mq);
    let result = tool.validate(&serde_json::json!({
        "path": "test.txt",
        "old_text": "a",
        "new_text": "b",
        "edits": [{"old_text": "c", "new_text": "d"}]
    }));
    assert!(result.is_err());
}

#[test]
fn edit_validates_empty_edits_rejected() {
    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(PathBuf::from("/tmp"), mq);
    let result = tool.validate(&serde_json::json!({
        "path": "test.txt",
        "edits": []
    }));
    assert!(result.is_err());
}

#[test]
fn edit_validates_missing_new_text_in_edits() {
    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(PathBuf::from("/tmp"), mq);
    let result = tool.validate(&serde_json::json!({
        "path": "test.txt",
        "edits": [{"old_text": "a"}]
    }));
    assert!(result.is_err());
}

// ── Single-edit additional coverage ──

#[test]
fn edit_single_fuzzy_match_smart_quotes() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("test.txt");
    std::fs::write(&file, "say \u{201C}hello\u{201D}\n").unwrap();

    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mq);
    let result = tool.execute(
        serde_json::json!({
            "path": "test.txt",
            "old_text": "say \"hello\"",
            "new_text": "say \"world\""
        }),
        noop_update(),
    );

    assert!(!result.is_error, "fuzzy match failed: {}", result.content);
    assert!(result.content.contains("fuzzy"));
}

#[test]
fn edit_single_nonexistent_file() {
    let tmp = TempDir::new().unwrap();
    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mq);
    let result = tool.execute(
        serde_json::json!({
            "path": "nonexistent.txt",
            "old_text": "a",
            "new_text": "b"
        }),
        noop_update(),
    );
    assert!(result.is_error);
    assert!(result.content.contains("Error reading"));
}

#[test]
fn edit_single_empty_replacement() {
    // Deletion: replace text with empty string
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("test.txt");
    std::fs::write(&file, "keep\ndelete_me\nkeep\n").unwrap();

    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mq);
    let result = tool.execute(
        serde_json::json!({
            "path": "test.txt",
            "old_text": "delete_me\n",
            "new_text": ""
        }),
        noop_update(),
    );

    assert!(!result.is_error, "failed: {}", result.content);
    let content = std::fs::read_to_string(&file).unwrap();
    assert_eq!(content, "keep\nkeep\n");
}

#[test]
fn edit_single_absolute_path() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("test.txt");
    std::fs::write(&file, "old\n").unwrap();

    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mq);
    let result = tool.execute(
        serde_json::json!({
            "path": file.to_str().unwrap(),
            "old_text": "old",
            "new_text": "new"
        }),
        noop_update(),
    );

    assert!(!result.is_error, "failed: {}", result.content);
    let content = std::fs::read_to_string(&file).unwrap();
    assert_eq!(content, "new\n");
}

#[test]
fn edit_multi_deletion_and_insertion() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("test.txt");
    std::fs::write(&file, "aaa\nbbb\nccc\nddd\n").unwrap();

    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mq);
    let result = tool.execute(
        serde_json::json!({
            "path": "test.txt",
            "edits": [
                {"old_text": "bbb\n", "new_text": ""},
                {"old_text": "ddd", "new_text": "DDD\neee"}
            ]
        }),
        noop_update(),
    );

    assert!(!result.is_error, "failed: {}", result.content);
    let content = std::fs::read_to_string(&file).unwrap();
    assert_eq!(content, "aaa\nccc\nDDD\neee\n");
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

#[test]
fn edit_single_preserves_bom() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("bom.txt");
    std::fs::write(&file, "\u{FEFF}hello world\n").unwrap();

    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mq);
    let result = tool.execute(
        serde_json::json!({
            "path": "bom.txt",
            "old_text": "hello",
            "new_text": "goodbye"
        }),
        noop_update(),
    );

    assert!(!result.is_error, "failed: {}", result.content);
    let bytes = std::fs::read(&file).unwrap();
    // BOM is EF BB BF in UTF-8
    assert_eq!(&bytes[..3], b"\xEF\xBB\xBF", "BOM was stripped");
    let content = String::from_utf8_lossy(&bytes);
    assert!(content.contains("goodbye"));
}

#[test]
fn edit_multi_preserves_bom() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("bom.txt");
    std::fs::write(&file, "\u{FEFF}aaa\nbbb\n").unwrap();

    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mq);
    let result = tool.execute(
        serde_json::json!({
            "path": "bom.txt",
            "edits": [
                {"old_text": "aaa", "new_text": "AAA"},
                {"old_text": "bbb", "new_text": "BBB"}
            ]
        }),
        noop_update(),
    );

    assert!(!result.is_error, "failed: {}", result.content);
    let bytes = std::fs::read(&file).unwrap();
    assert_eq!(&bytes[..3], b"\xEF\xBB\xBF", "BOM was stripped");
    let content = String::from_utf8_lossy(&bytes);
    assert!(content.contains("AAA"));
    assert!(content.contains("BBB"));
}

#[test]
fn edit_single_no_change_rejected() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("test.txt");
    std::fs::write(&file, "hello world\n").unwrap();

    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mq);
    let result = tool.execute(
        serde_json::json!({
            "path": "test.txt",
            "old_text": "hello",
            "new_text": "hello"
        }),
        noop_update(),
    );

    assert!(result.is_error);
    assert!(result.content.contains("No changes"), "{}", result.content);
}

#[test]
fn edit_multi_no_change_rejected() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("test.txt");
    std::fs::write(&file, "aaa\nbbb\n").unwrap();

    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mq);
    let result = tool.execute(
        serde_json::json!({
            "path": "test.txt",
            "edits": [
                {"old_text": "aaa", "new_text": "aaa"},
                {"old_text": "bbb", "new_text": "bbb"}
            ]
        }),
        noop_update(),
    );

    assert!(result.is_error);
    assert!(result.content.contains("No changes"), "{}", result.content);
}

// ── Bug regression tests ──
// These use 2+ edits to exercise the multi-edit code path.
// (1-edit arrays route to single-edit which has its own checks.)

#[test]
fn edit_multi_ambiguous_old_text_rejected() {
    // "foo" appears twice. An edit targeting "foo" alongside another valid
    // edit should be rejected — find() would silently pick the first.
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("test.txt");
    std::fs::write(&file, "foo\nbar\nfoo\n").unwrap();

    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mq);
    let result = tool.execute(
        serde_json::json!({
            "path": "test.txt",
            "edits": [
                {"old_text": "foo", "new_text": "baz"},
                {"old_text": "bar", "new_text": "BAR"}
            ]
        }),
        noop_update(),
    );

    assert!(result.is_error, "should reject ambiguous match: {}", result.content);
    let content = std::fs::read_to_string(&file).unwrap();
    assert_eq!(content, "foo\nbar\nfoo\n");
}

#[test]
fn edit_multi_two_edits_same_old_text_rejected() {
    // Two edits both targeting "foo" (which appears twice).
    // Third edit keeps us in multi-edit path.
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("test.txt");
    std::fs::write(&file, "foo\nbar\nfoo\nbaz\n").unwrap();

    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mq);
    let result = tool.execute(
        serde_json::json!({
            "path": "test.txt",
            "edits": [
                {"old_text": "foo", "new_text": "first"},
                {"old_text": "baz", "new_text": "BAZ"},
                {"old_text": "foo", "new_text": "second"}
            ]
        }),
        noop_update(),
    );

    assert!(result.is_error, "should reject duplicate targets: {}", result.content);
    let content = std::fs::read_to_string(&file).unwrap();
    assert_eq!(content, "foo\nbar\nfoo\nbaz\n");
}

#[test]
fn edit_multi_empty_old_text_rejected() {
    // Empty old_text with 2 edits to exercise multi-edit path
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("test.txt");
    std::fs::write(&file, "hello\nworld\n").unwrap();

    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mq);
    let result = tool.execute(
        serde_json::json!({
            "path": "test.txt",
            "edits": [
                {"old_text": "", "new_text": "injected"},
                {"old_text": "world", "new_text": "WORLD"}
            ]
        }),
        noop_update(),
    );

    assert!(result.is_error, "empty old_text should be rejected: {}", result.content);
    let content = std::fs::read_to_string(&file).unwrap();
    assert_eq!(content, "hello\nworld\n");
}
