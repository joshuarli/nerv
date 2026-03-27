use super::types::*;

/// Wire-format message that providers understand. Providers never see AgentMessage.
#[derive(Debug, Clone)]
pub enum LlmMessage {
    User {
        content: Vec<LlmContent>,
    },
    Assistant {
        content: Vec<LlmContent>,
    },
    ToolResult {
        tool_call_id: String,
        content: Vec<LlmContent>,
        is_error: bool,
    },
}

impl LlmMessage {
    pub fn role(&self) -> &'static str {
        match self {
            Self::User { .. } => "user",
            Self::Assistant { .. } => "assistant",
            Self::ToolResult { .. } => "tool_result",
        }
    }

    pub fn is_user(&self) -> bool {
        matches!(self, Self::User { .. })
    }

    pub fn is_assistant(&self) -> bool {
        matches!(self, Self::Assistant { .. })
    }
}

#[derive(Debug, Clone)]
pub enum LlmContent {
    Text(String),
    Image(ImageSource),
    ToolCall {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
    Thinking(String),
}

/// Convert AgentMessage[] to LlmMessage[]. Non-LLM messages (Custom,
/// BashExecution, CompactionSummary, BranchSummary) become User text messages.
/// Consecutive same-role messages are merged.
pub fn convert_to_llm(messages: &[AgentMessage]) -> Vec<LlmMessage> {
    let mut result: Vec<LlmMessage> = Vec::with_capacity(messages.len());

    for msg in messages {
        let llm_msg = match msg {
            AgentMessage::User { content, .. } => {
                let items = content.iter().map(content_item_to_llm).collect();
                LlmMessage::User { content: items }
            }
            AgentMessage::Assistant(assistant) => {
                let items = assistant
                    .content
                    .iter()
                    .map(|block| match block {
                        ContentBlock::Text { text } => LlmContent::Text(text.clone()),
                        ContentBlock::Thinking { thinking } => {
                            LlmContent::Thinking(thinking.clone())
                        }
                        ContentBlock::ToolCall {
                            id,
                            name,
                            arguments,
                        } => LlmContent::ToolCall {
                            id: id.clone(),
                            name: name.clone(),
                            arguments: arguments.clone(),
                        },
                    })
                    .collect();
                LlmMessage::Assistant { content: items }
            }
            AgentMessage::ToolResult {
                tool_call_id,
                content,
                is_error,
                ..
            } => {
                let items = content.iter().map(content_item_to_llm).collect();
                LlmMessage::ToolResult {
                    tool_call_id: tool_call_id.clone(),
                    content: items,
                    is_error: *is_error,
                }
            }
            AgentMessage::Custom { content, .. } => {
                let text = content
                    .iter()
                    .filter_map(|item| match item {
                        ContentItem::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                LlmMessage::User {
                    content: vec![LlmContent::Text(text)],
                }
            }
            AgentMessage::BashExecution {
                command,
                output,
                exit_code,
                ..
            } => {
                let text = format!(
                    "[Bash execution]\n$ {}\n{}\n[exit code: {}]",
                    command,
                    output,
                    exit_code.unwrap_or(-1)
                );
                LlmMessage::User {
                    content: vec![LlmContent::Text(text)],
                }
            }
            AgentMessage::CompactionSummary { summary, .. } => LlmMessage::User {
                content: vec![LlmContent::Text(format!(
                    "[Context compacted. Summary of previous conversation:]\n{}",
                    summary
                ))],
            },
            AgentMessage::BranchSummary { summary, .. } => LlmMessage::User {
                content: vec![LlmContent::Text(format!("[Branch summary:]\n{}", summary))],
            },
        };

        // Merge consecutive same-role messages (Anthropic requires alternating roles)
        if let Some(last) = result.last_mut()
            && should_merge(last, &llm_msg)
        {
            merge_into(last, llm_msg);
            continue;
        }
        result.push(llm_msg);
    }

    result
}

fn content_item_to_llm(item: &ContentItem) -> LlmContent {
    match item {
        ContentItem::Text { text } => LlmContent::Text(text.clone()),
        ContentItem::Image { source } => LlmContent::Image(source.clone()),
    }
}

fn should_merge(existing: &LlmMessage, new: &LlmMessage) -> bool {
    matches!(
        (existing, new),
        (LlmMessage::User { .. }, LlmMessage::User { .. })
            | (LlmMessage::Assistant { .. }, LlmMessage::Assistant { .. })
    )
}

fn merge_into(existing: &mut LlmMessage, new: LlmMessage) {
    match (existing, new) {
        (
            LlmMessage::User {
                content: existing_content,
            },
            LlmMessage::User {
                content: new_content,
            },
        ) => {
            existing_content.extend(new_content);
        }
        (
            LlmMessage::Assistant {
                content: existing_content,
            },
            LlmMessage::Assistant {
                content: new_content,
            },
        ) => {
            existing_content.extend(new_content);
        }
        _ => {}
    }
}

/// Pre-LLM context transform:
/// 1. Strip orphaned tool calls (no matching ToolResult)
/// 2. Strip thinking blocks (never referenced by the model)
/// 3. Strip args from denied tool calls (is_error + "denied")
/// 4. Truncate stale tool results to save tokens
/// 5. Replace superseded read results (same file read again later)
const RECENT_TURNS: usize = 10;
const TRUNCATED_MAX_CHARS: usize = 200;

pub fn transform_context(messages: Vec<AgentMessage>, _context_window: u32) -> Vec<AgentMessage> {
    // Pass 0: find superseded reads — walk backwards, track latest read per path.
    // Earlier reads of the same file are replaced with a short marker.
    let superseded_ids = find_superseded_reads(&messages);

    // Pass 0b: map tool_call_id → tool name for bash success detection
    let tool_names: std::collections::HashMap<String, String> = messages
        .iter()
        .filter_map(|m| match m {
            AgentMessage::Assistant(a) => Some(a),
            _ => None,
        })
        .flat_map(|a| &a.content)
        .filter_map(|b| match b {
            ContentBlock::ToolCall { id, name, .. } => Some((id.clone(), name.clone())),
            _ => None,
        })
        .collect();

    // Pass 1: collect tool_call_ids that have a ToolResult
    let answered_ids: std::collections::HashSet<String> = messages
        .iter()
        .filter_map(|m| match m {
            AgentMessage::ToolResult { tool_call_id, .. } => Some(tool_call_id.clone()),
            _ => None,
        })
        .collect();

    // Pass 1b: collect tool_call_ids that were denied
    let denied_ids: std::collections::HashSet<String> = messages
        .iter()
        .filter_map(|m| match m {
            AgentMessage::ToolResult {
                tool_call_id,
                content,
                is_error: true,
                ..
            } => {
                let text = content
                    .iter()
                    .filter_map(|c| match c {
                        ContentItem::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>();
                if text.contains("denied") {
                    Some(tool_call_id.clone())
                } else {
                    None
                }
            }
            _ => None,
        })
        .collect();

    // Pass 2: transform
    let cutoff = messages.len().saturating_sub(RECENT_TURNS);
    messages
        .into_iter()
        .enumerate()
        .filter_map(|(i, msg)| match msg {
            AgentMessage::Assistant(mut a) => {
                // Remove orphaned tool calls
                a.content.retain(|block| match block {
                    ContentBlock::ToolCall { id, .. } => answered_ids.contains(id),
                    _ => true,
                });

                // Strip thinking blocks — they're never referenced in context
                a.content
                    .retain(|block| !matches!(block, ContentBlock::Thinking { .. }));

                // Strip args from denied tool calls.
                // For stale turns, also strip bulky edit/write args (keep only path).
                a.content = a
                    .content
                    .into_iter()
                    .map(|block| match block {
                        ContentBlock::ToolCall { id, name, .. } if denied_ids.contains(&id) => {
                            ContentBlock::ToolCall {
                                id,
                                name,
                                arguments: serde_json::json!({}),
                            }
                        }
                        ContentBlock::ToolCall {
                            id,
                            ref name,
                            ref arguments,
                        } if i < cutoff && (name == "edit" || name == "write") => {
                            let path = arguments
                                .get("path")
                                .cloned()
                                .unwrap_or(serde_json::json!(""));
                            ContentBlock::ToolCall {
                                id,
                                name: name.clone(),
                                arguments: serde_json::json!({"path": path}),
                            }
                        }
                        other => other,
                    })
                    .collect();

                if a.content.is_empty() {
                    None
                } else {
                    Some(AgentMessage::Assistant(a))
                }
            }
            AgentMessage::ToolResult {
                tool_call_id,
                content: _,
                is_error,
                timestamp,
            } if superseded_ids.contains(&tool_call_id) => {
                Some(AgentMessage::ToolResult {
                    tool_call_id,
                    content: vec![ContentItem::Text {
                        text: "[superseded by later read]".into(),
                    }],
                    is_error,
                    timestamp,
                })
            }
            AgentMessage::ToolResult {
                tool_call_id,
                content,
                is_error: false,
                timestamp,
            } if tool_names.get(&tool_call_id).map(|n| n.as_str()) == Some("bash") => {
                let text = content_text(&content);
                if let Some(compressed) = compress_bash_success(&text) {
                    Some(AgentMessage::ToolResult {
                        tool_call_id,
                        content: vec![ContentItem::Text { text: compressed }],
                        is_error: false,
                        timestamp,
                    })
                } else if i < cutoff {
                    let summary = summarize_tool_content(&content);
                    Some(AgentMessage::ToolResult {
                        tool_call_id,
                        content: vec![ContentItem::Text { text: summary }],
                        is_error: false,
                        timestamp,
                    })
                } else {
                    Some(AgentMessage::ToolResult {
                        tool_call_id,
                        content,
                        is_error: false,
                        timestamp,
                    })
                }
            }
            AgentMessage::ToolResult {
                tool_call_id,
                content,
                is_error,
                timestamp,
            } if i < cutoff => {
                let summary = summarize_tool_content(&content);
                Some(AgentMessage::ToolResult {
                    tool_call_id,
                    content: vec![ContentItem::Text { text: summary }],
                    is_error,
                    timestamp,
                })
            }
            other => Some(other),
        })
        .collect()
}

fn summarize_tool_content(content: &[ContentItem]) -> String {
    let full_text: String = content
        .iter()
        .filter_map(|item| match item {
            ContentItem::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");

    let line_count = full_text.lines().count();
    let char_count = full_text.len();

    if char_count <= TRUNCATED_MAX_CHARS {
        return full_text; // small enough to keep
    }

    // Take first few lines as preview
    let preview: String = full_text.lines().take(3).collect::<Vec<_>>().join("\n");
    let preview = if preview.len() > TRUNCATED_MAX_CHARS {
        format!("{}...", &preview[..TRUNCATED_MAX_CHARS])
    } else {
        preview
    };

    format!(
        "{}\n[truncated: {} lines, {} chars]",
        preview, line_count, char_count
    )
}

fn content_text(content: &[ContentItem]) -> String {
    content
        .iter()
        .filter_map(|c| match c {
            ContentItem::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Detect bash success patterns and return a one-line summary.
/// Returns None if the output doesn't match a known success pattern.
fn compress_bash_success(text: &str) -> Option<String> {
    // cargo check / cargo build with no errors
    if (text.contains("Compiling") || text.contains("Checking") || text.contains("Finished"))
        && !text.contains("error[")
        && !text.contains("error:")
    {
        if let Some(line) = text.lines().rfind(|l| l.contains("Finished")) {
            return Some(line.trim().to_string());
        }
    }

    // cargo test with all passing
    if let Some(line) = text.lines().rfind(|l| l.starts_with("test result:")) {
        if line.contains("0 failed") {
            return Some(line.trim().to_string());
        }
    }

    // Python unittest: "Ran N tests... OK"
    if text.contains("Ran ") && text.lines().any(|l| l.trim() == "OK") {
        if let Some(line) = text.lines().rfind(|l| l.starts_with("Ran ")) {
            return Some(line.trim().to_string());
        }
    }

    // pytest: "N passed" with no failures
    if let Some(line) = text.lines().rfind(|l| l.contains(" passed")) {
        if !line.contains("failed") && !line.contains("error") {
            return Some(line.trim().to_string());
        }
    }

    // make: no error, last line is a target or "Nothing to be done"
    if text.contains("make") && !text.contains("Error ") && !text.contains("error:") {
        if let Some(line) = text.lines().rfind(|l| l.contains("Nothing to be done")) {
            return Some(line.trim().to_string());
        }
    }

    None
}

/// Walk backwards through messages. For each "read" tool call, track the path.
/// If an earlier read targeted the same path, mark its tool_call_id as superseded.
/// Only supersede successful reads (not errors) — the model may need error context.
fn find_superseded_reads(messages: &[AgentMessage]) -> std::collections::HashSet<String> {
    let mut latest_read_path: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut superseded = std::collections::HashSet::new();

    // Walk backwards: the first read we see for a path is the latest
    for msg in messages.iter().rev() {
        if let AgentMessage::Assistant(a) = msg {
            for block in &a.content {
                if let ContentBlock::ToolCall {
                    id,
                    name,
                    arguments,
                } = block
                {
                    if name == "read" {
                        if let Some(path) = arguments.get("path").and_then(|v| v.as_str()) {
                            if let Some(_latest_id) = latest_read_path.get(path) {
                                // This is an earlier read of the same path — supersede it
                                superseded.insert(id.clone());
                            } else {
                                latest_read_path.insert(path.to_string(), id.clone());
                            }
                        }
                    }
                }
            }
        }
    }

    // Don't supersede reads whose results were errors (model may need to see the error)
    for msg in messages {
        if let AgentMessage::ToolResult {
            tool_call_id,
            is_error: true,
            ..
        } = msg
        {
            superseded.remove(tool_call_id);
        }
    }

    superseded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_message_converts() {
        let msgs = vec![AgentMessage::User {
            content: vec![ContentItem::Text {
                text: "hello".into(),
            }],
            timestamp: 0,
        }];
        let llm = convert_to_llm(&msgs);
        assert_eq!(llm.len(), 1);
        assert!(llm[0].is_user());
    }

    #[test]
    fn consecutive_user_messages_merge() {
        let msgs = vec![
            AgentMessage::User {
                content: vec![ContentItem::Text { text: "a".into() }],
                timestamp: 0,
            },
            AgentMessage::Custom {
                custom_type: "note".into(),
                content: vec![ContentItem::Text { text: "b".into() }],
                display: false,
                timestamp: 1,
            },
        ];
        let llm = convert_to_llm(&msgs);
        // Both become User, so they merge
        assert_eq!(llm.len(), 1);
    }

    #[test]
    fn tool_result_not_merged() {
        let msgs = vec![
            AgentMessage::User {
                content: vec![ContentItem::Text { text: "a".into() }],
                timestamp: 0,
            },
            AgentMessage::ToolResult {
                tool_call_id: "t1".into(),
                content: vec![ContentItem::Text {
                    text: "result".into(),
                }],
                is_error: false,
                timestamp: 1,
            },
        ];
        let llm = convert_to_llm(&msgs);
        assert_eq!(llm.len(), 2);
    }

    // -----------------------------------------------------------------------
    // Helpers for building realistic conversations
    // -----------------------------------------------------------------------

    fn user(text: &str) -> AgentMessage {
        AgentMessage::User {
            content: vec![ContentItem::Text { text: text.into() }],
            timestamp: 0,
        }
    }

    fn assistant_text(text: &str) -> AgentMessage {
        AgentMessage::Assistant(AssistantMessage {
            content: vec![ContentBlock::Text { text: text.into() }],
            stop_reason: StopReason::EndTurn,
            usage: None,
            timestamp: 0,
        })
    }

    fn assistant_thinking_text(thinking: &str, text: &str) -> AgentMessage {
        AgentMessage::Assistant(AssistantMessage {
            content: vec![
                ContentBlock::Thinking {
                    thinking: thinking.into(),
                },
                ContentBlock::Text { text: text.into() },
            ],
            stop_reason: StopReason::EndTurn,
            usage: None,
            timestamp: 0,
        })
    }

    fn assistant_tool_call(id: &str, name: &str, args: serde_json::Value) -> AgentMessage {
        AgentMessage::Assistant(AssistantMessage {
            content: vec![ContentBlock::ToolCall {
                id: id.into(),
                name: name.into(),
                arguments: args,
            }],
            stop_reason: StopReason::ToolUse,
            usage: None,
            timestamp: 0,
        })
    }

    fn assistant_thinking_tool_call(
        thinking: &str,
        id: &str,
        name: &str,
        args: serde_json::Value,
    ) -> AgentMessage {
        AgentMessage::Assistant(AssistantMessage {
            content: vec![
                ContentBlock::Thinking {
                    thinking: thinking.into(),
                },
                ContentBlock::ToolCall {
                    id: id.into(),
                    name: name.into(),
                    arguments: args,
                },
            ],
            stop_reason: StopReason::ToolUse,
            usage: None,
            timestamp: 0,
        })
    }

    fn tool_result(id: &str, content: &str) -> AgentMessage {
        AgentMessage::ToolResult {
            tool_call_id: id.into(),
            content: vec![ContentItem::Text {
                text: content.into(),
            }],
            is_error: false,
            timestamp: 0,
        }
    }

    fn tool_error(id: &str, content: &str) -> AgentMessage {
        AgentMessage::ToolResult {
            tool_call_id: id.into(),
            content: vec![ContentItem::Text {
                text: content.into(),
            }],
            is_error: true,
            timestamp: 0,
        }
    }

    fn tool_denied(id: &str) -> AgentMessage {
        tool_error(id, "Tool call denied by user.")
    }

    fn get_assistant(msg: &AgentMessage) -> &AssistantMessage {
        match msg {
            AgentMessage::Assistant(a) => a,
            _ => panic!("expected assistant message"),
        }
    }

    fn count_thinking(msgs: &[AgentMessage]) -> usize {
        msgs.iter()
            .filter_map(|m| match m {
                AgentMessage::Assistant(a) => Some(a),
                _ => None,
            })
            .flat_map(|a| &a.content)
            .filter(|b| matches!(b, ContentBlock::Thinking { .. }))
            .count()
    }

    fn total_text_len(msgs: &[AgentMessage]) -> usize {
        msgs.iter()
            .map(|m| match m {
                AgentMessage::User { content, .. } | AgentMessage::ToolResult { content, .. } => {
                    content
                        .iter()
                        .map(|c| match c {
                            ContentItem::Text { text } => text.len(),
                            _ => 0,
                        })
                        .sum()
                }
                AgentMessage::Assistant(a) => a
                    .content
                    .iter()
                    .map(|b| match b {
                        ContentBlock::Text { text } => text.len(),
                        ContentBlock::Thinking { thinking } => thinking.len(),
                        ContentBlock::ToolCall { arguments, .. } => arguments.to_string().len(),
                    })
                    .sum(),
                _ => 0,
            })
            .sum()
    }

    // -----------------------------------------------------------------------
    // Context optimization tests
    // -----------------------------------------------------------------------

    #[test]
    fn thinking_blocks_stripped() {
        let msgs = vec![AgentMessage::Assistant(AssistantMessage {
            content: vec![
                ContentBlock::Thinking {
                    thinking: "Let me think about this very carefully...".into(),
                },
                ContentBlock::Text {
                    text: "The answer is 42.".into(),
                },
            ],
            stop_reason: StopReason::EndTurn,
            usage: None,
            timestamp: 0,
        })];
        let result = transform_context(msgs, 200_000);
        let a = match &result[0] {
            AgentMessage::Assistant(a) => a,
            _ => panic!("expected assistant"),
        };
        assert_eq!(a.content.len(), 1);
        assert!(matches!(&a.content[0], ContentBlock::Text { .. }));
    }

    #[test]
    fn denied_tool_args_stripped() {
        let msgs = vec![
            AgentMessage::Assistant(AssistantMessage {
                content: vec![ContentBlock::ToolCall {
                    id: "t1".into(),
                    name: "write".into(),
                    arguments: serde_json::json!({"path": "/etc/evil", "content": "x".repeat(5000)}),
                }],
                stop_reason: StopReason::ToolUse,
                usage: None,
                timestamp: 0,
            }),
            AgentMessage::ToolResult {
                tool_call_id: "t1".into(),
                content: vec![ContentItem::Text {
                    text: "Tool call denied by user.".into(),
                }],
                is_error: true,
                timestamp: 1,
            },
        ];
        let result = transform_context(msgs, 200_000);
        let a = match &result[0] {
            AgentMessage::Assistant(a) => a,
            _ => panic!("expected assistant"),
        };
        if let ContentBlock::ToolCall { arguments, .. } = &a.content[0] {
            assert_eq!(
                *arguments,
                serde_json::json!({}),
                "denied tool args should be stripped"
            );
        } else {
            panic!("expected tool call");
        }
    }

    #[test]
    fn non_denied_tool_args_preserved() {
        let msgs = vec![
            AgentMessage::Assistant(AssistantMessage {
                content: vec![ContentBlock::ToolCall {
                    id: "t1".into(),
                    name: "read".into(),
                    arguments: serde_json::json!({"path": "src/main.rs"}),
                }],
                stop_reason: StopReason::ToolUse,
                usage: None,
                timestamp: 0,
            }),
            AgentMessage::ToolResult {
                tool_call_id: "t1".into(),
                content: vec![ContentItem::Text {
                    text: "file contents...".into(),
                }],
                is_error: false,
                timestamp: 1,
            },
        ];
        let result = transform_context(msgs, 200_000);
        let a = match &result[0] {
            AgentMessage::Assistant(a) => a,
            _ => panic!("expected assistant"),
        };
        if let ContentBlock::ToolCall { arguments, .. } = &a.content[0] {
            assert_eq!(
                arguments["path"], "src/main.rs",
                "successful tool args should be preserved"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Realistic conversation scenarios
    // -----------------------------------------------------------------------

    /// Simulate: user asks to fix a bug. Model thinks hard, reads a file,
    /// edits it, runs tests. 15 turns with large thinking + large diffs.
    #[test]
    fn realistic_bug_fix_session() {
        let big_thinking = "a]".repeat(5000); // ~10k chars of thinking
        let big_file = (0..200)
            .map(|i| format!("    {} fn line_{}() {{}}", i + 1, i))
            .collect::<Vec<_>>()
            .join("\n");
        let mut msgs = vec![
            user("Fix the off-by-one error in parser.rs"),
            // Model thinks, then reads file
            assistant_thinking_tool_call(
                &big_thinking,
                "t1",
                "read",
                serde_json::json!({"path": "src/parser.rs"}),
            ),
            tool_result("t1", &big_file),
            // Model thinks again, edits
            assistant_thinking_tool_call(
                &big_thinking,
                "t2",
                "edit",
                serde_json::json!({
                    "path": "src/parser.rs",
                    "old_text": "old_code();",
                    "new_text": "new_code();"
                }),
            ),
            tool_result("t2", "Applied edit to src/parser.rs"),
            // Model runs tests
            assistant_thinking_tool_call(
                &big_thinking,
                "t3",
                "bash",
                serde_json::json!({"command": "cargo test"}),
            ),
            tool_result(
                "t3",
                &format!("test result: ok. 50 passed\n{}", "output ".repeat(500)),
            ),
            // Final response
            assistant_thinking_text(&big_thinking, "Fixed the off-by-one error."),
        ];

        // Add padding to push early messages past the truncation cutoff
        for i in 0..8 {
            msgs.push(user(&format!("follow-up question {}", i)));
            msgs.push(assistant_text(&format!("answer {}", i)));
        }

        let before_len = total_text_len(&msgs);
        let result = transform_context(msgs, 200_000);
        let after_len = total_text_len(&result);

        // All thinking should be gone
        assert_eq!(
            count_thinking(&result),
            0,
            "all thinking blocks should be stripped"
        );
        // Should save significant tokens (4 thinking blocks × ~10k each)
        assert!(
            after_len < before_len / 2,
            "should save >50% tokens: before={} after={}",
            before_len,
            after_len
        );
        // Early tool results should be truncated
        let early_result = match &result[2] {
            AgentMessage::ToolResult { content, .. } => content
                .iter()
                .filter_map(|c| match c {
                    ContentItem::Text { text } => Some(text.len()),
                    _ => None,
                })
                .sum::<usize>(),
            _ => panic!("expected tool result at index 2"),
        };
        assert!(
            early_result < 300,
            "early tool result should be truncated: {}",
            early_result
        );
    }

    /// Simulate: model tries to write outside repo, denied. Then tries
    /// again inside repo, succeeds. The denied write had a 5KB payload.
    #[test]
    fn denied_write_then_retry() {
        let big_content = "x".repeat(5000);
        let msgs = vec![
            user("Create a config file"),
            // First attempt — outside repo, denied
            assistant_tool_call(
                "t1",
                "write",
                serde_json::json!({"path": "/etc/myapp.conf", "content": big_content}),
            ),
            tool_denied("t1"),
            // Model apologizes and retries inside repo
            assistant_tool_call(
                "t2",
                "write",
                serde_json::json!({"path": "config/myapp.conf", "content": big_content}),
            ),
            tool_result("t2", "Created config/myapp.conf"),
            assistant_text("Created the config file at config/myapp.conf instead."),
        ];

        let result = transform_context(msgs, 200_000);

        // Denied tool call args should be stripped
        let denied_assistant = get_assistant(&result[1]);
        if let ContentBlock::ToolCall {
            arguments, name, ..
        } = &denied_assistant.content[0]
        {
            assert_eq!(name, "write");
            assert_eq!(
                *arguments,
                serde_json::json!({}),
                "denied write args should be empty"
            );
        } else {
            panic!("expected tool call");
        }

        // Successful tool call args should be preserved
        let ok_assistant = get_assistant(&result[3]);
        if let ContentBlock::ToolCall { arguments, .. } = &ok_assistant.content[0] {
            assert!(arguments["content"].as_str().unwrap().len() == 5000);
        } else {
            panic!("expected tool call");
        }
    }

    /// Simulate: model reads same file 3 times, edits fail twice, succeeds
    /// on third. Old results should be truncated.
    #[test]
    fn repeated_read_edit_failures() {
        let file_content = "line\n".repeat(500); // ~2500 chars
        let mut msgs = Vec::new();

        msgs.push(user("Refactor the database module"));

        // Three read-edit cycles
        for i in 0..3 {
            let read_id = format!("r{}", i);
            let edit_id = format!("e{}", i);
            msgs.push(assistant_tool_call(
                &read_id,
                "read",
                serde_json::json!({"path": "src/db.rs"}),
            ));
            msgs.push(tool_result(&read_id, &file_content));

            msgs.push(assistant_tool_call(
                &edit_id,
                "edit",
                serde_json::json!({
                    "path": "src/db.rs",
                    "old_text": format!("old_v{}", i),
                    "new_text": format!("new_v{}", i)
                }),
            ));
            if i < 2 {
                msgs.push(tool_error(&edit_id, "No match found for old_text"));
            } else {
                msgs.push(tool_result(&edit_id, "Applied edit to src/db.rs"));
            }
        }

        // Pad to push early turns past cutoff
        for i in 0..8 {
            msgs.push(user(&format!("q{}", i)));
            msgs.push(assistant_text(&format!("a{}", i)));
        }

        msgs.push(assistant_text("Done refactoring."));

        let result = transform_context(msgs, 200_000);

        // Early read results (first two) should be truncated
        let first_read_result = &result[2]; // tool_result for r0
        if let AgentMessage::ToolResult { content, .. } = first_read_result {
            let text: String = content
                .iter()
                .filter_map(|c| match c {
                    ContentItem::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            assert!(
                text.len() < 300,
                "first read result should be truncated: {} chars",
                text.len()
            );
        }
    }

    /// Simulate: model produces massive thinking blocks across multiple turns.
    /// Verify all are stripped and we preserve message structure.
    #[test]
    fn massive_thinking_stripped_preserves_structure() {
        let huge_thinking = "reasoning step\n".repeat(2000); // ~30k chars
        let msgs = vec![
            user("Explain quantum computing"),
            assistant_thinking_text(&huge_thinking, "Here's a brief explanation."),
            user("Now explain it in more detail"),
            assistant_thinking_text(&huge_thinking, "In more detail: ..."),
            user("Compare with classical computing"),
            assistant_thinking_text(&huge_thinking, "The key differences are: ..."),
        ];

        let before_thinking = count_thinking(&msgs);
        assert_eq!(before_thinking, 3);
        let before_len = total_text_len(&msgs);

        let result = transform_context(msgs, 200_000);

        assert_eq!(count_thinking(&result), 0);
        assert_eq!(result.len(), 6, "message count should be preserved");
        let after_len = total_text_len(&result);
        // 3 × 30k thinking = ~90k removed
        assert!(
            before_len - after_len > 80_000,
            "should strip >80k chars of thinking: saved {}",
            before_len - after_len
        );
    }

    /// Simulate: orphaned tool calls (model called tool but was aborted
    /// before result came back).
    #[test]
    fn orphaned_tool_calls_from_abort() {
        let msgs = vec![
            user("Read all the files"),
            // Model starts two tool calls but gets aborted
            AgentMessage::Assistant(AssistantMessage {
                content: vec![
                    ContentBlock::ToolCall {
                        id: "t1".into(),
                        name: "read".into(),
                        arguments: serde_json::json!({"path": "file1.rs"}),
                    },
                    ContentBlock::ToolCall {
                        id: "t2".into(),
                        name: "read".into(),
                        arguments: serde_json::json!({"path": "file2.rs"}),
                    },
                ],
                stop_reason: StopReason::Aborted,
                usage: None,
                timestamp: 0,
            }),
            // Only t1 got a result before abort
            tool_result("t1", "contents of file1"),
            // User continues
            user("Just read file1 please"),
            assistant_text("Based on file1: ..."),
        ];

        let result = transform_context(msgs, 200_000);

        // The assistant message should only have t1 (t2 is orphaned)
        let a = get_assistant(&result[1]);
        assert_eq!(a.content.len(), 1, "orphaned t2 should be removed");
        if let ContentBlock::ToolCall { id, .. } = &a.content[0] {
            assert_eq!(id, "t1");
        }
    }

    /// Simulate: mixed conversation with thinking, tool calls, denials,
    /// errors, and successful operations. The kitchen sink test.
    #[test]
    fn kitchen_sink_conversation() {
        let thinking = "detailed analysis\n".repeat(500);
        let big_write = "x".repeat(3000);

        let msgs = vec![
            // Turn 1: user asks, model thinks and reads
            user("Set up a new REST API endpoint"),
            assistant_thinking_tool_call(
                &thinking,
                "t1",
                "read",
                serde_json::json!({"path": "src/routes.rs"}),
            ),
            tool_result("t1", &"fn route() {}\n".repeat(100)),
            // Turn 2: model tries to write outside repo — denied
            assistant_thinking_tool_call(
                &thinking,
                "t2",
                "write",
                serde_json::json!({"path": "/var/log/api.log", "content": big_write}),
            ),
            tool_denied("t2"),
            // Turn 3: model edits correctly
            assistant_thinking_tool_call(
                &thinking,
                "t3",
                "edit",
                serde_json::json!({
                    "path": "src/routes.rs",
                    "old_text": "fn route() {}",
                    "new_text": "fn new_endpoint() { /* ... */ }"
                }),
            ),
            tool_result("t3", "Applied edit"),
            // Turn 4: model runs tests — they fail
            assistant_tool_call("t4", "bash", serde_json::json!({"command": "cargo test"})),
            tool_error("t4", &format!("FAILED\n{}", "error output\n".repeat(200))),
            // Turn 5: model fixes and retries
            assistant_thinking_tool_call(
                &thinking,
                "t5",
                "edit",
                serde_json::json!({
                    "path": "src/routes.rs",
                    "old_text": "fn new_endpoint() { /* ... */ }",
                    "new_text": "fn new_endpoint() -> Result<()> { Ok(()) }"
                }),
            ),
            tool_result("t5", "Applied edit"),
            assistant_tool_call("t6", "bash", serde_json::json!({"command": "cargo test"})),
            tool_result("t6", "test result: ok. 50 passed"),
            // Final response
            assistant_thinking_text(&thinking, "The endpoint is set up and tests pass."),
        ];

        let msg_count = msgs.len();
        let before_len = total_text_len(&msgs);
        let result = transform_context(msgs, 200_000);

        // Structure preserved
        assert_eq!(result.len(), msg_count, "all messages should be present");

        // No thinking blocks
        assert_eq!(count_thinking(&result), 0);

        // Denied write args stripped
        let denied = get_assistant(&result[3]);
        if let ContentBlock::ToolCall { arguments, .. } = &denied.content[0] {
            assert_eq!(*arguments, serde_json::json!({}));
        }

        // Significant token savings
        let after_len = total_text_len(&result);
        let saved = before_len - after_len;
        assert!(
            saved > 10_000,
            "should save significant tokens: before={} after={} saved={}",
            before_len,
            after_len,
            saved
        );
    }

    /// Verify stale tool results are truncated but recent ones aren't.
    #[test]
    fn stale_vs_recent_tool_results() {
        let big_output = "x".repeat(1000);
        let mut msgs = Vec::new();

        // 15 turns of read operations
        for i in 0..15 {
            let id = format!("t{}", i);
            msgs.push(user(&format!("read file {}", i)));
            msgs.push(assistant_tool_call(
                &id,
                "read",
                serde_json::json!({"path": format!("src/mod_{}.rs", i)}),
            ));
            msgs.push(tool_result(&id, &big_output));
            msgs.push(assistant_text(&format!("Here's what's in mod_{}.rs", i)));
        }

        let result = transform_context(msgs, 200_000);

        // Early results (within first cutoff) should be truncated
        // Cutoff = len - RECENT_TURNS (10), so turns 0-9 are before cutoff
        let early_result = match &result[2] {
            AgentMessage::ToolResult { content, .. } => content
                .iter()
                .filter_map(|c| match c {
                    ContentItem::Text { text } => Some(text.len()),
                    _ => None,
                })
                .sum::<usize>(),
            _ => panic!("expected tool result"),
        };
        assert!(
            early_result < 300,
            "early result should be truncated: {} chars",
            early_result
        );

        // Recent results (last few) should be preserved in full
        let last_result_idx = result.len() - 2; // second to last = last tool_result
        let recent_result = match &result[last_result_idx] {
            AgentMessage::ToolResult { content, .. } => content
                .iter()
                .filter_map(|c| match c {
                    ContentItem::Text { text } => Some(text.len()),
                    _ => None,
                })
                .sum::<usize>(),
            _ => panic!("expected tool result at {}", last_result_idx),
        };
        assert_eq!(
            recent_result, 1000,
            "recent result should be preserved in full"
        );
    }

    #[test]
    fn superseded_reads_replaced() {
        let msgs = vec![
            user("refactor the module"),
            // First read of main.rs
            assistant_tool_call("r1", "read", serde_json::json!({"path": "src/main.rs"})),
            tool_result("r1", &"fn main() {}\n".repeat(100)),
            // Edit
            assistant_tool_call("e1", "edit", serde_json::json!({"path": "src/main.rs", "old_text": "a", "new_text": "b"})),
            tool_result("e1", "Applied edit"),
            // Second read of same file (supersedes first)
            assistant_tool_call("r2", "read", serde_json::json!({"path": "src/main.rs"})),
            tool_result("r2", &"fn main() { updated }\n".repeat(100)),
            assistant_text("Done."),
        ];

        let result = transform_context(msgs, 200_000);

        // First read result should be superseded
        let first_read = match &result[2] {
            AgentMessage::ToolResult { content, .. } => content_text(content),
            _ => panic!("expected tool result"),
        };
        assert!(
            first_read.contains("superseded"),
            "first read should be superseded: {}",
            first_read
        );

        // Second read should be preserved
        let second_read = match &result[6] {
            AgentMessage::ToolResult { content, .. } => content_text(content),
            _ => panic!("expected tool result"),
        };
        assert!(
            second_read.contains("updated"),
            "second read should be preserved"
        );
    }

    #[test]
    fn bash_success_compressed() {
        let cargo_check_output = "\
   Compiling nerv v0.1.0 (/tmp/nerv)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.2s";

        let cargo_test_output = "\
running 25 tests
.........................
test result: ok. 25 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.06s";

        assert_eq!(
            compress_bash_success(cargo_check_output),
            Some("Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.2s".into())
        );
        assert_eq!(
            compress_bash_success(cargo_test_output),
            Some("test result: ok. 25 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.06s".into())
        );

        // Error output should NOT be compressed
        let error_output = "error[E0308]: mismatched types";
        assert_eq!(compress_bash_success(error_output), None);
    }

    #[test]
    fn stale_edit_args_stripped() {
        let big_content = "x".repeat(3000);
        let mut msgs = vec![
            user("refactor"),
            assistant_tool_call(
                "e1",
                "edit",
                serde_json::json!({
                    "path": "src/lib.rs",
                    "old_text": big_content,
                    "new_text": "replaced"
                }),
            ),
            tool_result("e1", "Edited src/lib.rs"),
        ];

        // Pad to push the edit past cutoff
        for i in 0..12 {
            msgs.push(user(&format!("q{}", i)));
            msgs.push(assistant_text(&format!("a{}", i)));
        }

        let result = transform_context(msgs, 200_000);
        let edit_msg = get_assistant(&result[1]);
        if let ContentBlock::ToolCall { arguments, .. } = &edit_msg.content[0] {
            // Should only have path, not old_text/new_text
            assert!(
                arguments.get("path").is_some(),
                "path should be preserved"
            );
            assert!(
                arguments.get("old_text").is_none(),
                "old_text should be stripped from stale edit"
            );
            assert!(
                arguments.get("new_text").is_none(),
                "new_text should be stripped from stale edit"
            );
        } else {
            panic!("expected tool call");
        }
    }

    #[test]
    fn bash_success_python_unittest() {
        let output = "\
.....................
----------------------------------------------------------------------
Ran 21 tests in 0.003s

OK";
        assert_eq!(
            compress_bash_success(output),
            Some("Ran 21 tests in 0.003s".into())
        );
    }

    #[test]
    fn bash_success_pytest() {
        let output = "\
============================= test session starts ==============================
collected 15 items

test_foo.py ...............                                              [100%]

============================== 15 passed in 0.42s ==============================";
        assert_eq!(
            compress_bash_success(output),
            Some("============================== 15 passed in 0.42s ==============================".into())
        );
    }

    #[test]
    fn bash_failure_not_compressed() {
        // Python unittest failure
        let fail = "\
..F..
FAILED (failures=1)";
        assert_eq!(compress_bash_success(fail), None);

        // cargo test failure
        let cargo_fail = "test result: FAILED. 24 passed; 1 failed; 0 ignored";
        assert_eq!(compress_bash_success(cargo_fail), None);

        // cargo check error
        let check_fail = "Compiling nerv v0.1.0\nerror[E0308]: mismatched types";
        assert_eq!(compress_bash_success(check_fail), None);
    }

    #[test]
    fn recent_edit_args_preserved() {
        // Edit in recent turns should keep full args
        let big_content = "x".repeat(3000);
        let msgs = vec![
            user("fix it"),
            assistant_tool_call(
                "e1",
                "edit",
                serde_json::json!({
                    "path": "src/lib.rs",
                    "old_text": big_content,
                    "new_text": "replaced"
                }),
            ),
            tool_result("e1", "Edited src/lib.rs"),
            assistant_text("Done."),
        ];

        let result = transform_context(msgs, 200_000);
        let edit_msg = get_assistant(&result[1]);
        if let ContentBlock::ToolCall { arguments, .. } = &edit_msg.content[0] {
            assert!(
                arguments.get("old_text").is_some(),
                "recent edit should preserve old_text"
            );
        } else {
            panic!("expected tool call");
        }
    }
}
