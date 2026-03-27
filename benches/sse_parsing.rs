/// SSE parsing benchmark — Value-based (old) vs typed-struct (new).
///
/// `value/*` benchmarks mirror the pre-refactor production code.
/// `typed/*` benchmarks mirror the post-refactor production code.
/// Both drive identical fixture data so the numbers are directly comparable.
use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use std::time::Duration;

use serde::Deserialize;

fn fast() -> Criterion {
    Criterion::default()
        .warm_up_time(Duration::from_millis(300))
        .measurement_time(Duration::from_secs(3))
}

// ---------------------------------------------------------------------------
// Typed structs — mirrors production code post-refactor (anthropic.rs)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct SseUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    #[serde(default)]
    cache_read_input_tokens: u32,
    #[serde(default)]
    cache_creation_input_tokens: u32,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum SseContentBlock {
    #[serde(rename = "tool_use")]
    ToolUse { id: String, name: String },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum SseDelta {
    #[serde(rename = "text_delta")]
    Text { text: String },
    #[serde(rename = "thinking_delta")]
    Thinking { thinking: String },
    #[serde(rename = "input_json_delta")]
    InputJson { partial_json: String },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct SseMessageStart {
    message: SseMessageStartInner,
}
#[derive(Deserialize)]
struct SseMessageStartInner {
    usage: SseUsage,
}

#[derive(Deserialize)]
struct SseContentBlockStart {
    index: u32,
    content_block: SseContentBlock,
}

#[derive(Deserialize)]
struct SseContentBlockDelta {
    index: u32,
    delta: SseDelta,
}

#[derive(Deserialize)]
struct SseContentBlockStop {
    index: u32,
}

#[derive(Deserialize)]
struct SseMessageDeltaInner {
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct SseMessageDelta {
    delta: SseMessageDeltaInner,
    usage: SseUsage,
}

// ---------------------------------------------------------------------------
// Typed structs — mirrors production code post-refactor (openai_compat.rs)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct OaiUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
}

#[derive(Deserialize)]
struct OaiFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct OaiToolCall {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<OaiFunction>,
}

#[derive(Deserialize)]
struct OaiDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    tool_calls: Vec<OaiToolCall>,
}

#[derive(Deserialize)]
struct OaiChoice {
    delta: OaiDelta,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct OaiChunk {
    #[serde(default)]
    choices: Vec<OaiChoice>,
    #[serde(default)]
    usage: Option<OaiUsage>,
}

// ---------------------------------------------------------------------------
// Fixture data — (event_type, json_data) pairs so the typed bench can avoid
// the extra type-field peek that the Value bench needs.
// ---------------------------------------------------------------------------

fn anthropic_text_stream(token_count: usize) -> Vec<(String, String)> {
    let mut lines: Vec<(String, String)> = Vec::with_capacity(token_count + 5);
    lines.push(("message_start".into(), r#"{"message":{"usage":{"input_tokens":823,"output_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}"#.into()));
    lines.push(("content_block_start".into(), r#"{"index":0,"content_block":{"type":"text","text":""}}"#.into()));
    let words = ["Hello", " world", "!", " Here", " is", " some", " code", ":\n\n", "```", "rust"];
    for i in 0..token_count {
        let word = words[i % words.len()];
        lines.push(("content_block_delta".into(), format!(
            r#"{{"index":0,"delta":{{"type":"text_delta","text":{}}}}}"#,
            serde_json::to_string(word).unwrap()
        )));
    }
    lines.push(("content_block_stop".into(), r#"{"index":0}"#.into()));
    lines.push(("message_delta".into(), format!(
        r#"{{"delta":{{"stop_reason":"end_turn"}},"usage":{{"output_tokens":{}}}}}"#,
        token_count
    )));
    lines
}

fn anthropic_tool_stream(chunk_count: usize) -> Vec<(String, String)> {
    let mut lines: Vec<(String, String)> = Vec::with_capacity(chunk_count + 5);
    lines.push(("message_start".into(), r#"{"message":{"usage":{"input_tokens":1200,"output_tokens":0,"cache_read_input_tokens":512,"cache_creation_input_tokens":0}}}"#.into()));
    lines.push(("content_block_start".into(), r#"{"index":0,"content_block":{"type":"tool_use","id":"toolu_01XFDUDYJgAACTvnkyfe","name":"read"}}"#.into()));
    let chunks = [r#"{"pa"#, r#"th": "#, r#""src/"#, r#"main"#, r#".rs"}"#];
    for i in 0..chunk_count {
        let chunk = chunks[i % chunks.len()];
        lines.push(("content_block_delta".into(), format!(
            r#"{{"index":0,"delta":{{"type":"input_json_delta","partial_json":{}}}}}"#,
            serde_json::to_string(chunk).unwrap()
        )));
    }
    lines.push(("content_block_stop".into(), r#"{"index":0}"#.into()));
    lines.push(("message_delta".into(), format!(
        r#"{{"delta":{{"stop_reason":"tool_use"}},"usage":{{"output_tokens":{}}}}}"#,
        chunk_count
    )));
    lines
}

fn openai_text_stream(token_count: usize) -> Vec<(String, String)> {
    let mut lines: Vec<(String, String)> = Vec::with_capacity(token_count + 2);
    lines.push(("chunk".into(), r#"{"id":"chatcmpl-abc","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}],"usage":{"prompt_tokens":350,"completion_tokens":0}}"#.into()));
    let words = ["Hello", " world", "!", " Here", " is", " some", " code", ":\n\n", "```", "rust"];
    for i in 0..token_count {
        let word = words[i % words.len()];
        lines.push(("chunk".into(), format!(
            r#"{{"id":"chatcmpl-abc","choices":[{{"index":0,"delta":{{"content":{}}},"finish_reason":null}}]}}"#,
            serde_json::to_string(word).unwrap()
        )));
    }
    lines.push(("chunk".into(), format!(
        r#"{{"id":"chatcmpl-abc","choices":[{{"index":0,"delta":{{}},"finish_reason":"stop"}}],"usage":{{"prompt_tokens":350,"completion_tokens":{}}}}}"#,
        token_count
    )));
    lines
}

// ---------------------------------------------------------------------------
// Value-based parsers (pre-refactor, kept for comparison)
// ---------------------------------------------------------------------------

fn parse_anthropic_value(lines: &[(String, String)]) -> usize {
    let mut events = 0usize;
    let mut tool_ids: std::collections::HashMap<u64, String> = Default::default();

    for (event_type, data) in lines {
        let Ok(json) = serde_json::from_str::<serde_json::Value>(data) else {
            continue;
        };
        match event_type.as_str() {
            "message_start" => {
                let u = &json["message"]["usage"];
                if u["input_tokens"].as_u64().unwrap_or(0) > 0 {
                    events += 1;
                }
            }
            "content_block_start" => {
                let index = json["index"].as_u64().unwrap_or(0);
                let block = &json["content_block"];
                if block["type"].as_str() == Some("tool_use") {
                    let id = block["id"].as_str().unwrap_or("").to_string();
                    tool_ids.insert(index, id);
                    events += 1;
                }
            }
            "content_block_delta" => {
                let delta = &json["delta"];
                match delta["type"].as_str() {
                    Some("text_delta") => {
                        let _ = delta["text"].as_str().unwrap_or("").to_string();
                        events += 1;
                    }
                    Some("input_json_delta") => {
                        let index = json["index"].as_u64().unwrap_or(0);
                        let _id = tool_ids.get(&index).cloned().unwrap_or_default();
                        let _ = delta["partial_json"].as_str().unwrap_or("").to_string();
                        events += 1;
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let index = json["index"].as_u64().unwrap_or(0);
                if tool_ids.remove(&index).is_some() {
                    events += 1;
                }
            }
            "message_delta" => {
                let _stop = json["delta"]["stop_reason"].as_str().unwrap_or("");
                events += 1;
            }
            _ => {}
        }
    }
    events
}

fn parse_openai_value(lines: &[(String, String)]) -> usize {
    let mut events = 0usize;
    for (_, data) in lines {
        let Ok(json) = serde_json::from_str::<serde_json::Value>(data) else {
            continue;
        };
        if let Some(u) = json.get("usage").filter(|u| u.is_object()) {
            if u["prompt_tokens"].as_u64().unwrap_or(0) > 0 {
                events += 1;
            }
        }
        let Some(choices) = json["choices"].as_array() else { continue };
        let Some(choice) = choices.first() else { continue };
        if let Some(text) = choice["delta"]["content"].as_str() {
            if !text.is_empty() {
                let _ = text.to_string();
                events += 1;
            }
        }
        if let Some(reason) = choice["finish_reason"].as_str() {
            let _ = reason;
            events += 1;
        }
    }
    events
}

// ---------------------------------------------------------------------------
// Typed parsers (post-refactor)
// ---------------------------------------------------------------------------

fn parse_anthropic_typed(lines: &[(String, String)]) -> usize {
    let mut events = 0usize;
    let mut tool_ids: std::collections::HashMap<u32, String> = Default::default();

    for (event_type, data) in lines {
        match event_type.as_str() {
            "message_start" => {
                if let Ok(ev) = serde_json::from_str::<SseMessageStart>(data) {
                    if ev.message.usage.input_tokens > 0 {
                        events += 1;
                    }
                }
            }
            "content_block_start" => {
                if let Ok(ev) = serde_json::from_str::<SseContentBlockStart>(data) {
                    if let SseContentBlock::ToolUse { id, .. } = ev.content_block {
                        tool_ids.insert(ev.index, id);
                        events += 1;
                    }
                }
            }
            "content_block_delta" => {
                if let Ok(ev) = serde_json::from_str::<SseContentBlockDelta>(data) {
                    match ev.delta {
                        SseDelta::Text { text } => {
                            let _ = text;
                            events += 1;
                        }
                        SseDelta::InputJson { partial_json } => {
                            let _id = tool_ids.get(&ev.index).cloned();
                            let _ = partial_json;
                            events += 1;
                        }
                        _ => {}
                    }
                }
            }
            "content_block_stop" => {
                if let Ok(ev) = serde_json::from_str::<SseContentBlockStop>(data) {
                    if tool_ids.remove(&ev.index).is_some() {
                        events += 1;
                    }
                }
            }
            "message_delta" => {
                if serde_json::from_str::<SseMessageDelta>(data).is_ok() {
                    events += 1;
                }
            }
            _ => {}
        }
    }
    events
}

fn parse_openai_typed(lines: &[(String, String)]) -> usize {
    let mut events = 0usize;
    for (_, data) in lines {
        let Ok(chunk) = serde_json::from_str::<OaiChunk>(data) else { continue };
        if let Some(u) = chunk.usage.filter(|u| u.prompt_tokens > 0) {
            let _ = u;
            events += 1;
        }
        let Some(choice) = chunk.choices.into_iter().next() else { continue };
        let delta = choice.delta;
        if let Some(text) = delta.content.filter(|t| !t.is_empty()) {
            let _ = text;
            events += 1;
        }
        if let Some(thinking) = delta.reasoning_content.or(delta.reasoning).filter(|t| !t.is_empty()) {
            let _ = thinking;
            events += 1;
        }
        for tc in delta.tool_calls {
            let _id = tc.id;
            if let Some(func) = tc.function {
                let _ = func.name;
                let _ = func.arguments;
            }
        }
        if choice.finish_reason.is_some() {
            events += 1;
        }
    }
    events
}

// ---------------------------------------------------------------------------
// Benchmarks
// ---------------------------------------------------------------------------

fn bench_anthropic_sse(c: &mut Criterion) {
    let mut group = c.benchmark_group("anthropic_sse_parse");
    for tokens in [50usize, 200, 800] {
        let text_lines = anthropic_text_stream(tokens);
        group.bench_with_input(BenchmarkId::new("value/text", tokens), &tokens, |b, _| {
            b.iter(|| black_box(parse_anthropic_value(&text_lines)));
        });
        group.bench_with_input(BenchmarkId::new("typed/text", tokens), &tokens, |b, _| {
            b.iter(|| black_box(parse_anthropic_typed(&text_lines)));
        });

        let tool_lines = anthropic_tool_stream(tokens / 5);
        let n = tokens / 5;
        group.bench_with_input(BenchmarkId::new("value/tool_call", n), &n, |b, _| {
            b.iter(|| black_box(parse_anthropic_value(&tool_lines)));
        });
        group.bench_with_input(BenchmarkId::new("typed/tool_call", n), &n, |b, _| {
            b.iter(|| black_box(parse_anthropic_typed(&tool_lines)));
        });
    }
    group.finish();
}

fn bench_openai_sse(c: &mut Criterion) {
    let mut group = c.benchmark_group("openai_sse_parse");
    for tokens in [50usize, 200, 800] {
        let lines = openai_text_stream(tokens);
        group.bench_with_input(BenchmarkId::new("value/text", tokens), &tokens, |b, _| {
            b.iter(|| black_box(parse_openai_value(&lines)));
        });
        group.bench_with_input(BenchmarkId::new("typed/text", tokens), &tokens, |b, _| {
            b.iter(|| black_box(parse_openai_typed(&lines)));
        });
    }
    group.finish();
}

criterion_group! {
    name = benches;
    config = fast();
    targets = bench_anthropic_sse, bench_openai_sse,
}
criterion_main!(benches);
