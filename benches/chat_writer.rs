use criterion::{Criterion, black_box, criterion_group, criterion_main};
use std::time::Duration;

use nerv::interactive::chat_writer::ChatWriter;
use nerv::tui::tui::Component;

fn fast() -> Criterion {
    Criterion::default()
        .warm_up_time(Duration::from_millis(200))
        .measurement_time(Duration::from_secs(2))
}

// --- sample content -------------------------------------------------------

const PROSE_RESPONSE: &str = "\
The `transform_context` function applies twelve optimisations in a single pass \
over the message history. The most impactful are **stale tool-result truncation** \
and **assistant-text deduplication**, which together cut context size by 30–60% on \
long sessions.\n\
\n\
Key design decisions:\n\
- Optimisations run in priority order so cheaper checks short-circuit early.\n\
- The function is pure (takes ownership, returns a new `Vec`) so it's trivially testable.\n\
- `ContextConfig` is computed once per turn in `AgentSession` and passed down.\n";

const CODE_RESPONSE: &str = "\
Here's the relevant excerpt from `transform.rs`:\n\
\n\
```rust\n\
pub fn transform_context(\n\
    messages: Vec<AgentMessage>,\n\
    _context_window: u32,\n\
    stale_cutoff: Option<usize>,\n\
) -> Vec<AgentMessage> {\n\
    let config = ContextConfig {\n\
        stale_cutoff: stale_cutoff\n\
            .unwrap_or_else(|| messages.len().saturating_sub(RECENT_TURNS)),\n\
        prune_tools: false,\n\
    };\n\
    transform_context_with_config(messages, &config)\n\
}\n\
```\n\
\n\
The `stale_cutoff` defaults to `len - RECENT_TURNS` so recent messages are always \
kept verbatim. Older tool results are truncated to 200 chars.\n";

const TOOL_RESULT: &str = "\
src/agent/transform.rs\n\
src/agent/convert.rs\n\
src/agent/types.rs\n\
src/agent/provider.rs\n\
src/agent/anthropic.rs\n\
src/agent/openai_compat.rs\n\
src/agent/agent.rs\n\
src/core/agent_session.rs\n\
src/core/config.rs\n\
src/core/permissions.rs\n\
src/core/model_registry.rs\n\
src/core/resource_loader.rs\n\
src/core/system_prompt.rs\n\
src/core/tool_registry.rs\n\
src/tools/read.rs\n\
src/tools/edit.rs\n\
src/tools/bash.rs\n\
src/tools/grep.rs\n\
src/tools/find.rs\n\
src/tools/ls.rs\n\
src/tools/symbols.rs\n\
src/tools/codemap.rs\n\
src/tools/memory.rs\n\
src/tools/diff.rs\n\
src/tools/truncate.rs\n";

// --------------------------------------------------------------------------

/// Build a ChatWriter that has already accumulated N turns of history.
/// This is the steady state during a long session.
fn preloaded_writer(turns: usize) -> ChatWriter {
    let mut w = ChatWriter::new();
    for i in 0..turns {
        w.push_user(&format!("User message {i}: what does transform_context do?"));
        // assistant text response
        w.begin_stream();
        for chunk in PROSE_RESPONSE.split_whitespace().collect::<Vec<_>>().chunks(8) {
            w.append_text(&format!("{} ", chunk.join(" ")));
        }
        w.finish_stream(PROSE_RESPONSE, None);
        // tool call + result
        let args = serde_json::json!({"path": "src/agent/transform.rs"});
        w.push_tool_call("read", &args);
        w.push_tool_result(TOOL_RESULT, false);
    }
    w
}

// --------------------------------------------------------------------------

fn bench_push_user(c: &mut Criterion) {
    c.bench_function("chat_writer/push_user", |b| {
        let mut w = ChatWriter::new();
        b.iter(|| {
            w.push_user(black_box("What does transform_context do?"));
        });
    });
}

fn bench_streaming(c: &mut Criterion) {
    let mut group = c.benchmark_group("chat_writer/streaming");

    // append_text: called on every SSE delta — must be very cheap
    group.bench_function("append_text_delta", |b| {
        let mut w = ChatWriter::new();
        w.begin_stream();
        b.iter(|| {
            w.append_text(black_box("the "));
        });
    });

    // finish_stream with a prose response — commits the markdown block
    group.bench_function("finish_stream_prose", |b| {
        b.iter(|| {
            let mut w = ChatWriter::new();
            w.begin_stream();
            w.append_text(black_box(PROSE_RESPONSE));
            w.finish_stream(black_box(PROSE_RESPONSE), None);
        });
    });

    // finish_stream with a code-heavy response
    group.bench_function("finish_stream_code", |b| {
        b.iter(|| {
            let mut w = ChatWriter::new();
            w.begin_stream();
            w.append_text(black_box(CODE_RESPONSE));
            w.finish_stream(black_box(CODE_RESPONSE), None);
        });
    });

    // finish_stream with extended thinking
    group.bench_function("finish_stream_thinking", |b| {
        let thinking = "Let me think about this carefully. \
            The transform_context function has twelve optimisations. \
            I should explain the most important ones first."
            .repeat(5);
        b.iter(|| {
            let mut w = ChatWriter::new();
            w.begin_stream();
            w.append_thinking(black_box(&thinking));
            w.append_text(black_box(PROSE_RESPONSE));
            w.finish_stream(black_box(PROSE_RESPONSE), Some(&thinking));
        });
    });

    group.finish();
}

fn bench_push_tool(c: &mut Criterion) {
    let args = serde_json::json!({
        "path": "src/agent/transform.rs",
        "pattern": "transform_context",
        "glob": "*.rs"
    });

    let mut group = c.benchmark_group("chat_writer/tool");

    group.bench_function("push_tool_call", |b| {
        let mut w = ChatWriter::new();
        b.iter(|| {
            w.push_tool_call(black_box("grep"), black_box(&args));
        });
    });

    group.bench_function("push_tool_result_short", |b| {
        let mut w = ChatWriter::new();
        b.iter(|| {
            w.push_tool_result(black_box("src/agent/transform.rs:114: pub fn transform_context("), false);
        });
    });

    group.bench_function("push_tool_result_long", |b| {
        let mut w = ChatWriter::new();
        b.iter(|| {
            w.push_tool_result(black_box(TOOL_RESULT), false);
        });
    });

    group.bench_function("push_tool_result_error", |b| {
        let mut w = ChatWriter::new();
        b.iter(|| {
            w.push_tool_result(black_box("error: file not found: src/agent/bogus.rs"), true);
        });
    });

    group.finish();
}

fn bench_render(c: &mut Criterion) {
    let mut group = c.benchmark_group("chat_writer/render");

    // Cold render: no cache yet — exercises all block rendering paths
    group.bench_function("cold_10_turns", |b| {
        b.iter(|| {
            let mut w = preloaded_writer(10);
            // render is &self so we need to call it; clear cache by rebuilding
            black_box(w.render(black_box(120)));
        });
    });

    // Warm render: cache is hot, only the streaming tail needs work
    group.bench_function("warm_10_turns", |b| {
        let mut w = preloaded_writer(10);
        // Prime the cache
        let _ = w.render(120);
        b.iter(|| {
            black_box(w.render(black_box(120)));
        });
    });

    // Width-change invalidates the cache — forces full re-render
    group.bench_function("width_change_10_turns", |b| {
        let mut w = preloaded_writer(10);
        let _ = w.render(120);
        let mut wide = true;
        b.iter(|| {
            let width = if wide { 120 } else { 80 };
            wide = !wide;
            black_box(w.render(black_box(width)));
        });
    });

    // Render with an active stream (the most common case during generation)
    group.bench_function("render_with_active_stream", |b| {
        let mut w = preloaded_writer(5);
        w.begin_stream();
        // Partial response so far
        w.append_text(&PROSE_RESPONSE[..PROSE_RESPONSE.len() / 2]);
        b.iter(|| {
            black_box(w.render(black_box(120)));
        });
    });

    // Larger history: 30 turns
    group.bench_function("warm_30_turns", |b| {
        let mut w = preloaded_writer(30);
        let _ = w.render(120);
        b.iter(|| {
            black_box(w.render(black_box(120)));
        });
    });

    group.finish();
}

criterion_group!(
    name = chat_writer_benches;
    config = fast();
    targets =
        bench_push_user,
        bench_streaming,
        bench_push_tool,
        bench_render,
);
criterion_main!(chat_writer_benches);
