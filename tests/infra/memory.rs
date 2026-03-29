use std::sync::Arc;

use nerv::agent::agent::{AgentTool, UpdateCallback};
use nerv::agent::provider::{CancelFlag, new_cancel_flag};
use nerv::tools::MemoryTool;
use tempfile::TempDir;

fn setup() -> (TempDir, MemoryTool) {
    let tmp = TempDir::new().unwrap();
    let tool = MemoryTool::new(tmp.path().to_path_buf());
    (tmp, tool)
}

fn noop_update() -> UpdateCallback {
    Arc::new(|_| {})
}

fn noop_cancel() -> CancelFlag {
    new_cancel_flag()
}

#[test]
fn list_empty_memories() {
    let (_tmp, tool) = setup();
    let result = tool.execute(serde_json::json!({"action": "list"}), noop_update(), &noop_cancel());
    assert!(!result.is_error);
    assert!(result.content.contains("No memories"));
}

#[test]
fn add_and_list_memory() {
    let (_tmp, tool) = setup();
    let result = tool.execute(
        serde_json::json!({"action": "add", "content": "User prefers Rust"}),
        noop_update(),
        &noop_cancel(),
    );
    assert!(!result.is_error);
    assert!(result.content.contains("Memory added"));

    let result = tool.execute(serde_json::json!({"action": "list"}), noop_update(), &noop_cancel());
    assert!(result.content.contains("User prefers Rust"));
    assert!(result.content.contains("1."));
}

#[test]
fn add_multiple_and_list() {
    let (_tmp, tool) = setup();
    tool.execute(
        serde_json::json!({"action": "add", "content": "first"}),
        noop_update(),
        &noop_cancel(),
    );
    tool.execute(
        serde_json::json!({"action": "add", "content": "second"}),
        noop_update(),
        &noop_cancel(),
    );
    let result = tool.execute(serde_json::json!({"action": "list"}), noop_update(), &noop_cancel());
    assert!(result.content.contains("1. first"));
    assert!(result.content.contains("2. second"));
}

#[test]
fn remove_memory() {
    let (_tmp, tool) = setup();
    tool.execute(
        serde_json::json!({"action": "add", "content": "keep this"}),
        noop_update(),
        &noop_cancel(),
    );
    tool.execute(
        serde_json::json!({"action": "add", "content": "remove this"}),
        noop_update(),
        &noop_cancel(),
    );
    let result = tool.execute(
        serde_json::json!({"action": "remove", "content": "2"}),
        noop_update(),
        &noop_cancel(),
    );
    assert!(!result.is_error);
    assert!(result.content.contains("Removed"));

    let result = tool.execute(serde_json::json!({"action": "list"}), noop_update(), &noop_cancel());
    assert!(result.content.contains("keep this"));
    assert!(!result.content.contains("remove this"));
}

#[test]
fn remove_invalid_index() {
    let (_tmp, tool) = setup();
    let result = tool.execute(
        serde_json::json!({"action": "remove", "content": "99"}),
        noop_update(),
        &noop_cancel(),
    );
    assert!(result.is_error);
}

#[test]
fn add_compresses_multiline_to_single() {
    let (_tmp, tool) = setup();
    tool.execute(
        serde_json::json!({"action": "add", "content": "line one\nline two"}),
        noop_update(),
        &noop_cancel(),
    );
    let result = tool.execute(serde_json::json!({"action": "list"}), noop_update(), &noop_cancel());
    // Should be on one line (newlines replaced with spaces)
    assert!(result.content.contains("line one line two"));
}

#[test]
fn validate_rejects_bad_action() {
    let (_tmp, tool) = setup();
    let result = tool.validate(&serde_json::json!({"action": "destroy"}));
    assert!(result.is_err());
}

#[test]
fn validate_rejects_empty_add() {
    let (_tmp, tool) = setup();
    let result = tool.validate(&serde_json::json!({"action": "add", "content": ""}));
    assert!(result.is_err());
}

#[test]
fn memories_persist_to_file() {
    let (tmp, tool) = setup();
    tool.execute(
        serde_json::json!({"action": "add", "content": "persistent"}),
        noop_update(),
        &noop_cancel(),
    );

    // Read the file directly
    let content = std::fs::read_to_string(tmp.path().join("memory.md")).unwrap();
    assert!(content.contains("persistent"));
}
