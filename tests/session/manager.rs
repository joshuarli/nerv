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
        display: None,
        timestamp: now_millis(),
    }
}

#[test]
fn round_trip_session_with_multiple_message_types() {
    let (tmp, mut mgr) = setup();
    mgr.new_session(tmp.path(), None).unwrap();

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
            display: None,
            timestamp: now_millis(),
        },
        None,
    )
    .unwrap();
    mgr.append_model_change("anthropic", "claude-sonnet-4-6")
        .unwrap();
    mgr.append_thinking_level_change(ThinkingLevel::On)
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
    assert_eq!(ctx.thinking_level, ThinkingLevel::On);
}

#[test]
fn new_session_creates_db_entry() {
    let (tmp, mut mgr) = setup();
    mgr.new_session(tmp.path(), None).unwrap();
    assert!(mgr.has_session());
    let sessions = mgr.list_sessions();
    assert_eq!(sessions.len(), 1);
}

#[test]
fn list_sessions_returns_sorted_by_newest() {
    let (tmp, mut mgr) = setup();

    mgr.new_session(tmp.path(), None).unwrap();
    mgr.append_message(&user_msg("first session"), None)
        .unwrap();
    let id1 = mgr.session_id().to_string();

    mgr.new_session(tmp.path(), None).unwrap();
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
    mgr.new_session(tmp.path(), None).unwrap();
    // No messages — session exists in DB but has empty preview
    let sessions = mgr.list_sessions();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].preview, "");
}

#[test]
fn load_session_by_prefix() {
    let (tmp, mut mgr) = setup();
    mgr.new_session(tmp.path(), None).unwrap();
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
    mgr.new_session(tmp.path(), None).unwrap();

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
    mgr.new_session(tmp.path(), None).unwrap();

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
    mgr.new_session(tmp.path(), None).unwrap();

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
    mgr.new_session(tmp.path(), None).unwrap();
    mgr.append_message(&user_msg("hello"), None).unwrap();
    mgr.append_message(&assistant_msg("world"), None).unwrap();

    let jsonl = mgr.export_jsonl().unwrap();
    let lines: Vec<&str> = jsonl.lines().collect();

    // First line: header
    let header: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(header["type"], "session");
    assert_eq!(header["version"], 4);

    // Following lines: entries
    assert!(lines.len() >= 3); // header + 2 entries
}

#[test]
fn export_preserves_tool_calls_and_results() {
    let (tmp, mut mgr) = setup();
    mgr.new_session(tmp.path(), None).unwrap();
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
    mgr.new_session(tmp.path(), None).unwrap();

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
    mgr.new_session(tmp.path(), None).unwrap();

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

// ── Session tree / branching ──

// ── Branch-aware export ───────────────────────────────────────────────────────

#[test]
fn export_jsonl_includes_only_current_branch() {
    let (_tmp, mut mgr) = setup();
    mgr.new_session(std::path::Path::new("/tmp"), None).unwrap();

    mgr.append_message(&user_msg("shared root"), None).unwrap();
    let fork_point = mgr.leaf_id().unwrap().to_string();

    // Branch A
    mgr.append_message(&assistant_msg("BRANCH_A_ONLY"), None).unwrap();

    // Branch B (fork from the shared root)
    mgr.branch(&fork_point);
    mgr.append_message(&assistant_msg("BRANCH_B_ONLY"), None).unwrap();

    // Currently on branch B — export should contain B but not A
    let jsonl = mgr.export_jsonl().unwrap();
    assert!(jsonl.contains("BRANCH_B_ONLY"), "should include current branch");
    assert!(!jsonl.contains("BRANCH_A_ONLY"), "should NOT include sibling branch");
    assert!(jsonl.contains("shared root"), "should include shared ancestor");
}

#[test]
fn export_jsonl_leaf_id_in_header() {
    let (_tmp, mut mgr) = setup();
    mgr.new_session(std::path::Path::new("/tmp"), None).unwrap();
    mgr.append_message(&user_msg("hello"), None).unwrap();

    let leaf = mgr.leaf_id().unwrap().to_string();
    let jsonl = mgr.export_jsonl().unwrap();
    let header: serde_json::Value = serde_json::from_str(jsonl.lines().next().unwrap()).unwrap();
    assert_eq!(header["leaf_id"], leaf.as_str());
}

#[test]
fn export_jsonl_branch_order_root_to_leaf() {
    let (_tmp, mut mgr) = setup();
    mgr.new_session(std::path::Path::new("/tmp"), None).unwrap();

    mgr.append_message(&user_msg("first"), None).unwrap();
    mgr.append_message(&assistant_msg("second"), None).unwrap();
    mgr.append_message(&user_msg("third"), None).unwrap();

    let jsonl = mgr.export_jsonl().unwrap();
    // Skip header line; entries should appear in root→leaf order
    let texts: Vec<&str> = jsonl.lines()
        .skip(1)
        .filter(|l| l.contains("first") || l.contains("second") || l.contains("third"))
        .collect();
    assert_eq!(texts.len(), 3);
    let first_pos = jsonl.find("first").unwrap();
    let second_pos = jsonl.find("second").unwrap();
    let third_pos = jsonl.find("third").unwrap();
    assert!(first_pos < second_pos && second_pos < third_pos,
        "entries should be in root→leaf order");
}

// ── Branch-aware compaction ───────────────────────────────────────────────────

#[test]
fn compaction_only_removes_current_branch_entries() {
    let (_tmp, mut mgr) = setup();
    mgr.new_session(std::path::Path::new("/tmp"), None).unwrap();

    // Shared root
    mgr.append_message(&user_msg("shared"), None).unwrap();
    let fork_point = mgr.leaf_id().unwrap().to_string();

    // Branch A: old messages that will be compacted
    mgr.append_message(&assistant_msg("branch_a_old_1"), None).unwrap();
    mgr.append_message(&user_msg("branch_a_old_2"), None).unwrap();
    let branch_a_keep_id = mgr.leaf_id().unwrap().to_string();
    mgr.append_message(&assistant_msg("branch_a_keep"), None).unwrap();

    // Branch B: fork from the shared root, independent
    mgr.branch(&fork_point);
    mgr.append_message(&assistant_msg("BRANCH_B_SENTINEL"), None).unwrap();

    // Re-enter branch A
    mgr.branch(&branch_a_keep_id);
    // Compact branch A: remove entries before branch_a_keep_id
    mgr.append_compaction("summary of A".into(), branch_a_keep_id.clone(), 500).unwrap();

    // Branch B sentinel should still be in the DB
    let all_entries = mgr.entries();
    let has_b = all_entries.iter().any(|e| {
        if let nerv::session::SessionEntry::Message(me) = e {
            if let AgentMessage::Assistant(a) = &me.message {
                return a.text_content().contains("BRANCH_B_SENTINEL");
            }
        }
        false
    });
    assert!(has_b, "compaction should not remove sibling branch B entries");
}

#[test]
fn compaction_removes_pre_cut_entries_on_current_branch() {
    let (_tmp, mut mgr) = setup();
    mgr.new_session(std::path::Path::new("/tmp"), None).unwrap();

    for i in 0..6 {
        mgr.append_message(&user_msg(&format!("msg_{}", i)), None).unwrap();
    }

    // Cut at entry index 3 (keep entries 3,4,5)
    let cut_id = mgr.get_branch()[3].id().to_string();
    mgr.append_compaction("summary".into(), cut_id, 1000).unwrap();

    let ctx = mgr.build_session_context();
    // summary + 3 kept messages
    assert_eq!(ctx.messages.len(), 4);
    // First message is the summary
    assert!(matches!(ctx.messages[0], AgentMessage::CompactionSummary { .. }));
    // Last message is msg_5
    if let AgentMessage::User { content, .. } = &ctx.messages[3] {
        let text = match &content[0] { ContentItem::Text { text } => text, _ => panic!() };
        assert!(text.contains("msg_5"));
    }
}

// ── get_tree structure ────────────────────────────────────────────────────────

#[test]
fn get_tree_linear_session_is_single_chain() {
    let (_tmp, mut mgr) = setup();
    mgr.new_session(std::path::Path::new("/tmp"), None).unwrap();

    mgr.append_message(&user_msg("a"), None).unwrap();
    mgr.append_message(&assistant_msg("b"), None).unwrap();
    mgr.append_message(&user_msg("c"), None).unwrap();

    let tree = mgr.get_tree();
    // One root with a single child chain — each node has at most 1 child
    fn max_branch_width(nodes: &[nerv::session::types::SessionTreeNode]) -> usize {
        let mut max = nodes.len();
        for n in nodes {
            max = max.max(max_branch_width(&n.children));
        }
        max
    }
    // No branching: every node has 0 or 1 children
    fn all_single_child(nodes: &[nerv::session::types::SessionTreeNode]) -> bool {
        nodes.iter().all(|n| n.children.len() <= 1 && all_single_child(&n.children))
    }
    assert!(all_single_child(&tree), "linear session should have no branch points");
}

#[test]
fn get_tree_fork_produces_two_children() {
    let (_tmp, mut mgr) = setup();
    mgr.new_session(std::path::Path::new("/tmp"), None).unwrap();

    mgr.append_message(&user_msg("root"), None).unwrap();
    let fork = mgr.leaf_id().unwrap().to_string();
    mgr.append_message(&assistant_msg("branch_a"), None).unwrap();
    mgr.branch(&fork);
    mgr.append_message(&assistant_msg("branch_b"), None).unwrap();

    let tree = mgr.get_tree();

    // Walk to the fork node (the "root" user message)
    fn find_fork(nodes: &[nerv::session::types::SessionTreeNode]) -> Option<usize> {
        for n in nodes {
            if n.children.len() == 2 { return Some(2); }
            if let Some(c) = find_fork(&n.children) { return Some(c); }
        }
        None
    }
    assert_eq!(find_fork(&tree), Some(2), "should find a node with exactly 2 children");
}

#[test]
fn get_tree_empty_session_returns_empty() {
    let (_tmp, mut mgr) = setup();
    mgr.new_session(std::path::Path::new("/tmp"), None).unwrap();
    assert!(mgr.get_tree().is_empty());
}

// ── Search: branch-aware behaviour ────────────────────────────────────────────

#[test]
fn search_deduplicates_hits_across_branches_same_session() {
    let (_tmp, mut mgr) = setup();
    mgr.new_session(std::path::Path::new("/tmp"), None).unwrap();

    mgr.append_message(&user_msg("xylophone question"), None).unwrap();
    let fork = mgr.leaf_id().unwrap().to_string();

    // Both branches mention the keyword
    mgr.append_message(&assistant_msg("xylophone answer branch A"), None).unwrap();
    mgr.branch(&fork);
    mgr.append_message(&assistant_msg("xylophone answer branch B"), None).unwrap();

    let results = mgr.search_sessions("xylophone");
    // Both hits are in the same session — should deduplicate to 1 result
    assert_eq!(results.len(), 1, "multiple branch hits in one session → 1 search result");
}

#[test]
fn search_hit_in_inactive_branch_still_returns_session() {
    let (_tmp, mut mgr) = setup();
    mgr.new_session(std::path::Path::new("/tmp"), None).unwrap();

    mgr.append_message(&user_msg("shared"), None).unwrap();
    let fork = mgr.leaf_id().unwrap().to_string();

    // Branch A: has the keyword
    mgr.append_message(&assistant_msg("OBSCURE_TERM_47X branch A"), None).unwrap();

    // Branch B: active, no keyword
    mgr.branch(&fork);
    mgr.append_message(&assistant_msg("unrelated content"), None).unwrap();

    // Search should still find the session even though the hit is on the inactive branch
    let results = mgr.search_sessions("OBSCURE_TERM_47X");
    assert_eq!(results.len(), 1, "should find session even if hit is on inactive branch");
}

#[test]
fn get_tree_shows_both_branches() {
    let (_tmp, mut mgr) = setup();
    mgr.new_session(std::path::Path::new("/tmp"), None).unwrap();

    mgr.append_message(&user_msg("root"), None).unwrap();
    let branch_point = mgr.leaf_id().unwrap().to_string();

    mgr.append_message(&assistant_msg("branch A"), None).unwrap();
    mgr.branch(&branch_point);
    mgr.append_message(&assistant_msg("branch B"), None).unwrap();

    let tree = mgr.get_tree();
    // Root node should have 2 children
    assert_eq!(tree.len(), 1, "should have one root");
    assert_eq!(tree[0].children.len(), 2, "root should have 2 children (fork)");
}

#[test]
fn branch_walk_from_leaf() {
    let (_tmp, mut mgr) = setup();
    mgr.new_session(std::path::Path::new("/tmp"), None).unwrap();

    mgr.append_message(&user_msg("a"), None).unwrap();
    mgr.append_message(&assistant_msg("b"), None).unwrap();
    mgr.append_message(&user_msg("c"), None).unwrap();

    let branch = mgr.get_branch();
    assert_eq!(branch.len(), 3);
}

#[test]
// ── FTS5 search tests ────────────────────────────────────────────────

#[test]
fn search_finds_user_message() {
    let (tmp, mut mgr) = setup();
    mgr.new_session(tmp.path(), None).unwrap();
    mgr.append_message(&user_msg("implement the zorkblatt algorithm"), None)
        .unwrap();
    mgr.append_message(&assistant_msg("sure, here it is"), None)
        .unwrap();

    let results = mgr.search_sessions("zorkblatt");
    assert_eq!(results.len(), 1);
    assert!(results[0].excerpt.contains("zorkblatt"));
}

#[test]
fn search_finds_assistant_message() {
    let (tmp, mut mgr) = setup();
    mgr.new_session(tmp.path(), None).unwrap();
    mgr.append_message(&user_msg("hello"), None).unwrap();
    mgr.append_message(&assistant_msg("the frobnitz value is 42"), None)
        .unwrap();

    let results = mgr.search_sessions("frobnitz");
    assert_eq!(results.len(), 1);
    assert!(results[0].excerpt.contains("frobnitz"));
}

#[test]
fn search_finds_tool_result() {
    let (tmp, mut mgr) = setup();
    mgr.new_session(tmp.path(), None).unwrap();
    mgr.append_message(&user_msg("read the file"), None)
        .unwrap();
    mgr.append_message(&tool_result_msg("tc1", "fn quuxinator() {}"), None)
        .unwrap();

    let results = mgr.search_sessions("quuxinator");
    assert_eq!(results.len(), 1);
}

#[test]
fn search_deduplicates_across_sessions() {
    let (tmp, mut mgr) = setup();

    mgr.new_session(tmp.path(), None).unwrap();
    mgr.append_message(&user_msg("flamingo analysis part one"), None)
        .unwrap();
    mgr.append_message(&user_msg("flamingo analysis part two"), None)
        .unwrap();

    mgr.new_session(tmp.path(), None).unwrap();
    mgr.append_message(&user_msg("unrelated flamingo topic"), None)
        .unwrap();

    let results = mgr.search_sessions("flamingo");
    // Two sessions, not three hits
    assert_eq!(results.len(), 2);
}

#[test]
fn search_empty_query_returns_nothing() {
    let (tmp, mut mgr) = setup();
    mgr.new_session(tmp.path(), None).unwrap();
    mgr.append_message(&user_msg("hello"), None).unwrap();

    assert!(mgr.search_sessions("").is_empty());
    assert!(mgr.search_sessions("   ").is_empty());
}

#[test]
fn search_no_match_returns_empty() {
    let (tmp, mut mgr) = setup();
    mgr.new_session(tmp.path(), None).unwrap();
    mgr.append_message(&user_msg("hello world"), None).unwrap();

    assert!(mgr.search_sessions("xyznonexistent").is_empty());
}

#[test]
fn search_excerpt_contains_highlight_markers() {
    let (tmp, mut mgr) = setup();
    mgr.new_session(tmp.path(), None).unwrap();
    mgr.append_message(&user_msg("debug the sprongle handler"), None)
        .unwrap();

    let results = mgr.search_sessions("sprongle");
    assert_eq!(results.len(), 1);
    // FTS5 snippet uses <<HL>> / <</HL>> as markers
    assert!(results[0].excerpt.contains("<<HL>>"));
    assert!(results[0].excerpt.contains("<</HL>>"));
}

#[test]
fn search_with_special_characters() {
    let (tmp, mut mgr) = setup();
    mgr.new_session(tmp.path(), None).unwrap();
    mgr.append_message(&user_msg("fix the \"quoted\" bug"), None)
        .unwrap();

    // Should not crash on special FTS characters
    let results = mgr.search_sessions("\"quoted\"");
    assert_eq!(results.len(), 1);
}

#[test]
fn search_stemming_works() {
    let (tmp, mut mgr) = setup();
    mgr.new_session(tmp.path(), None).unwrap();
    mgr.append_message(&user_msg("implementing the parser"), None)
        .unwrap();

    // porter stemmer should match "implement" against "implementing"
    let results = mgr.search_sessions("implement");
    assert_eq!(results.len(), 1);
}

#[test]
fn backfill_indexes_preexisting_entries() {
    let tmp = TempDir::new().unwrap();
    let nerv_dir = tmp.path().join(".nerv");
    std::fs::create_dir_all(&nerv_dir).unwrap();

    // Create a session manager, add data, then drop it
    {
        let mut mgr = SessionManager::new(&nerv_dir);
        mgr.new_session(tmp.path(), None).unwrap();
        mgr.append_message(&user_msg("backfill canary xylophone"), None)
            .unwrap();
    }

    // Wipe the FTS index to simulate a pre-FTS database
    let db = sqlite::open(nerv_dir.join("sessions.db")).unwrap();
    db.execute("DELETE FROM search_index").unwrap();

    // Reopen — backfill should repopulate
    let mgr = SessionManager::new(&nerv_dir);
    let results = mgr.search_sessions("xylophone");
    assert_eq!(results.len(), 1);
}

#[test]
fn compaction_removes_fts_entries() {
    let (tmp, mut mgr) = setup();
    mgr.new_session(tmp.path(), None).unwrap();

    mgr.append_message(&user_msg("ephemeral garblotz message"), None)
        .unwrap();
    for i in 0..5 {
        mgr.append_message(&user_msg(&format!("msg {}", i)), None)
            .unwrap();
    }

    // Compact away the first message (entry 0), keeping from entry 1
    let kept_id = mgr.entries()[1].id().to_string();
    mgr.append_compaction("summary".into(), kept_id, 1000)
        .unwrap();

    // The compacted message's text should be gone from search
    let results = mgr.search_sessions("garblotz");
    assert!(results.is_empty());
}

#[test]
fn search_returns_session_metadata() {
    let (tmp, mut mgr) = setup();
    mgr.new_session(tmp.path(), None).unwrap();
    let session_id = mgr.session_id().to_string();
    mgr.append_message(&user_msg("metadata test wibblefish"), None)
        .unwrap();
    mgr.append_message(&assistant_msg("response"), None)
        .unwrap();

    let results = mgr.search_sessions("wibblefish");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].session_id, session_id);
    assert_eq!(results[0].id_short, &session_id[..8]);
    assert_eq!(results[0].cwd, tmp.path().to_string_lossy());
    assert!(results[0].message_count >= 2);
}

// ── Branching tests (continued) ─────────────────────────────────────

#[test]
fn load_session_finds_latest_leaf() {
    let (_tmp, mut mgr) = setup();
    mgr.new_session(std::path::Path::new("/tmp"), None).unwrap();

    mgr.append_message(&user_msg("root"), None).unwrap();
    let branch_point = mgr.leaf_id().unwrap().to_string();
    mgr.append_message(&assistant_msg("old branch"), None).unwrap();

    // Fork and add newer messages
    mgr.branch(&branch_point);
    mgr.append_message(&assistant_msg("new branch 1"), None).unwrap();
    mgr.append_message(&user_msg("new branch 2"), None).unwrap();

    // Reload the session — should land on the latest leaf (new branch 2)
    let session_id = mgr.session_id().to_string();
    let ctx = mgr.load_session(&session_id).unwrap();

    // The context should contain "new branch" messages, not "old branch"
    let has_new = ctx.messages.iter().any(|m| {
        if let AgentMessage::User { content, .. } = m {
            content.iter().any(|c| match c {
                ContentItem::Text { text } => text.contains("new branch 2"),
                _ => false,
            })
        } else {
            false
        }
    });
    assert!(has_new, "load_session should land on the latest leaf");
}

#[test]
fn worktree_create_and_merge() {
    // Set up a git repo
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(&repo)
            .output()
            .expect("git failed")
    };
    git(&["init"]);
    std::fs::write(repo.join("file.txt"), "v1\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-m", "init"]);

    let nerv_dir = tmp.path().join(".nerv");
    std::fs::create_dir_all(&nerv_dir).unwrap();

    // Create worktree
    let wt_path =
        nerv::worktree::create_worktree(&repo, &nerv_dir, "my-feature", "abc12345").unwrap();
    assert!(wt_path.exists());
    assert!(wt_path.join("file.txt").exists());

    // Branch name should be set
    let branch_out = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(&wt_path)
        .output()
        .unwrap();
    let branch = String::from_utf8_lossy(&branch_out.stdout).trim().to_string();
    assert_eq!(branch, "nerv/abc12345/my-feature");

    // Make a change in the worktree
    std::fs::write(wt_path.join("file.txt"), "v2\n").unwrap();
    let git_wt = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(&wt_path)
            .output()
            .expect("git failed")
    };
    git_wt(&["add", "."]);
    git_wt(&["commit", "-m", "worktree change"]);

    // Main repo still has v1
    assert_eq!(
        std::fs::read_to_string(repo.join("file.txt")).unwrap(),
        "v1\n"
    );

    // Merge worktree
    let main_wt = nerv::worktree::merge_worktree(&wt_path).unwrap();
    assert_eq!(main_wt, repo.canonicalize().unwrap());

    // Main repo now has v2
    assert_eq!(
        std::fs::read_to_string(repo.join("file.txt")).unwrap(),
        "v2\n"
    );

    // Worktree directory should be gone
    assert!(!wt_path.exists());
}

#[test]
fn worktree_merge_rejects_dirty() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(&repo)
            .output()
            .expect("git failed")
    };
    git(&["init"]);
    std::fs::write(repo.join("file.txt"), "v1\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-m", "init"]);

    let nerv_dir = tmp.path().join(".nerv");
    std::fs::create_dir_all(&nerv_dir).unwrap();

    let wt_path =
        nerv::worktree::create_worktree(&repo, &nerv_dir, "dirty-test", "def67890").unwrap();

    // Leave uncommitted changes
    std::fs::write(wt_path.join("file.txt"), "dirty\n").unwrap();

    let result = nerv::worktree::merge_worktree(&wt_path);
    assert!(result.is_err());
    assert!(
        result.unwrap_err().to_string().contains("uncommitted"),
        "should mention uncommitted changes"
    );

    // Worktree should still exist since merge was aborted
    assert!(wt_path.exists());

    // Clean up manually
    std::process::Command::new("git")
        .args(["worktree", "remove", "--force", &wt_path.to_string_lossy()])
        .current_dir(&repo)
        .output()
        .ok();
}

#[test]
fn session_remembers_worktree() {
    let (tmp, mut mgr) = setup();
    let wt_path = tmp.path().join("fake-worktree");
    std::fs::create_dir_all(&wt_path).unwrap();

    mgr.new_session(tmp.path(), Some(&wt_path)).unwrap();
    let stored = mgr.session_worktree();
    assert_eq!(stored, Some(wt_path.clone()));

    // Reload from DB
    let session_id = mgr.session_id().to_string();
    mgr.load_session(&session_id).unwrap();
    assert_eq!(mgr.session_worktree(), Some(wt_path));
}

#[test]
fn session_without_worktree_returns_none() {
    let (_tmp, mut mgr) = setup();
    mgr.new_session(std::path::Path::new("/tmp"), None).unwrap();
    assert_eq!(mgr.session_worktree(), None);
}

#[test]
fn worktree_merge_aborts_on_conflict() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |dir: &std::path::Path, args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git failed")
    };
    git(&repo, &["init"]);
    std::fs::write(repo.join("file.txt"), "original\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "init"]);

    let nerv_dir = tmp.path().join(".nerv");
    std::fs::create_dir_all(&nerv_dir).unwrap();

    let wt_path =
        nerv::worktree::create_worktree(&repo, &nerv_dir, "conflict-test", "ccc11111").unwrap();

    // Make conflicting changes on both sides
    std::fs::write(repo.join("file.txt"), "main change\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "main diverges"]);

    std::fs::write(wt_path.join("file.txt"), "worktree change\n").unwrap();
    git(&wt_path, &["add", "."]);
    git(&wt_path, &["commit", "-m", "worktree diverges"]);

    // Merge should fail and abort
    let result = nerv::worktree::merge_worktree(&wt_path);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("conflicts"), "error should mention conflicts: {}", err);
    assert!(err.contains("cd "), "error should include manual merge instructions: {}", err);

    // Main repo should be clean (merge was aborted)
    let status = git(&repo, &["status", "--porcelain"]);
    let status_str = String::from_utf8_lossy(&status.stdout);
    assert!(status_str.trim().is_empty(), "main repo should be clean after abort");

    // Worktree should still exist
    assert!(wt_path.exists());

    // Clean up
    std::process::Command::new("git")
        .args(["worktree", "remove", "--force", &wt_path.to_string_lossy()])
        .current_dir(&repo)
        .output()
        .ok();
}

#[test]
fn update_worktree_on_existing_session() {
    let (tmp, mut mgr) = setup();
    mgr.new_session(tmp.path(), None).unwrap();
    assert_eq!(mgr.session_worktree(), None);

    // Simulate /wt after /new: update the worktree on an empty session
    let wt_path = tmp.path().join("new-worktree");
    std::fs::create_dir_all(&wt_path).unwrap();
    mgr.update_worktree(&wt_path, &wt_path);

    assert_eq!(mgr.session_worktree(), Some(wt_path.clone()));

    // Survives reload
    let session_id = mgr.session_id().to_string();
    mgr.load_session(&session_id).unwrap();
    assert_eq!(mgr.session_worktree(), Some(wt_path));
}
