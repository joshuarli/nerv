use nerv::agent::types::*;
use nerv::compaction::*;
use nerv::session::types::*;

fn user_entry(text: &str, id: &str, parent: Option<&str>) -> SessionEntry {
    SessionEntry::Message(MessageEntry {
        id: id.to_string(),
        parent_id: parent.map(|s| s.to_string()),
        timestamp: now_iso(),
        tokens: None,
        message: AgentMessage::User {
            content: vec![ContentItem::Text { text: text.into() }],
            timestamp: now_millis(),
        },
    })
}

fn assistant_entry(text: &str, id: &str, parent: &str) -> SessionEntry {
    SessionEntry::Message(MessageEntry {
        id: id.to_string(),
        parent_id: Some(parent.to_string()),
        timestamp: now_iso(),
        tokens: None,
        message: AgentMessage::Assistant(AssistantMessage {
            content: vec![ContentBlock::Text { text: text.into() }],
            stop_reason: StopReason::EndTurn,
            usage: Some(Usage {
                input: 100,
                output: 50,
                ..Default::default()
            }),
            timestamp: now_millis(),
        }),
    })
}

fn tool_result_entry(text: &str, id: &str, parent: &str) -> SessionEntry {
    SessionEntry::Message(MessageEntry {
        id: id.to_string(),
        parent_id: Some(parent.to_string()),
        timestamp: now_iso(),
        tokens: None,
        message: AgentMessage::ToolResult {
            tool_call_id: "tc1".into(),
            content: vec![ContentItem::Text { text: text.into() }],
            is_error: false,
            display: None,
            timestamp: now_millis(),
        },
    })
}

#[test]
fn count_tokens_returns_reasonable_values() {
    // "Hello, world!" is typically 4 tokens in cl100k_base
    let n = count_tokens("Hello, world!");
    assert!((3..=6).contains(&n), "got {}", n);

    assert_eq!(count_tokens(""), 0);

    // Longer text should have proportionally more tokens
    let short = count_tokens("hello");
    let long = count_tokens(&"hello ".repeat(100));
    assert!(long > short * 10);
}

#[test]
fn estimate_tokens_includes_overhead() {
    let msg = AgentMessage::User {
        content: vec![ContentItem::Text { text: "Hi".into() }],
        timestamp: 0,
    };
    let n = estimate_tokens(&msg);
    // "Hi" = 1 token + 4 overhead = 5
    assert!(n >= 4, "expected at least 4 (with overhead), got {}", n);
}

#[test]
fn should_compact_threshold() {
    // Default threshold_pct = 0.50 → threshold at 50k of 100k window
    let s = CompactionSettings { enabled: true, threshold_pct: 0.90, keep_recent_tokens: 20_000 };
    assert!(!should_compact(50_000, 100_000, &s)); // well under
    assert!(!should_compact(89_999, 100_000, &s)); // just under
    assert!(should_compact(90_001, 100_000, &s));  // just over

    let disabled = CompactionSettings { enabled: false, ..s };
    assert!(!should_compact(99_999, 100_000, &disabled));
}

#[test]
fn find_cut_point_never_cuts_at_tool_result() {
    let entries = vec![
        user_entry("hello", "e1", None),
        assistant_entry("I'll read the file", "e2", "e1"),
        tool_result_entry(&"x".repeat(500), "e3", "e2"), // large tool result
        user_entry("thanks", "e4", Some("e3")),
        assistant_entry("you're welcome", "e5", "e4"),
    ];

    // Very small budget — forces cut early
    let cut = find_cut_point(&entries, 0, entries.len(), 10);
    let cut_entry = &entries[cut.first_kept_entry_index];
    if let SessionEntry::Message(me) = cut_entry {
        assert!(
            !matches!(me.message, AgentMessage::ToolResult { .. }),
            "cut at index {} is a tool result — should never happen",
            cut.first_kept_entry_index
        );
    }
}

#[test]
fn find_cut_point_keeps_recent_by_token_budget() {
    // 20 entries, each ~50 tokens
    let mut entries = Vec::new();
    let mut prev: Option<String> = None;
    for i in 0..20 {
        let id = format!("e{}", i);
        entries.push(user_entry(&"word ".repeat(40), &id, prev.as_deref()));
        prev = Some(id);
    }

    // Budget of 200 tokens should keep ~4 entries from the end
    let cut = find_cut_point(&entries, 0, entries.len(), 200);
    assert!(
        cut.first_kept_entry_index >= 14,
        "expected cut at 14+, got {}",
        cut.first_kept_entry_index
    );
    assert!(
        cut.first_kept_entry_index <= 18,
        "cut too late: {}",
        cut.first_kept_entry_index
    );
}

#[test]
fn find_cut_point_with_empty_entries() {
    let entries: Vec<SessionEntry> = vec![];
    let cut = find_cut_point(&entries, 0, 0, 1000);
    assert_eq!(cut.first_kept_entry_index, 0);
    assert!(!cut.is_split_turn);
}

#[test]
fn find_cut_point_split_turn_detected() {
    // A turn is: user → assistant(tool_call) → tool_result → assistant(final)
    // If we cut at the second assistant, it's a split turn.
    let entries = vec![
        user_entry(&"x".repeat(200), "e1", None), // large, will be cut
        assistant_entry(&"x".repeat(200), "e2", "e1"), // large
        tool_result_entry(&"x".repeat(200), "e3", "e2"), // large
        user_entry("recent", "e4", Some("e3")),   // small, recent
        assistant_entry("done", "e5", "e4"),      // small, recent
    ];

    let cut = find_cut_point(&entries, 0, entries.len(), 50);
    // With small budget, should keep from e4 or e5
    // e4 is a user message, so is_split_turn should be false
    if cut.first_kept_entry_index >= 3 {
        let entry = &entries[cut.first_kept_entry_index];
        if let SessionEntry::Message(me) = entry
            && matches!(me.message, AgentMessage::User { .. })
        {
            assert!(!cut.is_split_turn, "user message should not be split turn");
        }
    }
}
