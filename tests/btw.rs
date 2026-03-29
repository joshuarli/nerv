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
fn strip_tool_content_removes_tool_call_blocks_from_assistant() {
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
        assert_eq!(a.content.len(), 1, "only text block should remain");
        assert!(matches!(a.content[0], ContentBlock::Text { .. }));
    } else {
        panic!("expected assistant message");
    }
}

#[test]
fn strip_tool_content_drops_assistant_with_only_tool_calls() {
    let messages = vec![
        AgentMessage::Assistant(AssistantMessage {
            content: vec![tool_call_block()],
            stop_reason: StopReason::ToolUse,
            usage: Some(Usage::default()),
            timestamp: 0,
        }),
    ];
    let stripped = strip_tool_content(messages);
    assert!(stripped.is_empty(), "assistant with only tool calls should be dropped");
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
