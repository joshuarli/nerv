//! Tool execution tests — each tool tested with real filesystem ops.

use std::path::PathBuf;
use std::sync::Arc;

use nerv::agent::agent::{AgentTool, ToolResult};
use nerv::agent::provider::{CancelFlag, new_cancel_flag};
use nerv::tools::*;
use tempfile::TempDir;

fn noop_cancel() -> CancelFlag {
    new_cancel_flag()
}

/// Count approximate tokens in a tool result (chars/4 heuristic).
fn approx_tokens(result: &ToolResult) -> usize {
    result.content.len() / 4
}

#[test]
fn read_tool_returns_numbered_lines() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("test.txt");
    std::fs::write(&file, "line1\nline2\nline3\n").unwrap();

    let tool = ReadTool::new(tmp.path().to_path_buf());
    let result = tool.execute(serde_json::json!({"path": "test.txt"}), &noop_cancel());

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
    // offset is 1-based: offset=3, limit=2 → lines 3 and 4 (c, d)
    let result = tool
        .execute(serde_json::json!({"path": "test.txt", "offset": 3, "limit": 2}), &noop_cancel());

    assert!(!result.is_error);
    assert!(result.content.contains("c"));
    assert!(result.content.contains("d"));
    assert!(!result.content.contains("\ta\n"));
}

#[test]
fn read_tool_nonexistent_file() {
    let tmp = TempDir::new().unwrap();
    let tool = ReadTool::new(tmp.path().to_path_buf());
    let result = tool.execute(serde_json::json!({"path": "nonexistent.txt"}), &noop_cancel());

    assert!(result.is_error);
    assert!(
        result.content.contains("not found") || result.content.contains("Error"),
        "expected error message, got: {}",
        result.content
    );
}

#[test]
fn write_tool_creates_file_and_dirs() {
    let tmp = TempDir::new().unwrap();
    let tool = WriteTool::new(tmp.path().to_path_buf());
    let result = tool.execute(
        serde_json::json!({"path": "sub/dir/file.txt", "content": "hello world"}),
        &noop_cancel(),
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
        &noop_cancel(),
    );

    assert!(!result.is_error);
    let content = std::fs::read_to_string(&file).unwrap();
    assert!(content.contains("println!(\"new\")"));
    assert!(!content.contains("println!(\"old\")"));
    assert!(result.content.contains("Edited"));
    // Diff goes to details, not content
    assert!(result.details.is_some());
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
        &noop_cancel(),
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
        &noop_cancel(),
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
        &noop_cancel(),
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
        &noop_cancel(),
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
        &noop_cancel(),
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
        &noop_cancel(),
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
        &noop_cancel(),
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
        &noop_cancel(),
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
        &noop_cancel(),
    );

    assert!(!result.is_error);
    assert!(result.content.contains("Applied 2 edits"));
    // Diff is in details, not content
    let details = result.details.unwrap();
    let diff = details.diff.as_deref().unwrap();
    assert!(diff.contains("-foo"), "diff missing -foo: {}", diff);
    assert!(diff.contains("+FOO"), "diff missing +FOO: {}", diff);
    assert!(diff.contains("-baz"), "diff missing -baz: {}", diff);
    assert!(diff.contains("+BAZ"), "diff missing +BAZ: {}", diff);
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
        &noop_cancel(),
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
        &noop_cancel(),
    );

    assert!(!result.is_error, "failed: {}", result.content);
    let content = std::fs::read_to_string(&file).unwrap();
    assert_eq!(content, "LINE1\nLINE2\nline3\nLINE4\n");
}

#[test]
fn edit_multi_multiline_replacements() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("test.rs");
    std::fs::write(&file, "fn foo() {\n    old_body_1();\n}\n\nfn bar() {\n    old_body_2();\n}\n")
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
        &noop_cancel(),
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

// ── normalize: double-encoded edits ──

// Regression: models sometimes emit `edits` as a JSON string rather than an
// inline array. validate() alone rejects it; normalize() must unwrap it first.
//
// The tricky case (observed in session d2cd6f6c) is that the string value
// contains literal newline characters — the model streams \n inside string
// values, which serde_json rejects. normalize() must re-escape them before
// attempting to parse the inner JSON.
#[test]
fn edit_validate_rejects_string_encoded_edits() {
    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(PathBuf::from("/tmp"), mq);
    // edits is a JSON string containing literal newlines (as a model would emit)
    let edits_str = "[\n  {\n    \"old_text\": \"Vec<String>,\",\n    \"new_text\": \"b\"\n  }\n]";
    let args = serde_json::json!({
        "path": "test.txt",
        "edits": edits_str
    });
    assert!(tool.validate(&args).is_err(), "validate should reject string-encoded edits");
}

#[test]
fn edit_normalize_unwraps_string_encoded_edits_and_validate_passes() {
    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(PathBuf::from("/tmp"), mq);
    // Mirrors the actual failure: edits is a string with literal \n chars and
    // inner quotes (e.g. Vec<String> in old_text).
    let edits_str = "[\n  {\n    \"old_text\": \"Vec<String>,\",\n    \"new_text\": \"b\"\n  }\n]";
    let args = serde_json::json!({
        "path": "test.txt",
        "edits": edits_str
    });
    let normalized = tool.normalize(args);
    assert!(
        normalized["edits"].is_array(),
        "normalize should convert string-encoded edits (with literal newlines) to array"
    );
    assert!(tool.validate(&normalized).is_ok());
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
        &noop_cancel(),
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
        &noop_cancel(),
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
        &noop_cancel(),
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
        &noop_cancel(),
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
        &noop_cancel(),
    );

    assert!(!result.is_error, "failed: {}", result.content);
    let content = std::fs::read_to_string(&file).unwrap();
    assert_eq!(content, "aaa\nccc\nDDD\neee\n");
}

#[test]
fn bash_tool_runs_command() {
    let tmp = TempDir::new().unwrap();
    let tool = EpshTool::new(tmp.path().to_path_buf());
    let result = tool.execute(serde_json::json!({"command": "echo hello"}), &noop_cancel());

    assert!(!result.is_error, "bash failed: {}", result.content);
    assert!(result.content.contains("hello"));
}

#[test]
fn bash_tool_reports_nonzero_exit() {
    let tmp = TempDir::new().unwrap();
    let tool = EpshTool::new(tmp.path().to_path_buf());
    let result = tool.execute(serde_json::json!({"command": "exit 42"}), &noop_cancel());

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
        &noop_cancel(),
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
        &noop_cancel(),
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
        &noop_cancel(),
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
        &noop_cancel(),
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
        &noop_cancel(),
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
        &noop_cancel(),
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
        &noop_cancel(),
    );

    assert!(result.is_error, "empty old_text should be rejected: {}", result.content);
    let content = std::fs::read_to_string(&file).unwrap();
    assert_eq!(content, "hello\nworld\n");
}

#[test]
fn edit_multi_error_reports_original_index() {
    // Edits listed in reverse file order. The third edit (index 2) has
    // bad old_text. After sorting by position, it might be at a different
    // slot — but the error should report the original index [2].
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
                {"old_text": "aaa", "new_text": "AAA"},
                {"old_text": "DOES NOT EXIST", "new_text": "x"}
            ]
        }),
        &noop_cancel(),
    );

    assert!(result.is_error);
    // Should say edits[2], not some other index
    assert!(
        result.content.contains("edits[2]"),
        "expected original index [2] in error: {}",
        result.content
    );
}

#[test]
fn edit_multi_large_file_rejected() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("big.txt");
    // 11MB file
    let content = "x".repeat(11 * 1024 * 1024);
    std::fs::write(&file, &content).unwrap();

    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mq);
    let result = tool.execute(
        serde_json::json!({
            "path": "big.txt",
            "old_text": "x",
            "new_text": "y"
        }),
        &noop_cancel(),
    );

    assert!(result.is_error, "should reject large file: {}", result.content);
    assert!(result.content.contains("too large"), "{}", result.content);
}

// ── Read tool ──

#[test]
fn edit_multi_fuzzy_match_trailing_whitespace() {
    // old_text with trailing whitespace stripped should still match file content
    // that has trailing whitespace (or vice-versa). This mirrors the real failure
    // mode where a model sends old_text with trailing spaces trimmed.
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("test.rs");
    std::fs::write(&file, "fn alpha() {  \nfn beta() {\n").unwrap();

    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mq);
    let result = tool.execute(
        serde_json::json!({
            "path": "test.rs",
            "edits": [
                {"old_text": "fn alpha() {", "new_text": "fn one() {"},
                {"old_text": "fn beta() {", "new_text": "fn two() {"}
            ]
        }),
        &noop_cancel(),
    );

    assert!(!result.is_error, "multi fuzzy match failed: {}", result.content);
    let written = std::fs::read_to_string(&file).unwrap();
    assert!(written.contains("fn one()"), "first edit not applied: {}", written);
    assert!(written.contains("fn two()"), "second edit not applied: {}", written);
}

#[test]
fn edit_multi_fuzzy_match_smart_quotes() {
    // Same fuzzy normalization (smart quotes → ASCII) should work in multi-edit
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("test.txt");
    std::fs::write(&file, "say \u{201C}hello\u{201D}\nsay \u{201C}world\u{201D}\n").unwrap();

    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mq);
    let result = tool.execute(
        serde_json::json!({
            "path": "test.txt",
            "edits": [
                {"old_text": "say \"hello\"", "new_text": "say \"goodbye\""},
                {"old_text": "say \"world\"", "new_text": "say \"earth\""}
            ]
        }),
        &noop_cancel(),
    );

    assert!(!result.is_error, "multi fuzzy smart-quote match failed: {}", result.content);
    let written = std::fs::read_to_string(&file).unwrap();
    assert!(written.contains("goodbye"), "first edit not applied: {}", written);
    assert!(written.contains("earth"), "second edit not applied: {}", written);
}

#[test]
fn read_empty_file() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("empty.txt"), "").unwrap();

    let tool = ReadTool::new(tmp.path().to_path_buf());
    let result = tool.execute(serde_json::json!({"path": "empty.txt"}), &noop_cancel());
    assert!(!result.is_error);
    assert!(result.content.is_empty() || result.content.trim().is_empty());
}

#[test]
fn read_binary_file_does_not_panic() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("bin"), b"\x00\x01\xff\xfe").unwrap();

    let tool = ReadTool::new(tmp.path().to_path_buf());
    let result = tool.execute(serde_json::json!({"path": "bin"}), &noop_cancel());
    assert!(!result.is_error);
}

#[test]
fn read_unicode() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("uni.txt"), "héllo 世界\nñ\n").unwrap();

    let tool = ReadTool::new(tmp.path().to_path_buf());
    let result = tool.execute(serde_json::json!({"path": "uni.txt"}), &noop_cancel());
    assert!(!result.is_error);
    assert!(result.content.contains("héllo"));
    assert!(result.content.contains("世界"));
}

#[test]
fn read_offset_past_end() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("small.txt"), "one\ntwo\n").unwrap();

    let tool = ReadTool::new(tmp.path().to_path_buf());
    let result =
        tool.execute(serde_json::json!({"path": "small.txt", "offset": 999}), &noop_cancel());
    assert!(!result.is_error);
    assert!(result.content.is_empty() || result.content.trim().is_empty());
}

#[test]
fn read_absolute_path() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("abs.txt");
    std::fs::write(&file, "content\n").unwrap();

    let tool = ReadTool::new(tmp.path().to_path_buf());
    let result = tool.execute(serde_json::json!({"path": file.to_str().unwrap()}), &noop_cancel());
    assert!(!result.is_error);
    assert!(result.content.contains("content"));
}

#[test]
fn read_output_token_efficiency() {
    let tmp = TempDir::new().unwrap();
    // 100 lines of code
    let lines: Vec<String> = (1..=100).map(|i| format!("let x{} = {};", i, i)).collect();
    std::fs::write(tmp.path().join("code.rs"), lines.join("\n")).unwrap();

    let tool = ReadTool::new(tmp.path().to_path_buf());
    let result = tool.execute(serde_json::json!({"path": "code.rs"}), &noop_cancel());
    assert!(!result.is_error);

    let tokens = approx_tokens(&result);
    let source_tokens = lines.join("\n").len() / 4;
    // Line numbers add overhead, but should be <2x the source
    assert!(
        tokens < source_tokens * 2,
        "read output too bloated: {} tokens for {} source tokens",
        tokens,
        source_tokens,
    );
}

// ── Write tool ──

#[test]
fn write_overwrites_existing() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("test.txt");
    std::fs::write(&file, "old content").unwrap();

    let tool = WriteTool::new(tmp.path().to_path_buf());
    let result = tool
        .execute(serde_json::json!({"path": "test.txt", "content": "new content"}), &noop_cancel());
    assert!(!result.is_error);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "new content");
}

#[test]
fn write_empty_content() {
    let tmp = TempDir::new().unwrap();

    let tool = WriteTool::new(tmp.path().to_path_buf());
    let result =
        tool.execute(serde_json::json!({"path": "empty.txt", "content": ""}), &noop_cancel());
    assert!(!result.is_error);
    assert_eq!(std::fs::read_to_string(tmp.path().join("empty.txt")).unwrap(), "");
}

#[test]
fn write_unicode_content() {
    let tmp = TempDir::new().unwrap();

    let tool = WriteTool::new(tmp.path().to_path_buf());
    let result = tool
        .execute(serde_json::json!({"path": "uni.txt", "content": "héllo 世界\n"}), &noop_cancel());
    assert!(!result.is_error);
    assert!(std::fs::read_to_string(tmp.path().join("uni.txt")).unwrap().contains("世界"));
}

#[test]
fn write_deeply_nested_path() {
    let tmp = TempDir::new().unwrap();

    let tool = WriteTool::new(tmp.path().to_path_buf());
    let result = tool
        .execute(serde_json::json!({"path": "a/b/c/d/e.txt", "content": "deep"}), &noop_cancel());
    assert!(!result.is_error);
    assert_eq!(std::fs::read_to_string(tmp.path().join("a/b/c/d/e.txt")).unwrap(), "deep");
}

#[test]
fn write_absolute_path() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("abs.txt");

    let tool = WriteTool::new(tmp.path().to_path_buf());
    let result = tool.execute(
        serde_json::json!({"path": file.to_str().unwrap(), "content": "abs"}),
        &noop_cancel(),
    );
    assert!(!result.is_error);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "abs");
}

// ── File mutation queue ──

#[test]
fn mutation_queue_serializes_same_file() {
    use std::sync::atomic::{AtomicU32, Ordering};

    let mq = Arc::new(FileMutationQueue::new());
    let counter = Arc::new(AtomicU32::new(0));
    let path = PathBuf::from("/tmp/nerv-test-mq");

    let mut handles = Vec::new();
    for _ in 0..10 {
        let mq = mq.clone();
        let counter = counter.clone();
        let path = path.clone();
        handles.push(std::thread::spawn(move || {
            mq.with(&path, || {
                let val = counter.load(Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(1));
                counter.store(val + 1, Ordering::SeqCst);
            });
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    // Without serialization, concurrent read-modify-write would lose increments
    assert_eq!(counter.load(Ordering::SeqCst), 10);
}

#[test]
fn mutation_queue_different_files_parallel() {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Instant;

    let mq = Arc::new(FileMutationQueue::new());
    let counter = Arc::new(AtomicU32::new(0));
    let start = Instant::now();

    let mut handles = Vec::new();
    for i in 0..4 {
        let mq = mq.clone();
        let counter = counter.clone();
        handles.push(std::thread::spawn(move || {
            let path = PathBuf::from(format!("/tmp/nerv-test-mq-{}", i));
            mq.with(&path, || {
                std::thread::sleep(std::time::Duration::from_millis(25));
                counter.fetch_add(1, Ordering::SeqCst);
            });
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(counter.load(Ordering::SeqCst), 4);
    // Should complete in ~25ms (parallel), not ~100ms (serial).
    // Allow generous headroom for slow/loaded CI runners.
    assert!(start.elapsed().as_millis() < 500, "different files should run in parallel");
}

// ── Diff token efficiency ──

#[test]
fn diff_output_is_compact() {
    // Single-line change in a 50-line file. Diff should include ~7 lines
    // (3 context + 1 old + 1 new + header), not 50.
    let old: String = (1..=50).map(|i| format!("line {}", i)).collect::<Vec<_>>().join("\n");
    let new = old.replace("line 25", "CHANGED 25");
    let diff = nerv::tools::diff::unified_diff(&old, &new, "a/f", "b/f");

    let diff_lines = diff.lines().count();
    assert!(
        diff_lines < 15,
        "diff too verbose for single-line change: {} lines\n{}",
        diff_lines,
        diff,
    );
}

#[test]
fn diff_no_change_is_minimal() {
    let text = "unchanged\n".repeat(100);
    let diff = nerv::tools::diff::unified_diff(&text, &text, "a/f", "b/f");
    // Just the header, no hunks
    assert_eq!(diff.lines().count(), 2, "no-change diff should be header only: {}", diff);
}

#[test]
fn edit_single_output_token_efficiency() {
    let tmp = TempDir::new().unwrap();
    // 200-line file, change one line
    let lines: Vec<String> = (1..=200).map(|i| format!("fn func_{}() {{}}", i)).collect();
    std::fs::write(tmp.path().join("big.rs"), lines.join("\n")).unwrap();

    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mq);
    let result = tool.execute(
        serde_json::json!({
            "path": "big.rs",
            "old_text": "fn func_100() {}",
            "new_text": "fn func_100_renamed() {}"
        }),
        &noop_cancel(),
    );
    assert!(!result.is_error, "{}", result.content);

    let tokens = approx_tokens(&result);
    // A single-line edit in a 200-line file: header + compact diff with 3 lines
    // context
    assert!(
        tokens < 80,
        "edit output too bloated for single-line change: {} tokens\n{}",
        tokens,
        result.content,
    );
}

#[test]
fn edit_multi_output_token_efficiency() {
    let tmp = TempDir::new().unwrap();
    let lines: Vec<String> = (1..=200).map(|i| format!("let var_{} = {};", i, i)).collect();
    std::fs::write(tmp.path().join("big.rs"), lines.join("\n")).unwrap();

    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mq);
    let result = tool.execute(
        serde_json::json!({
            "path": "big.rs",
            "edits": [
                {"old_text": "let var_10 = 10;", "new_text": "let var_10 = 100;"},
                {"old_text": "let var_190 = 190;", "new_text": "let var_190 = 1900;"}
            ]
        }),
        &noop_cancel(),
    );
    assert!(!result.is_error, "{}", result.content);

    let tokens = approx_tokens(&result);
    // Two small edits far apart — should produce 2 hunks, <120 tokens
    assert!(tokens < 120, "multi-edit output too bloated: {} tokens\n{}", tokens, result.content,);
}

#[test]
fn edit_content_includes_diff() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("test.rs"), "fn main() {\n    println!(\"old\");\n}\n").unwrap();

    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mq);
    let result = tool.execute(
        serde_json::json!({
            "path": "test.rs",
            "old_text": "println!(\"old\")",
            "new_text": "println!(\"new\")"
        }),
        &noop_cancel(),
    );

    assert!(!result.is_error);
    // Content sent to LLM should include the diff, not just "Edited test.rs"
    assert!(
        result.content.contains('-') && result.content.contains('+'),
        "edit content should include diff hunks, got: {}",
        result.content
    );
    assert!(result.content.contains("old"), "diff should show removed line");
    assert!(result.content.contains("new"), "diff should show added line");
}

#[test]
fn edit_content_diff_is_compact() {
    let tmp = TempDir::new().unwrap();
    let lines: Vec<String> = (1..=100).map(|i| format!("line {}", i)).collect();
    std::fs::write(tmp.path().join("big.txt"), lines.join("\n")).unwrap();

    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mq);
    let result = tool.execute(
        serde_json::json!({
            "path": "big.txt",
            "old_text": "line 50",
            "new_text": "line FIFTY"
        }),
        &noop_cancel(),
    );

    assert!(!result.is_error);
    // Should NOT contain the full 100-line file — just a compact diff
    assert!(!result.content.contains("line 1\n"), "content should not include full file");
    assert!(result.content.contains("FIFTY"), "content should include the changed text");
    // Should be compact: header + a few context lines + change
    let line_count = result.content.lines().count();
    assert!(
        line_count < 20,
        "compact diff should be <20 lines, got {}:\n{}",
        line_count,
        result.content,
    );
}

#[test]
fn edit_multi_content_includes_diff() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("multi.rs"), "let a = 1;\nlet b = 2;\nlet c = 3;\n").unwrap();

    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mq);
    let result = tool.execute(
        serde_json::json!({
            "path": "multi.rs",
            "edits": [
                {"old_text": "let a = 1;", "new_text": "let a = 10;"},
                {"old_text": "let c = 3;", "new_text": "let c = 30;"}
            ]
        }),
        &noop_cancel(),
    );

    assert!(!result.is_error);
    assert!(
        result.content.contains('-') && result.content.contains('+'),
        "multi-edit content should include diff hunks, got: {}",
        result.content
    );
}

#[test]
fn edit_fuzzy_content_includes_diff() {
    let tmp = TempDir::new().unwrap();
    // Smart quotes in file trigger fuzzy matching when model sends ASCII quotes
    std::fs::write(
        tmp.path().join("fuzzy.rs"),
        "fn main() {\n    println!(\u{201C}hello\u{201D});\n}\n",
    )
    .unwrap();

    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mq);
    let result = tool.execute(
        serde_json::json!({
            "path": "fuzzy.rs",
            "old_text": "println!(\"hello\")",
            "new_text": "println!(\"world\")"
        }),
        &noop_cancel(),
    );

    assert!(!result.is_error, "{}", result.content);
    assert!(
        result.content.contains('-') && result.content.contains('+'),
        "fuzzy edit content should include diff hunks, got: {}",
        result.content
    );
}

#[test]
fn symbols_tool_finds_definitions() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("lib.rs"),
        "pub struct Config;\n\nimpl Config {\n    pub fn load() -> Self { Config }\n}\n\nfn helper() {}\n",
    )
    .unwrap();

    let tool = SymbolsTool::new(tmp.path().to_path_buf());

    // Search by type name
    let result = tool.execute(serde_json::json!({"query": "Config"}), &noop_cancel());
    assert!(!result.is_error);
    assert!(result.content.contains("struct"), "should find struct: {}", result.content);

    // Search by method name — should show parent impl
    let result = tool.execute(serde_json::json!({"query": "load"}), &noop_cancel());
    assert!(!result.is_error);
    assert!(result.content.contains("fn"), "should find method: {}", result.content);
    assert!(result.content.contains("impl Config"), "should show parent: {}", result.content);
}

#[test]
fn symbols_tool_kind_filter() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("lib.rs"), "fn foo() {}\nstruct Foo;\n").unwrap();

    let tool = SymbolsTool::new(tmp.path().to_path_buf());
    let result =
        tool.execute(serde_json::json!({"query": "foo", "kind": "function"}), &noop_cancel());
    assert!(!result.is_error);
    assert!(result.content.contains("fn foo"), "{}", result.content);
    assert!(
        !result.content.contains("struct Foo"),
        "struct should be filtered out: {}",
        result.content
    );
}

#[test]
fn symbols_tool_no_results() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("lib.rs"), "fn hello() {}\n").unwrap();

    let tool = SymbolsTool::new(tmp.path().to_path_buf());
    let result = tool.execute(serde_json::json!({"query": "nonexistent"}), &noop_cancel());
    assert!(!result.is_error);
    assert!(result.content.contains("No definitions found"), "{}", result.content);
}

fn codemap_tool(tmp: &TempDir) -> CodemapTool {
    use std::sync::RwLock;
    let index = Arc::new(RwLock::new(nerv::index::SymbolIndex::new()));
    CodemapTool::new(tmp.path().to_path_buf(), index)
}

#[test]
fn codemap_tool_full_depth() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("lib.rs"), "fn hello() {\n    println!(\"world\");\n}\n")
        .unwrap();

    let tool = codemap_tool(&tmp);
    let result =
        tool.execute(serde_json::json!({"query": "hello", "depth": "full"}), &noop_cancel());
    assert!(!result.is_error);
    assert!(result.content.contains("fn hello()"), "{}", result.content);
    assert!(result.content.contains("println!"), "should contain body: {}", result.content);
}

#[test]
fn codemap_tool_signatures_depth() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("lib.rs"), "fn hello() {\n    println!(\"world\");\n}\n")
        .unwrap();

    let tool = codemap_tool(&tmp);
    let result =
        tool.execute(serde_json::json!({"query": "hello", "depth": "signatures"}), &noop_cancel());
    assert!(!result.is_error);
    assert!(result.content.contains("fn hello()"), "{}", result.content);
    assert!(!result.content.contains("println!"), "should NOT contain body: {}", result.content);
}

#[test]
fn codemap_tool_no_results() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("lib.rs"), "fn hello() {}\n").unwrap();

    let tool = codemap_tool(&tmp);
    let result = tool.execute(serde_json::json!({"query": "nonexistent"}), &noop_cancel());
    assert!(!result.is_error);
    // Non-empty query with definitions in scope → redirect message
    assert!(result.content.contains("No symbols matching"), "{}", result.content);
}

#[test]
fn codemap_tool_kind_filter() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("lib.rs"), "struct Foo;\nfn bar() {}\n").unwrap();

    let tool = codemap_tool(&tmp);
    let result = tool.execute(
        serde_json::json!({"query": "", "kind": "struct", "depth": "full"}),
        &noop_cancel(),
    );
    assert!(!result.is_error);
    assert!(result.content.contains("Foo"), "{}", result.content);
    assert!(!result.content.contains("bar"), "should not contain fn: {}", result.content);
}
