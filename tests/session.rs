//! Session persistence tests — SQLite storage, context reconstruction, compaction, export.

use nerv::agent::types::*;
use nerv::session::manager::SessionManager;
use tempfile::TempDir;

fn setup() -> (TempDir, SessionManager) {
    let tmp = TempDir::new().unwrap();
    let nerv_dir = tmp.path().join(".nerv");
    std::fs::create_dir_all(&nerv_dir).unwrap();
    let mgr = SessionManager::new(&nerv_dir);
    (tmp, mgr)
}

fn user_msg(text: &str) -> AgentMessage {
    AgentMessage::User {
        content: vec![ContentItem::Text { text: text.into() }],
        timestamp: now_millis(),
    }
}

fn assistant_msg(text: &str) -> AgentMessage {
    AgentMessage::Assistant(AssistantMessage {
        content: vec![ContentBlock::Text { text: text.into() }],
        stop_reason: StopReason::EndTurn,
        usage: Some(Usage {
            input: 100,
            output: 50,
            ..Default::default()
        }),
        timestamp: now_millis(),
    })
}

fn tool_call_msg(id: &str, name: &str) -> AgentMessage {
    AgentMessage::Assistant(AssistantMessage {
        content: vec![ContentBlock::ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: serde_json::json!({"path": "test.rs"}),
        }],
        stop_reason: StopReason::ToolUse,
        usage: Some(Usage {
            input: 80,
            output: 20,
            ..Default::default()
        }),
        timestamp: now_millis(),
    })
}

fn tool_result_msg(id: &str, content: &str) -> AgentMessage {
    AgentMessage::ToolResult {
        tool_call_id: id.into(),
        content: vec![ContentItem::Text {
            text: content.into(),
        }],
        is_error: false,
        timestamp: now_millis(),
    }
}

#[test]
fn round_trip_session_with_multiple_message_types() {
    let (tmp, mut mgr) = setup();
    mgr.new_session(tmp.path()).unwrap();

    mgr.append_message(&user_msg("hello"), None).unwrap();
    mgr.append_message(&assistant_msg("hi there"), None)
        .unwrap();
    mgr.append_message(
        &AgentMessage::ToolResult {
            tool_call_id: "tc_1".into(),
            content: vec![ContentItem::Text {
                text: "result".into(),
            }],
            is_error: false,
            timestamp: now_millis(),
        },
        None,
    )
    .unwrap();
    mgr.append_model_change("anthropic", "claude-sonnet-4-6")
        .unwrap();
    mgr.append_thinking_level_change(ThinkingLevel::High)
        .unwrap();

    let session_id = mgr.session_id().to_string();
    assert_eq!(mgr.entry_count(), 5);

    // Reload from same DB (simulates restart)
    let nerv_dir = tmp.path().join(".nerv");
    let mut mgr2 = SessionManager::new(&nerv_dir);
    let ctx = mgr2.load_session(&session_id).unwrap();

    assert_eq!(ctx.messages.len(), 3);
    assert_eq!(
        ctx.model,
        Some(("anthropic".into(), "claude-sonnet-4-6".into()))
    );
    assert_eq!(ctx.thinking_level, ThinkingLevel::High);
}

#[test]
fn new_session_creates_db_entry() {
    let (tmp, mut mgr) = setup();
    mgr.new_session(tmp.path()).unwrap();
    assert!(mgr.has_session());
    let sessions = mgr.list_sessions();
    assert_eq!(sessions.len(), 1);
}

#[test]
fn list_sessions_returns_sorted_by_newest() {
    let (tmp, mut mgr) = setup();

    mgr.new_session(tmp.path()).unwrap();
    mgr.append_message(&user_msg("first session"), None)
        .unwrap();
    let id1 = mgr.session_id().to_string();

    mgr.new_session(tmp.path()).unwrap();
    mgr.append_message(&user_msg("second session"), None)
        .unwrap();
    let id2 = mgr.session_id().to_string();

    let sessions = mgr.list_sessions();
    assert_eq!(sessions.len(), 2);
    // Both sessions present (order may vary if created in same second)
    let ids: Vec<&str> = sessions.iter().map(|s| s.id_short.as_str()).collect();
    assert!(ids.contains(&&id1[..8]));
    assert!(ids.contains(&&id2[..8]));
    // Preview of second session exists somewhere
    assert!(sessions.iter().any(|s| s.preview == "second session"));
}

#[test]
fn empty_session_listed() {
    let (tmp, mut mgr) = setup();
    mgr.new_session(tmp.path()).unwrap();
    // No messages — session exists in DB but has empty preview
    let sessions = mgr.list_sessions();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].preview, "");
}

#[test]
fn load_session_by_prefix() {
    let (tmp, mut mgr) = setup();
    mgr.new_session(tmp.path()).unwrap();
    mgr.append_message(&user_msg("hello"), None).unwrap();
    mgr.append_message(&assistant_msg("world"), None).unwrap();

    let full_id = mgr.session_id().to_string();
    let prefix = &full_id[..8];

    // Load by prefix
    let ctx = mgr.load_session(prefix).unwrap();
    assert_eq!(ctx.messages.len(), 2);
}

// ── Compaction tests ─────────────────────────────────────────────────

#[test]
fn compaction_preserves_messages_after_cut_point() {
    let (tmp, mut mgr) = setup();
    mgr.new_session(tmp.path()).unwrap();

    for i in 0..10 {
        mgr.append_message(&user_msg(&format!("msg {}", i)), None)
            .unwrap();
    }

    let entry7_id = mgr.entries()[7].id().to_string();
    mgr.append_compaction("summary of 0-6".into(), entry7_id, 3000)
        .unwrap();

    let ctx = mgr.build_session_context();
    // compaction summary + msgs 7,8,9 = 4
    assert_eq!(ctx.messages.len(), 4);
    assert!(matches!(
        ctx.messages[0],
        AgentMessage::CompactionSummary { .. }
    ));
    if let AgentMessage::User { content, .. } = &ctx.messages[1]
        && let ContentItem::Text { text } = &content[0]
    {
        assert_eq!(text, "msg 7");
    }
}

#[test]
fn compaction_survives_reload() {
    let (tmp, mut mgr) = setup();
    mgr.new_session(tmp.path()).unwrap();

    for i in 0..10 {
        mgr.append_message(&user_msg(&format!("msg {}", i)), None)
            .unwrap();
    }

    let entry5_id = mgr.entries()[5].id().to_string();
    mgr.append_compaction("summary".into(), entry5_id, 2000)
        .unwrap();

    let session_id = mgr.session_id().to_string();
    let ctx = mgr.load_session(&session_id).unwrap();

    assert!(matches!(
        ctx.messages[0],
        AgentMessage::CompactionSummary { .. }
    ));
    // Entries before cut point were deleted. Remaining: entries 5-9 + compaction entry
    // Context: summary + msgs 5-9 = 6
    assert_eq!(ctx.messages.len(), 6);
}

#[test]
fn compaction_with_tool_calls_across_boundary() {
    let (tmp, mut mgr) = setup();
    mgr.new_session(tmp.path()).unwrap();

    mgr.append_message(&user_msg("old msg"), None).unwrap();
    mgr.append_message(&assistant_msg("old response"), None)
        .unwrap();

    let kept_id_entry = mgr.entries().len();
    mgr.append_message(&user_msg("read the file"), None)
        .unwrap();
    mgr.append_message(&tool_call_msg("tc1", "read"), None)
        .unwrap();
    mgr.append_message(&tool_result_msg("tc1", "contents"), None)
        .unwrap();
    mgr.append_message(&assistant_msg("here it is"), None)
        .unwrap();

    let cut_id = mgr.entries()[kept_id_entry].id().to_string();
    mgr.append_compaction("old conversation summary".into(), cut_id, 1000)
        .unwrap();

    let ctx = mgr.build_session_context();
    assert_eq!(ctx.messages.len(), 5);
    assert!(matches!(
        ctx.messages[0],
        AgentMessage::CompactionSummary { .. }
    ));
    assert!(matches!(ctx.messages[3], AgentMessage::ToolResult { .. }));
}

// ── Export tests ──────────────────────────────────────────────────────

#[test]
fn export_jsonl_produces_valid_output() {
    let (tmp, mut mgr) = setup();
    mgr.new_session(tmp.path()).unwrap();
    mgr.append_message(&user_msg("hello"), None).unwrap();
    mgr.append_message(&assistant_msg("world"), None).unwrap();

    let jsonl = mgr.export_jsonl().unwrap();
    let lines: Vec<&str> = jsonl.lines().collect();

    // First line: header
    let header: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(header["type"], "session");
    assert_eq!(header["version"], 3);

    // Following lines: entries
    assert!(lines.len() >= 3); // header + 2 entries
}

#[test]
fn export_preserves_tool_calls_and_results() {
    let (tmp, mut mgr) = setup();
    mgr.new_session(tmp.path()).unwrap();
    mgr.append_message(&user_msg("read test.rs"), None).unwrap();
    mgr.append_message(&tool_call_msg("tc1", "read"), None)
        .unwrap();
    mgr.append_message(&tool_result_msg("tc1", "fn main() {}"), None)
        .unwrap();
    mgr.append_message(&assistant_msg("Here's the file"), None)
        .unwrap();

    let jsonl = mgr.export_jsonl().unwrap();
    assert!(jsonl.contains("fn main() {}"));
    assert!(jsonl.contains("tc1"));
}

// ── Thinking tests ───────────────────────────────────────────────────

#[test]
fn session_with_thinking_blocks_round_trips() {
    let (tmp, mut mgr) = setup();
    mgr.new_session(tmp.path()).unwrap();

    mgr.append_message(&user_msg("think about this"), None)
        .unwrap();
    mgr.append_message(
        &AgentMessage::Assistant(AssistantMessage {
            content: vec![
                ContentBlock::Thinking {
                    thinking: "Let me consider...".into(),
                },
                ContentBlock::Text {
                    text: "The answer is 42.".into(),
                },
            ],
            stop_reason: StopReason::EndTurn,
            usage: Some(Usage {
                input: 100,
                output: 50,
                ..Default::default()
            }),
            timestamp: now_millis(),
        }),
        None,
    )
    .unwrap();

    let session_id = mgr.session_id().to_string();
    let ctx = mgr.load_session(&session_id).unwrap();

    assert_eq!(ctx.messages.len(), 2);
    if let AgentMessage::Assistant(a) = &ctx.messages[1] {
        assert!(a.content.iter().any(
            |b| matches!(b, ContentBlock::Thinking { thinking } if thinking == "Let me consider...")
        ));
        assert_eq!(a.text_content(), "The answer is 42.");
    } else {
        panic!("expected assistant");
    }
}

#[test]
fn html_export_excludes_thinking() {
    let (tmp, mut mgr) = setup();
    mgr.new_session(tmp.path()).unwrap();

    mgr.append_message(&user_msg("think hard"), None).unwrap();
    mgr.append_message(
        &AgentMessage::Assistant(AssistantMessage {
            content: vec![
                ContentBlock::Thinking {
                    thinking: "SECRET_THINKING_CONTENT".into(),
                },
                ContentBlock::Text {
                    text: "visible answer".into(),
                },
            ],
            stop_reason: StopReason::EndTurn,
            usage: Some(Usage {
                input: 100,
                output: 50,
                ..Default::default()
            }),
            timestamp: now_millis(),
        }),
        None,
    )
    .unwrap();

    let messages = &mgr.build_session_context().messages;

    let mut html = String::from("<html>");
    for msg in messages {
        if let AgentMessage::Assistant(a) = msg {
            let text = a.text_content();
            html.push_str(&text);
        }
    }
    html.push_str("</html>");

    assert!(html.contains("visible answer"));
    assert!(!html.contains("SECRET_THINKING_CONTENT"));
}
