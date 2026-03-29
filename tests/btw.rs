//! Tests for the /btw overlay — pure-function logic only (no I/O, no terminal).

mod helpers;

use nerv::agent::types::{
    AgentMessage, AssistantMessage, ContentBlock, ContentItem, StopReason, Usage,
};
use nerv::interactive::btw_overlay::{pad_right, turn_succeeded, wrap_text};

// ── helpers ──────────────────────────────────────────────────────────────────

fn user_msg(text: &str) -> AgentMessage {
    AgentMessage::User {
        content: vec![ContentItem::Text { text: text.into() }],
        timestamp: 0,
    }
}

fn assistant_msg(text: &str, stop: StopReason) -> AgentMessage {
    AgentMessage::Assistant(AssistantMessage {
        content: vec![ContentBlock::Text { text: text.into() }],
        stop_reason: stop,
        usage: Some(Usage::default()),
        timestamp: 0,
    })
}

// ── wrap_text ─────────────────────────────────────────────────────────────────

#[test]
fn wrap_empty_string() {
    // An empty string produces one empty paragraph line — consistent with
    // how the renderer handles it (empty lines are just blank rows).
    let result = wrap_text("", 80);
    assert_eq!(result, vec!["".to_string()]);
}

#[test]
fn wrap_short_line_unchanged() {
    let result = wrap_text("hello world", 80);
    assert_eq!(result, vec!["hello world"]);
}

#[test]
fn wrap_exactly_at_boundary() {
    // 5 chars + space + 5 chars = 11 total, fits in width 11
    let result = wrap_text("hello world", 11);
    assert_eq!(result, vec!["hello world"]);
}

#[test]
fn wrap_splits_at_word_boundary() {
    // "hello world" — width 8 → "hello" (5) + " world" would be 11, exceeds 8
    let result = wrap_text("hello world", 8);
    assert_eq!(result, vec!["hello", "world"]);
}

#[test]
fn wrap_multiple_words_on_one_line() {
    // "a b c d" with width 6 → "a b c" (5) + "d"
    let result = wrap_text("a b c d", 6);
    assert_eq!(result, vec!["a b c", "d"]);
}

#[test]
fn wrap_hard_wraps_long_word() {
    // A word longer than max_chars must be split mid-word
    let result = wrap_text("abcdefghij", 5);
    assert_eq!(result, vec!["abcde", "fghij"]);
}

#[test]
fn wrap_hard_wraps_long_word_then_continues() {
    // Long word followed by a short word
    let result = wrap_text("abcdefghij short", 5);
    // "abcde" "fghij" "short"
    assert_eq!(result, vec!["abcde", "fghij", "short"]);
}

#[test]
fn wrap_hard_wrap_uneven() {
    // 7-char word at width 3 → "abc" "def" "g"
    let result = wrap_text("abcdefg", 3);
    assert_eq!(result, vec!["abc", "def", "g"]);
}

#[test]
fn wrap_respects_newlines() {
    let result = wrap_text("hello\nworld", 80);
    assert_eq!(result, vec!["hello", "world"]);
}

#[test]
fn wrap_empty_line_preserved() {
    let result = wrap_text("a\n\nb", 80);
    assert_eq!(result, vec!["a", "", "b"]);
}

#[test]
fn wrap_multiple_paragraphs() {
    let result = wrap_text("foo bar\nbaz qux", 5);
    // "foo" + " bar" = 7 > 5, so splits → "foo", "bar"; "baz", "qux"
    assert_eq!(result, vec!["foo", "bar", "baz", "qux"]);
}

#[test]
fn wrap_unicode_chars_counted_by_codepoint() {
    // "éàü" = 3 chars, width 3 → fits on one line
    let result = wrap_text("éàü", 3);
    assert_eq!(result, vec!["éàü"]);
}

#[test]
fn wrap_unicode_long_word_hard_wraps() {
    // "éàüöä" = 5 chars, width 3 → "éàü" + "öä"
    let result = wrap_text("éàüöä", 3);
    assert_eq!(result, vec!["éàü", "öä"]);
}

#[test]
fn wrap_zero_width_returns_whole_text() {
    // Width 0 is a degenerate case — return text as-is rather than panic
    let result = wrap_text("hello world", 0);
    assert_eq!(result, vec!["hello world"]);
}

#[test]
fn wrap_preserves_leading_text_on_overflow() {
    // "aa bb" at width 4: "aa" fits (2), " bb" would be 5 > 4, wrap → "aa", "bb"
    let result = wrap_text("aa bb", 4);
    assert_eq!(result, vec!["aa", "bb"]);
}

#[test]
fn wrap_packs_words_greedily() {
    // "a b c" at width 5: "a b c" = 5 chars → all on one line
    let result = wrap_text("a b c", 5);
    assert_eq!(result, vec!["a b c"]);
}

// ── pad_right ─────────────────────────────────────────────────────────────────

#[test]
fn pad_short_string_padded_with_spaces() {
    let result = pad_right("hi", 6);
    assert_eq!(result, "hi    ");
    assert_eq!(result.chars().count(), 6);
}

#[test]
fn pad_exact_width_unchanged() {
    let result = pad_right("hello", 5);
    assert_eq!(result, "hello");
}

#[test]
fn pad_longer_than_width_truncated() {
    let result = pad_right("hello world", 5);
    assert_eq!(result, "hello");
    assert_eq!(result.chars().count(), 5);
}

#[test]
fn pad_empty_string() {
    let result = pad_right("", 4);
    assert_eq!(result, "    ");
    assert_eq!(result.chars().count(), 4);
}

#[test]
fn pad_unicode_counts_codepoints() {
    // "éàü" = 3 chars, pad to 5 → "éàü  "
    let result = pad_right("éàü", 5);
    assert_eq!(result, "éàü  ");
    assert_eq!(result.chars().count(), 5);
}

#[test]
fn pad_unicode_truncates_at_char_boundary() {
    // "éàüöä" = 5 chars, truncate to 3 → "éàü"
    let result = pad_right("éàüöä", 3);
    assert_eq!(result, "éàü");
    assert_eq!(result.chars().count(), 3);
}

#[test]
fn pad_zero_width_returns_empty() {
    let result = pad_right("hello", 0);
    // Truncating to 0 chars → empty string (or first 0 chars)
    assert_eq!(result.chars().count(), 0);
}

// ── turn_succeeded ────────────────────────────────────────────────────────────

#[test]
fn turn_succeeded_empty_messages() {
    assert!(turn_succeeded(&[]));
}

#[test]
fn turn_succeeded_normal_turn() {
    let msgs = vec![
        user_msg("hello"),
        assistant_msg("hi there", StopReason::EndTurn),
    ];
    assert!(turn_succeeded(&msgs));
}

#[test]
fn turn_succeeded_tool_use_turn() {
    // ToolUse stop reason is not an error — the turn loops to execute tools.
    // The turn is only "succeeded" from snapshot perspective after the loop
    // completes. The agent collects all messages including ToolUse responses
    // into AgentEnd.messages, so we treat ToolUse as ok (loop succeeded).
    let msgs = vec![
        user_msg("use a tool"),
        assistant_msg("calling tool", StopReason::ToolUse),
    ];
    assert!(turn_succeeded(&msgs));
}

#[test]
fn turn_failed_aborted() {
    let msgs = vec![
        user_msg("question"),
        assistant_msg("partial answer", StopReason::Aborted),
    ];
    assert!(!turn_succeeded(&msgs));
}

#[test]
fn turn_failed_error() {
    let msgs = vec![
        user_msg("question"),
        assistant_msg(
            "",
            StopReason::Error {
                message: "rate limit".into(),
            },
        ),
    ];
    assert!(!turn_succeeded(&msgs));
}

#[test]
fn turn_succeeded_only_user_message() {
    // If only a user message was pushed (edge case), no abort occurred.
    let msgs = vec![user_msg("hello")];
    assert!(turn_succeeded(&msgs));
}

#[test]
fn turn_failed_error_among_multi_turn() {
    // Multi-tool turn where the last assistant errors out.
    let msgs = vec![
        user_msg("do stuff"),
        assistant_msg("calling tool", StopReason::ToolUse),
        // tool result would be here but we omit for simplicity — turn_succeeded
        // only inspects Assistant messages.
        assistant_msg(
            "",
            StopReason::Error {
                message: "context exceeded".into(),
            },
        ),
    ];
    assert!(!turn_succeeded(&msgs));
}

// ── snapshot accumulation integration ─────────────────────────────────────────
// Test the guard logic as it would be used in the event loop.

#[test]
fn snapshot_skips_aborted_turns() {
    let mut snapshot: Vec<AgentMessage> = Vec::new();

    // First turn: successful
    let turn1 = vec![
        user_msg("first question"),
        assistant_msg("first answer", StopReason::EndTurn),
    ];
    if turn_succeeded(&turn1) {
        snapshot.extend(turn1);
    }

    // Second turn: aborted mid-stream
    let turn2 = vec![
        user_msg("second question"),
        assistant_msg("partial...", StopReason::Aborted),
    ];
    if turn_succeeded(&turn2) {
        snapshot.extend(turn2);
    }

    // Third turn: retry of second question, successful
    let turn3 = vec![
        user_msg("second question"),
        assistant_msg("complete answer", StopReason::EndTurn),
    ];
    if turn_succeeded(&turn3) {
        snapshot.extend(turn3);
    }

    // Snapshot should have turn1 + turn3 = 4 messages, not 6.
    assert_eq!(
        snapshot.len(),
        4,
        "aborted turn should not appear in snapshot"
    );

    // The user message "second question" should appear only once.
    let user_texts: Vec<_> = snapshot
        .iter()
        .filter_map(|m| match m {
            AgentMessage::User { content, .. } => {
                let t = content
                    .iter()
                    .filter_map(|c| match c {
                        ContentItem::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>();
                Some(t)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        user_texts.iter().filter(|t| t.as_str() == "second question").count(),
        1,
        "second question should appear exactly once in snapshot"
    );
}

#[test]
fn snapshot_skips_error_turns() {
    let mut snapshot: Vec<AgentMessage> = Vec::new();

    let errored = vec![
        user_msg("q"),
        assistant_msg("", StopReason::Error { message: "oops".into() }),
    ];
    if turn_succeeded(&errored) {
        snapshot.extend(errored);
    }

    assert!(
        snapshot.is_empty(),
        "errored turn should not be in snapshot"
    );
}

// ── cache-hit assumptions ─────────────────────────────────────────────────────
//
// stream_btw claims "Anthropic's cache breakpoints match and the prefix is a
// cache hit".  These tests pin the three specific ways that claim can fail.

use nerv::agent::transform::transform_context;

/// 1. transform_context mutates the messages before every real API request.
///    The btw call sends raw agent.state.messages without running
///    transform_context, so the wire content diverges whenever any
///    transformation would fire.
#[test]
fn transform_context_mutates_stale_read_results() {
    // Build a history where two read calls happen on the same file.
    // transform_context should mark the first one superseded.
    let read_call_1 = ContentBlock::ToolCall {
        id: "r1".into(),
        name: "read".into(),
        arguments: serde_json::json!({"path": "foo.rs"}),
    };
    let read_call_2 = ContentBlock::ToolCall {
        id: "r2".into(),
        name: "read".into(),
        arguments: serde_json::json!({"path": "foo.rs"}),
    };
    let messages = vec![
        user_msg("start"),
        AgentMessage::Assistant(AssistantMessage {
            content: vec![read_call_1],
            stop_reason: StopReason::ToolUse,
            usage: Some(Usage::default()),
            timestamp: 0,
        }),
        AgentMessage::ToolResult {
            tool_call_id: "r1".into(),
            content: vec![ContentItem::Text { text: "old content".into() }],
            is_error: false,
            display: None,
            timestamp: 0,
        },
        AgentMessage::Assistant(AssistantMessage {
            content: vec![read_call_2],
            stop_reason: StopReason::ToolUse,
            usage: Some(Usage::default()),
            timestamp: 0,
        }),
        AgentMessage::ToolResult {
            tool_call_id: "r2".into(),
            content: vec![ContentItem::Text { text: "new content".into() }],
            is_error: false,
            display: None,
            timestamp: 0,
        },
        user_msg("done"),
    ];

    let transformed = transform_context(messages.clone(), 200_000, None);

    // Find the first ToolResult (r1) in the transformed output.
    let r1_transformed = transformed.iter().find_map(|m| {
        if let AgentMessage::ToolResult { tool_call_id, content, .. } = m {
            if tool_call_id == "r1" { Some(content.clone()) } else { None }
        } else { None }
    });
    let r1_raw = messages.iter().find_map(|m| {
        if let AgentMessage::ToolResult { tool_call_id, content, .. } = m {
            if tool_call_id == "r1" { Some(content.clone()) } else { None }
        } else { None }
    });

    let r1_t = r1_transformed.expect("r1 should survive transform");
    let r1_r = r1_raw.expect("r1 in raw messages");

    // The transformed result should be "[superseded by later call]", not the
    // original text — proving the wire bytes differ from the raw snapshot.
    let raw_text = match &r1_r[0] {
        ContentItem::Text { text } => text.clone(),
        other => panic!("expected text in raw, got {:?}", other),
    };
    let superseded_text = match &r1_t[0] {
        ContentItem::Text { text } => text.clone(),
        other => panic!("expected text in transformed, got {:?}", other),
    };
    assert_ne!(
        raw_text, superseded_text,
        "transform_context should have mutated the first read result to '[superseded]'; \
         if it matches the raw content the btw call would send the correct bytes by accident, \
         but for the wrong reason — the transform still runs on real requests and not on btw"
    );
    assert!(
        superseded_text.contains("superseded"),
        "expected '[superseded by later call]', got: {superseded_text:?}"
    );
}

/// 2. Appending to the system prompt changes its bytes.
///    Anthropic's cache breakpoint 1 is placed on the last content block of
///    system[], which is the full system prompt string.  A different string
///    always misses that breakpoint — there is no prefix-match caching.
#[test]
fn btw_system_prompt_differs_from_main_agent_system_prompt() {
    let base = "You are a helpful assistant.";
    let btw_suffix = "\n\n<btw>The user is asking a side question while the agent works. \
        Answer concisely in 1-4 sentences without calling any tools.</btw>";

    let btw_prompt = format!("{base}{btw_suffix}");

    // The two strings must be different — that's the only claim we're making.
    assert_ne!(
        base, btw_prompt.as_str(),
        "btw system prompt must differ from the base; \
         if they match the breakpoint-1 cache hit would be real"
    );

    // More concretely: the btw prompt is strictly longer.
    assert!(
        btw_prompt.len() > base.len(),
        "btw system prompt should be longer than base"
    );

    // And the suffix must be non-empty (guard against accidental empty suffix).
    assert!(
        !btw_suffix.trim().is_empty(),
        "btw suffix must not be empty"
    );
}

/// 3. The snapshot captured at AgentEnd contains the *full* accumulated history
///    including all intra-turn tool rounds.  When the btw call sends those same
///    messages, it appends a new user message at the end — but the main agent's
///    *next* request will also append its new user message to the same base.
///    The positions of cache breakpoints (first-user, last-user) therefore
///    shift relative to what the agent last sent, breaking the last-user
///    breakpoint alignment.
///
///    This test shows that the last-user-message index in the btw request is
///    different from the last-user-message index in the immediately preceding
///    agent request — the cache breakpoint lands on a different message.
#[test]
fn btw_last_user_breakpoint_lands_on_different_message_than_agent() {
    // Simulate agent state after one complete tool-using turn.
    // agent.state.messages at AgentEnd time (= what the snapshot captures):
    let snapshot = vec![
        user_msg("initial question"),       // idx 0 — first user (bp4)
        AgentMessage::Assistant(AssistantMessage {
            content: vec![
                ContentBlock::ToolCall {
                    id: "c1".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({"command": "ls"}),
                },
            ],
            stop_reason: StopReason::ToolUse,
            usage: Some(Usage::default()),
            timestamp: 0,
        }),
        AgentMessage::ToolResult {
            tool_call_id: "c1".into(),
            content: vec![ContentItem::Text { text: "file.txt".into() }],
            is_error: false,
            display: None,
            timestamp: 0,
        },
        // The agent's final reply in this turn:
        AgentMessage::Assistant(AssistantMessage {
            content: vec![ContentBlock::Text { text: "done".into() }],
            stop_reason: StopReason::EndTurn,
            usage: Some(Usage::default()),
            timestamp: 0,
        }),
    ];
    // The agent's last request had this as the last (and only) user message,
    // so breakpoint 3 (last-user) was on idx 0.
    let agent_last_user_idx = snapshot
        .iter()
        .rposition(|m| matches!(m, AgentMessage::User { .. }))
        .expect("at least one user message");

    // The btw call appends a new user message (the /btw note).
    let mut btw_messages = snapshot.clone();
    btw_messages.push(user_msg("quick side question"));

    let btw_last_user_idx = btw_messages
        .iter()
        .rposition(|m| matches!(m, AgentMessage::User { .. }))
        .expect("at least one user message");

    // The breakpoints land on different positions → different wire bytes →
    // cache miss on last-user breakpoint for the new user message.
    assert_ne!(
        agent_last_user_idx,
        btw_last_user_idx,
        "btw appends a user message, so last-user breakpoint moves; \
         the agent's cached last-user position no longer matches"
    );

    // Specifically: btw's last-user is the new note, agent's is the original question.
    assert_eq!(agent_last_user_idx, 0, "agent: only user message is at idx 0");
    assert_eq!(btw_last_user_idx, btw_messages.len() - 1, "btw: note is last");
}

// ── strip_tool_content ────────────────────────────────────────────────────────

use nerv::interactive::btw_overlay::strip_tool_content;

fn tool_call_block() -> ContentBlock {
    ContentBlock::ToolCall {
        id: "call_1".into(),
        name: "bash".into(),
        arguments: serde_json::json!({"command": "ls"}),
    }
}

fn tool_result_msg(id: &str) -> AgentMessage {
    AgentMessage::ToolResult {
        tool_call_id: id.into(),
        content: vec![ContentItem::Text { text: "output".into() }],
        is_error: false,
        display: None,
        timestamp: 0,
    }
}

#[test]
fn strip_tool_content_removes_tool_results() {
    let messages = vec![
        user_msg("hello"),
        tool_result_msg("call_1"),
        user_msg("world"),
    ];
    let stripped = strip_tool_content(messages);
    assert_eq!(stripped.len(), 2, "tool result should be removed");
    assert!(matches!(stripped[0], AgentMessage::User { .. }));
    assert!(matches!(stripped[1], AgentMessage::User { .. }));
}

#[test]
fn strip_tool_content_replaces_tool_call_blocks_with_summary() {
    let messages = vec![
        AgentMessage::Assistant(AssistantMessage {
            content: vec![
                ContentBlock::Text { text: "Let me run that.".into() },
                tool_call_block(),
            ],
            stop_reason: StopReason::ToolUse,
            usage: Some(Usage::default()),
            timestamp: 0,
        }),
    ];
    let stripped = strip_tool_content(messages);
    assert_eq!(stripped.len(), 1);
    if let AgentMessage::Assistant(a) = &stripped[0] {
        // Expect: original text block + a summary text block for the tool call.
        assert_eq!(a.content.len(), 2, "text block + tool summary should remain");
        assert!(matches!(a.content[0], ContentBlock::Text { .. }));
        assert!(matches!(a.content[1], ContentBlock::Text { .. }));
        if let ContentBlock::Text { text } = &a.content[1] {
            assert!(text.contains("bash"), "summary should name the tool");
        }
    } else {
        panic!("expected assistant message");
    }
}

#[test]
fn strip_tool_content_keeps_assistant_with_only_tool_calls_as_summary() {
    let messages = vec![
        AgentMessage::Assistant(AssistantMessage {
            content: vec![tool_call_block()],
            stop_reason: StopReason::ToolUse,
            usage: Some(Usage::default()),
            timestamp: 0,
        }),
    ];
    let stripped = strip_tool_content(messages);
    // Now produces a summary text block instead of being dropped.
    assert_eq!(stripped.len(), 1, "assistant should be kept as a summary");
    if let AgentMessage::Assistant(a) = &stripped[0] {
        assert_eq!(a.content.len(), 1);
        assert!(matches!(a.content[0], ContentBlock::Text { .. }));
    }
}

#[test]
fn strip_tool_content_preserves_user_and_text_assistant() {
    let messages = vec![
        user_msg("question"),
        AgentMessage::Assistant(AssistantMessage {
            content: vec![ContentBlock::Text { text: "answer".into() }],
            stop_reason: StopReason::EndTurn,
            usage: Some(Usage::default()),
            timestamp: 0,
        }),
    ];
    let stripped = strip_tool_content(messages.clone());
    assert_eq!(stripped.len(), 2, "all messages should be preserved");
}
