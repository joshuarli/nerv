use std::time::Duration;

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use nerv::agent::convert::{LlmMessage, convert_to_llm};
use nerv::agent::provider::*;
use nerv::agent::transform::transform_context;
use nerv::agent::types::*;
use nerv::agent::{AnthropicProvider, OpenAICompatProvider};

fn pgo_criterion() -> Criterion {
    // For `make pgo-profile`: just hit the hot paths, no statistical rigor needed.
    Criterion::default()
        .warm_up_time(Duration::from_millis(1))
        .measurement_time(Duration::from_millis(10))
        .sample_size(10)
}

fn fast() -> Criterion {
    if std::env::var("PGO_PROFILE").is_ok() {
        return pgo_criterion();
    }
    Criterion::default()
        .warm_up_time(Duration::from_millis(200))
        .measurement_time(Duration::from_secs(2))
}

fn make_system_prompt(tokens: usize) -> String {
    // ~4 chars per token
    "You are an expert coding assistant. ".repeat(tokens / 9 + 1)[..tokens * 4].to_string()
}

fn make_tools(count: usize) -> Vec<WireTool> {
    (0..count)
        .map(|i| WireTool {
            name: format!("tool_{}", i),
            description: format!("Description for tool {} that does something useful", i),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "The file path"},
                    "content": {"type": "string", "description": "The content to write"},
                },
                "required": ["path"]
            }),
        })
        .collect()
}

fn make_conversation(turns: usize) -> Vec<AgentMessage> {
    let mut messages = Vec::with_capacity(turns * 2);
    for i in 0..turns {
        messages.push(AgentMessage::User {
            content: vec![ContentItem::Text {
                text: format!(
                    "Please read the file src/main.rs and tell me about the function on line {}. Also check for any issues with error handling.",
                    i * 10
                ),
            }],
            timestamp: 1000 + i as u64,
        });
        messages.push(AgentMessage::Assistant(AssistantMessage {
            content: vec![
                ContentBlock::Text {
                    text: format!(
                        "I'll read that file for you. Looking at line {}, I can see the function handles several cases. Let me explain the key parts:\n\n1. The main loop processes events from the channel\n2. Error handling uses Result types throughout\n3. The function returns early on critical errors\n\nHere's what I found interesting about the implementation...",
                        i * 10
                    ),
                },
            ],
            stop_reason: StopReason::EndTurn,
            usage: Some(Usage {
                input: 500 + i as u32 * 100,
                output: 200,
                ..Default::default()
            }),
            timestamp: 2000 + i as u64,
        }));
    }
    messages
}

fn make_conversation_with_tools(turns: usize) -> Vec<AgentMessage> {
    let mut messages = Vec::with_capacity(turns * 4);
    for i in 0..turns {
        messages.push(AgentMessage::User {
            content: vec![ContentItem::Text { text: format!("Read file src/module_{}.rs", i) }],
            timestamp: 1000 + i as u64,
        });
        messages.push(AgentMessage::Assistant(AssistantMessage {
            content: vec![ContentBlock::ToolCall {
                id: format!("toolu_{:08x}", i),
                name: "read".into(),
                arguments: serde_json::json!({"path": format!("src/module_{}.rs", i)}),
            }],
            stop_reason: StopReason::ToolUse,
            usage: Some(Usage { input: 500, output: 50, ..Default::default() }),
            timestamp: 2000 + i as u64,
        }));
        messages.push(AgentMessage::ToolResult {
            tool_call_id: format!("toolu_{:08x}", i),
            content: vec![ContentItem::Text {
                text: format!(
                    "use std::io;\n\nfn process_item_{i}(input: &str) -> io::Result<String> {{\n    let trimmed = input.trim();\n    if trimmed.is_empty() {{\n        return Err(io::Error::new(io::ErrorKind::InvalidInput, \"empty\"));\n    }}\n    Ok(trimmed.to_uppercase())\n}}\n"
                ),
            }],
            is_error: false,
            display: None,
            timestamp: 3000 + i as u64,
        });
        messages.push(AgentMessage::Assistant(AssistantMessage {
            content: vec![ContentBlock::Text {
                text: format!("The module_{i}.rs file contains a `process_item_{i}` function that takes a string, trims it, validates it's not empty, and returns the uppercase version."),
            }],
            stop_reason: StopReason::EndTurn,
            usage: Some(Usage {
                input: 800,
                output: 100,
                ..Default::default()
            }),
            timestamp: 4000 + i as u64,
        }));
    }
    messages
}

fn make_completion_request(
    system_prompt: &str,
    messages: &[LlmMessage],
    tools: &[WireTool],
) -> CompletionRequest {
    CompletionRequest {
        model_id: "claude-sonnet-4-6".into(),
        system_prompt: system_prompt.to_string(),
        messages: messages.to_vec(),
        tools: tools.to_vec(),
        max_tokens: 32000,
        thinking: None,
        cache: CacheConfig::default(),
    }
}

fn bench_transform_context(c: &mut Criterion) {
    let mut group = c.benchmark_group("transform_context");
    // 100 and 200 turns exercise the stale-truncation and dedup optimisations
    // that only activate once the context grows beyond RECENT_TURNS
    for turns in [5, 20, 50, 100, 200] {
        let messages = make_conversation_with_tools(turns);
        group.bench_with_input(BenchmarkId::new("turns", turns), &turns, |b, _| {
            b.iter(|| black_box(transform_context(messages.clone(), 200_000, None)));
        });
    }
    group.finish();
}

fn bench_full_pipeline(c: &mut Criterion) {
    let provider = AnthropicProvider::new("sk-test".into());
    let system = make_system_prompt(2000);
    let tools = make_tools(8);

    let mut group = c.benchmark_group("full_pipeline");
    for turns in [1, 5, 20, 50] {
        let messages = make_conversation_with_tools(turns);

        // End-to-end: AgentMessage[] → transform → convert → build body → serialize
        group.bench_with_input(BenchmarkId::new("agent_to_json", turns), &turns, |b, _| {
            b.iter(|| {
                let transformed = transform_context(messages.clone(), 200_000, None);
                let llm = convert_to_llm(&transformed);
                let req = make_completion_request(&system, &llm, &tools);
                let body = provider.build_request_body(&req);
                black_box(serde_json::to_vec(&body).unwrap())
            });
        });
    }
    group.finish();
}

criterion_group! {
    name = benches;
    config = fast();
    targets =
        bench_transform_context,
        bench_full_pipeline,
}
criterion_main!(benches);
