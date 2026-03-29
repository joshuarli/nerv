use super::types::*;

/// Wire-format message that providers understand. Providers never see
/// AgentMessage.
#[derive(Debug, Clone)]
pub enum LlmMessage {
    User { content: Vec<LlmContent> },
    Assistant { content: Vec<LlmContent> },
    ToolResult { tool_call_id: String, content: Vec<LlmContent>, is_error: bool },
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
    ToolCall { id: String, name: String, arguments: serde_json::Value },
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
                        ContentBlock::ToolCall { id, name, arguments } => LlmContent::ToolCall {
                            id: id.clone(),
                            name: name.clone(),
                            arguments: arguments.clone(),
                        },
                    })
                    .collect();
                LlmMessage::Assistant { content: items }
            }
            AgentMessage::ToolResult { tool_call_id, content, is_error, .. } => {
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
                LlmMessage::User { content: vec![LlmContent::Text(text)] }
            }
            AgentMessage::BashExecution { command, output, exit_code, .. } => {
                let text = format!(
                    "[Bash execution]\n$ {}\n{}\n[exit code: {}]",
                    command,
                    output,
                    exit_code.unwrap_or(-1)
                );
                LlmMessage::User { content: vec![LlmContent::Text(text)] }
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
            LlmMessage::User { content: existing_content },
            LlmMessage::User { content: new_content },
        ) => {
            existing_content.extend(new_content);
        }
        (
            LlmMessage::Assistant { content: existing_content },
            LlmMessage::Assistant { content: new_content },
        ) => {
            existing_content.extend(new_content);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_message_converts() {
        let msgs = vec![AgentMessage::User {
            content: vec![ContentItem::Text { text: "hello".into() }],
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
                content: vec![ContentItem::Text { text: "result".into() }],
                is_error: false,
                display: None,
                details: None,
                timestamp: 1,
            },
        ];
        let llm = convert_to_llm(&msgs);
        assert_eq!(llm.len(), 2);
    }
}
