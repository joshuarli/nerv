use nerv::agent::convert::*;
use nerv::agent::transform::*;
use nerv::agent::types::*;

#[test]
fn compaction_summary_converts_to_user() {
    let msgs = vec![AgentMessage::CompactionSummary {
        summary: "Previous work: edited files".into(),
        tokens_before: 5000,
        timestamp: 0,
    }];
    let llm = convert_to_llm(&msgs);
    assert_eq!(llm.len(), 1);
    assert!(llm[0].is_user());
}

#[test]
fn bash_execution_converts_to_user() {
    let msgs = vec![AgentMessage::BashExecution {
        command: "ls -la".into(),
        output: "total 0\n".into(),
        exit_code: Some(0),
        timestamp: 0,
    }];
    let llm = convert_to_llm(&msgs);
    assert_eq!(llm.len(), 1);
    assert!(llm[0].is_user());
}

#[test]
fn tool_result_stays_separate_from_assistant() {
    let msgs = vec![
        AgentMessage::Assistant(AssistantMessage {
            content: vec![ContentBlock::ToolCall {
                id: "tc1".into(),
                name: "read".into(),
                arguments: serde_json::json!({"path": "foo.txt"}),
            }],
            stop_reason: StopReason::ToolUse,
            usage: None,
            timestamp: 0,
        }),
        AgentMessage::ToolResult {
            tool_call_id: "tc1".into(),
            content: vec![ContentItem::Text { text: "file contents".into() }],
            is_error: false,
            display: None,
            details: None,
            timestamp: 1,
        },
    ];
    let llm = convert_to_llm(&msgs);
    assert_eq!(llm.len(), 2);
    assert!(llm[0].is_assistant());
    assert!(!llm[1].is_assistant());
}

#[test]
fn transform_context_truncates_old_large_tool_results() {
    let mut msgs = Vec::new();
    for i in 0..15 {
        msgs.push(AgentMessage::User {
            content: vec![ContentItem::Text { text: format!("msg {}", i) }],
            timestamp: i as u64,
        });
        msgs.push(AgentMessage::ToolResult {
            tool_call_id: format!("tc{}", i),
            content: vec![ContentItem::Text { text: "x".repeat(500) }],
            is_error: false,
            display: None,
            details: None,
            timestamp: i as u64 + 1,
        });
    }

    let transformed = transform_context(msgs, 100_000, None);
    assert_eq!(transformed.len(), 30);

    // Old tool result (index 1) should be truncated
    if let AgentMessage::ToolResult { content, .. } = &transformed[1] {
        let text: String = content
            .iter()
            .filter_map(|c| match c {
                ContentItem::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(text.len() < 500, "old tool result should be truncated, got len {}", text.len());
        assert!(text.contains("[truncated"), "should contain truncation marker");
    }

    // Recent tool result (index 21) should be intact
    if let AgentMessage::ToolResult { content, .. } = &transformed[21] {
        let text: String = content
            .iter()
            .filter_map(|c| match c {
                ContentItem::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text.len(), 500, "recent tool result should be intact");
    }
}

#[test]
fn transform_context_preserves_small_old_results() {
    let mut msgs: Vec<AgentMessage> = (0..15)
        .map(|i| AgentMessage::User {
            content: vec![ContentItem::Text { text: format!("m{}", i) }],
            timestamp: i,
        })
        .collect();
    msgs.insert(
        1,
        AgentMessage::ToolResult {
            tool_call_id: "tc".into(),
            content: vec![ContentItem::Text { text: "tiny".into() }],
            is_error: false,
            display: None,
            details: None,
            timestamp: 99,
        },
    );

    let transformed = transform_context(msgs, 100_000, None);
    if let AgentMessage::ToolResult { content, .. } = &transformed[1] {
        let text: String = content
            .iter()
            .filter_map(|c| match c {
                ContentItem::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "tiny", "small results should be preserved even when old");
    }
}

#[test]
fn convert_to_llm_preserves_thinking_blocks() {
    let msgs = vec![AgentMessage::Assistant(AssistantMessage {
        content: vec![
            ContentBlock::Thinking { thinking: "Let me consider...".into() },
            ContentBlock::Text { text: "The answer is 42.".into() },
        ],
        stop_reason: StopReason::EndTurn,
        usage: None,
        timestamp: 0,
    })];
    let llm = convert_to_llm(&msgs);
    assert_eq!(llm.len(), 1);
    if let LlmMessage::Assistant { content } = &llm[0] {
        assert!(
            content
                .iter()
                .any(|c| matches!(c, LlmContent::Thinking(t) if t == "Let me consider..."))
        );
        assert!(
            content.iter().any(|c| matches!(c, LlmContent::Text(t) if t == "The answer is 42."))
        );
    } else {
        panic!("expected assistant message");
    }
}

#[test]
fn orphaned_tool_calls_stripped() {
    // After abort mid-tool-execution, the assistant has a tool call
    // but no corresponding ToolResult. transform_context strips orphaned
    // tool calls to prevent API errors on retry.
    let msgs = vec![
        AgentMessage::User {
            content: vec![ContentItem::Text { text: "read foo.txt".into() }],
            timestamp: 0,
        },
        // Assistant made a tool call, but was aborted before result came back
        AgentMessage::Assistant(AssistantMessage {
            content: vec![ContentBlock::ToolCall {
                id: "call_1".into(),
                name: "read".into(),
                arguments: serde_json::json!({"path": "foo.txt"}),
            }],
            stop_reason: StopReason::Aborted,
            usage: None,
            timestamp: 1,
        }),
        // User sends a new message (no tool result provided)
        AgentMessage::User {
            content: vec![ContentItem::Text { text: "never mind".into() }],
            timestamp: 2,
        },
    ];

    let transformed = transform_context(msgs, 100_000, None);
    // Orphaned tool call stripped → empty assistant removed → 2 messages
    assert_eq!(transformed.len(), 2);
    assert!(matches!(transformed[0], AgentMessage::User { .. }));
    assert!(matches!(transformed[1], AgentMessage::User { .. }));
}

#[test]
fn answered_tool_calls_preserved() {
    let msgs = vec![
        AgentMessage::User {
            content: vec![ContentItem::Text { text: "read foo.txt".into() }],
            timestamp: 0,
        },
        AgentMessage::Assistant(AssistantMessage {
            content: vec![ContentBlock::ToolCall {
                id: "call_1".into(),
                name: "read".into(),
                arguments: serde_json::json!({"path": "foo.txt"}),
            }],
            stop_reason: StopReason::ToolUse,
            usage: None,
            timestamp: 1,
        }),
        AgentMessage::ToolResult {
            tool_call_id: "call_1".into(),
            content: vec![ContentItem::Text { text: "file contents".into() }],
            is_error: false,
            display: None,
            details: None,
            timestamp: 2,
        },
    ];

    let transformed = transform_context(msgs, 100_000, None);
    assert_eq!(transformed.len(), 3);
    // Tool call preserved because it has a matching result
    if let AgentMessage::Assistant(a) = &transformed[1] {
        assert!(a.content.iter().any(|b| matches!(b, ContentBlock::ToolCall { .. })));
    } else {
        panic!("expected assistant");
    }
}

#[test]
fn transform_context_passthrough_when_few_messages() {
    let msgs = vec![
        AgentMessage::User { content: vec![ContentItem::Text { text: "hi".into() }], timestamp: 0 },
        AgentMessage::ToolResult {
            tool_call_id: "tc1".into(),
            content: vec![ContentItem::Text { text: "x".repeat(500) }],
            is_error: false,
            display: None,
            details: None,
            timestamp: 1,
        },
    ];
    let transformed = transform_context(msgs, 100_000, None);
    if let AgentMessage::ToolResult { content, .. } = &transformed[1] {
        let text: String = content
            .iter()
            .filter_map(|c| match c {
                ContentItem::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text.len(), 500, "few messages = no truncation");
    }
}
