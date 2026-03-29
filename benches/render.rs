use criterion::{Criterion, black_box, criterion_group, criterion_main};
use std::time::Duration;

use nerv::tui::components::markdown::Markdown;
use nerv::tui::components::spacer::Spacer;
use nerv::tui::components::text::Text;
use nerv::tui::terminal::Terminal;
use nerv::tui::tui::{Component, Container, TUI};

/// Null terminal that discards output — for benchmarking rendering only.
struct NullTerminal {
    cols: u16,
    rows: u16,
}

impl NullTerminal {
    fn new(cols: u16, rows: u16) -> Self {
        Self { cols, rows }
    }
}

impl Terminal for NullTerminal {
    fn start(&mut self) {}
    fn stop(&mut self) {}
    fn restart(&mut self) {}
    fn write_bytes(&mut self, _data: &[u8]) {}
    fn dump_scrollback(&mut self, _text: &str) {}
    fn columns(&self) -> u16 {
        self.cols
    }
    fn rows(&self) -> u16 {
        self.rows
    }
    fn hide_cursor(&mut self) {}
    fn show_cursor(&mut self) {}
    fn kitty_protocol_active(&self) -> bool {
        false
    }
}

fn fast() -> Criterion {
    Criterion::default()
        .warm_up_time(Duration::from_millis(300))
        .measurement_time(Duration::from_secs(2))
}

fn build_conversation(n_messages: usize) -> Container {
    let mut messages = Container::new();
    for i in 0..n_messages {
        messages.push(Box::new(Text::new(format!(
            "\x1b[38;5;75m▎\x1b[0m User message number {} with some text content",
            i
        ))));
        messages.push(Box::new(Spacer::new(1)));
        messages.push(Box::new(Markdown::new(format!(
            "Here is the **assistant** response number {}. It contains `code`, *emphasis*, and a paragraph of text that should be long enough to exercise wrapping behavior in the renderer.",
            i
        ))));
        messages.push(Box::new(Spacer::new(1)));
    }
    messages
}

struct BenchLayout {
    spacer: Spacer,
    header: Text,
    spacer2: Spacer,
    messages: Container,
    editor: Text,
    footer: Text,
}

impl Component for BenchLayout {
    fn render(&self, width: u16) -> Vec<String> {
        let mut lines = Vec::new();
        lines.extend(self.spacer.render(width));
        lines.extend(self.header.render(width));
        lines.extend(self.spacer2.render(width));
        lines.extend(self.messages.render(width));
        lines.extend(self.editor.render(width));
        lines.extend(self.footer.render(width));
        lines
    }
}

fn make_layout(n_messages: usize) -> BenchLayout {
    BenchLayout {
        spacer: Spacer::new(1),
        header: Text::new("\x1b[1;38;5;75m nerv \x1b[0m"),
        spacer2: Spacer::new(1),
        messages: build_conversation(n_messages),
        editor: Text::new("> "),
        footer: Text::new("\x1b[38;5;242m~/project (main)  ↑12k ↓3k  claude-sonnet-4-6\x1b[0m"),
    }
}

fn bench_full_render_10_messages(c: &mut Criterion) {
    let layout = make_layout(10);
    c.bench_function("full_render 10msg", |b| {
        b.iter(|| {
            let mut tui = TUI::new(Box::new(NullTerminal::new(120, 40)));
            tui.request_render(true);
            tui.maybe_render(black_box(&layout), 0);
        });
    });
}

fn bench_full_render_50_messages(c: &mut Criterion) {
    let layout = make_layout(50);
    c.bench_function("full_render 50msg", |b| {
        b.iter(|| {
            let mut tui = TUI::new(Box::new(NullTerminal::new(120, 40)));
            tui.request_render(true);
            tui.maybe_render(black_box(&layout), 0);
        });
    });
}

fn bench_diff_render_no_change(c: &mut Criterion) {
    let layout = make_layout(10);
    let mut tui = TUI::new(Box::new(NullTerminal::new(120, 40)));
    tui.request_render(true);
    tui.maybe_render(&layout, 0);

    c.bench_function("diff_render no_change", |b| {
        b.iter(|| {
            tui.request_render(false);
            tui.maybe_render(black_box(&layout), 0);
        });
    });
}

fn bench_diff_render_streaming_append(c: &mut Criterion) {
    // Simulate streaming: each iteration adds a word to the last message
    let mut tui = TUI::new(Box::new(NullTerminal::new(120, 40)));

    let words = [
        "The ", "quick ", "brown ", "fox ", "jumps ", "over ", "the ", "lazy ", "dog. ",
    ];
    let mut text = String::from("Response: ");

    // Initial render
    let mut layout = make_layout(5);
    tui.request_render(true);
    tui.maybe_render(&layout, 0);

    let mut word_idx = 0;
    c.bench_function("diff_render streaming_append", |b| {
        b.iter(|| {
            text.push_str(words[word_idx % words.len()]);
            word_idx += 1;

            // Replace last message component
            layout.messages.pop(); // spacer
            layout.messages.pop(); // old markdown
            layout.messages.push(Box::new(Markdown::new(&text)));
            layout.messages.push(Box::new(Spacer::new(1)));

            tui.request_render(false);
            tui.maybe_render(black_box(&layout), 0);
        });
    });
}

fn bench_component_render_markdown(c: &mut Criterion) {
    let md = Markdown::new(
        "# Heading\n\nSome **bold** and *italic* text with `inline code`.\n\n\
         ```rust\nfn main() {\n    println!(\"hello\");\n}\n```\n\n\
         - Item one\n- Item two\n- Item three\n\nA final paragraph.",
    );
    c.bench_function("component_render markdown", |b| {
        b.iter(|| black_box(md.render(120)));
    });
}

fn bench_component_render_text_wrap(c: &mut Criterion) {
    let long = "A ".repeat(200);
    let text = Text::new(long);
    c.bench_function("component_render text_wrap", |b| {
        b.iter(|| black_box(text.render(80)));
    });
}

criterion_group!(
    name = render_benches;
    config = fast();
    targets =
    bench_full_render_10_messages,
    bench_full_render_50_messages,
    bench_diff_render_no_change,
    bench_diff_render_streaming_append,
    bench_component_render_markdown,
    bench_component_render_text_wrap,
);

criterion_main!(render_benches);
