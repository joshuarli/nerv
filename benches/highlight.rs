use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use std::time::Duration;

use nerv::tui::highlight::{HlState, highlight_line, rules_for_lang};


fn pgo_criterion() -> Criterion {
    // For `make pgo-profile`: just hit the hot paths, no statistical rigor needed.
    Criterion::default()
        .warm_up_time(Duration::from_millis(1))
        .measurement_time(Duration::from_millis(10))
        .sample_size(10)
}

fn fast() -> Criterion {
    if std::env::var("PGO_PROFILE").is_ok() { return pgo_criterion(); }
    Criterion::default()
        .warm_up_time(Duration::from_millis(200))
        .measurement_time(Duration::from_secs(2))
}

const RUST_LINES: &[&str] = &[
    "pub fn transform_context(messages: Vec<AgentMessage>, _context_window: u32, stale_cutoff: Option<usize>) -> Vec<AgentMessage> {",
    "    let config = match stale_cutoff {",
    "        Some(c) => ContextConfig { stale_cutoff: c, prune_tools: false },",
    "        None => ContextConfig { stale_cutoff: messages.len().saturating_sub(RECENT_TURNS), prune_tools: false },",
    "    };",
    "    // Call the real transform pipeline",
    "    transform_context_with_config(messages, &config)",
    "}",
    "let mut result: Vec<LlmMessage> = Vec::with_capacity(messages.len());",
    "if let Some(last) = result.last_mut() && should_merge(last, &llm_msg) {",
    "    merge_into(last, llm_msg);",
    "    continue;",
    "}",
    "    /* block comment spanning a keyword like match or fn */",
    r#"    let path = "/home/user/.config/nerv/config.jsonc";"#,
    "    const MAX_TOKENS: u32 = 200_000;",
    r#"    println!("input={} output={} cache_read={}", u.input, u.output, u.cache_read);"#,
];

const PYTHON_LINES: &[&str] = &[
    "def transform_messages(messages: list[dict], stale_cutoff: int | None = None) -> list[dict]:",
    "    \"\"\"Apply context optimisations before sending to the LLM.\"\"\"",
    "    result = []",
    "    for i, msg in enumerate(messages):",
    "        if stale_cutoff is not None and i < stale_cutoff:",
    "            result.append(_truncate(msg, max_chars=200))",
    "        else:",
    "            result.append(msg)",
    "    return result",
    "# This is a comment with some keywords: for while if else",
    "MAX_TOKENS = 200_000  # constant",
    "path = '/home/user/.config/nerv/config.jsonc'",
    "    raise ValueError(f'unknown role: {msg[\"role\"]}')",
];

const GO_LINES: &[&str] = &[
    "func transformContext(messages []AgentMessage, staleCutoff int) []AgentMessage {",
    "    result := make([]AgentMessage, 0, len(messages))",
    "    for i, msg := range messages {",
    "        if i < staleCutoff && msg.Role == \"tool_result\" {",
    "            msg.Content = truncate(msg.Content, 200)",
    "        }",
    "        result = append(result, msg)",
    "    }",
    "    return result",
    "}",
    "// maxTokens is the per-request token budget",
    "const maxTokens = 200_000",
    "    path := \"/home/user/.config/nerv/config.jsonc\"",
];

const TS_LINES: &[&str] = &[
    "export async function transformContext(messages: AgentMessage[], staleCutoff?: number): Promise<AgentMessage[]> {",
    "  const result: AgentMessage[] = [];",
    "  for (let i = 0; i < messages.length; i++) {",
    "    const msg = messages[i];",
    "    if (staleCutoff !== undefined && i < staleCutoff && msg.role === 'tool_result') {",
    "      result.push({ ...msg, content: truncate(msg.content, 200) });",
    "    } else {",
    "      result.push(msg);",
    "    }",
    "  }",
    "  return result;",
    "}",
    "// Maximum tokens per request",
    "const MAX_TOKENS = 200_000;",
    "const path = `/home/user/.config/nerv/${filename}`;",
];

fn bench_single_line(c: &mut Criterion) {
    let rules = rules_for_lang("rust").unwrap();
    // The hottest path: one line arriving per SSE delta during streaming
    c.bench_function("highlight/rust_single_line", |b| {
        b.iter(|| {
            let mut state = HlState::Normal;
            black_box(highlight_line(black_box(RUST_LINES[0]), &mut state, rules))
        });
    });
}

fn bench_full_blocks(c: &mut Criterion) {
    let mut group = c.benchmark_group("highlight/full_block");

    for (lang, lines) in [
        ("rust", RUST_LINES as &[&str]),
        ("python", PYTHON_LINES),
        ("go", GO_LINES),
        ("typescript", TS_LINES),
    ] {
        let rules = rules_for_lang(lang).unwrap();
        group.bench_with_input(BenchmarkId::from_parameter(lang), lang, |b, _| {
            b.iter(|| {
                let mut state = HlState::Normal;
                for &line in lines {
                    black_box(highlight_line(black_box(line), &mut state, rules));
                }
            });
        });
    }

    group.finish();
}

fn bench_large_response(c: &mut Criterion) {
    // Simulates rendering a 200-line code fence — common in real responses
    let rules = rules_for_lang("rust").unwrap();
    let block: Vec<String> = (0..200)
        .map(|i| RUST_LINES[i % RUST_LINES.len()].to_string())
        .collect();

    c.bench_function("highlight/rust_200_lines", |b| {
        b.iter(|| {
            let mut state = HlState::Normal;
            for line in &block {
                black_box(highlight_line(black_box(line), &mut state, rules));
            }
        });
    });
}

criterion_group!(
    name = highlight_benches;
    config = fast();
    targets = bench_single_line, bench_full_blocks, bench_large_response,
);
criterion_main!(highlight_benches);
