use criterion::{black_box, criterion_group, criterion_main, Criterion};
use std::sync::Arc;
use std::time::Duration;

use nerv::agent::agent::{AgentTool, UpdateCallback};
use nerv::tools::*;

fn fast() -> Criterion {
    Criterion::default()
        .warm_up_time(Duration::from_millis(200))
        .measurement_time(Duration::from_secs(2))
}

fn noop_update() -> UpdateCallback {
    Arc::new(|_| {})
}

fn bench_read_small(c: &mut Criterion) {
    let tmp = tempfile::TempDir::new().unwrap();
    let lines: Vec<String> = (1..=100).map(|i| format!("line {}", i)).collect();
    std::fs::write(tmp.path().join("small.txt"), lines.join("\n")).unwrap();

    let tool = ReadTool::new(tmp.path().to_path_buf());
    let input = serde_json::json!({"path": "small.txt"});

    c.bench_function("read_100_lines", |b| {
        b.iter(|| black_box(tool.execute(input.clone(), noop_update())));
    });
}

fn bench_read_large(c: &mut Criterion) {
    let tmp = tempfile::TempDir::new().unwrap();
    let lines: Vec<String> = (1..=5000).map(|i| format!("line {}", i)).collect();
    std::fs::write(tmp.path().join("large.txt"), lines.join("\n")).unwrap();

    let tool = ReadTool::new(tmp.path().to_path_buf());
    let input = serde_json::json!({"path": "large.txt"});

    c.bench_function("read_5000_lines", |b| {
        b.iter(|| black_box(tool.execute(input.clone(), noop_update())));
    });
}

fn bench_edit_single(c: &mut Criterion) {
    let tmp = tempfile::TempDir::new().unwrap();
    let lines: Vec<String> = (1..=500).map(|i| format!("fn func_{}() {{}}", i)).collect();
    let original = lines.join("\n");

    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mq);

    c.bench_function("edit_single_500_lines", |b| {
        b.iter(|| {
            std::fs::write(tmp.path().join("code.rs"), &original).unwrap();
            let input = serde_json::json!({
                "path": "code.rs",
                "old_text": "fn func_250() {}",
                "new_text": "fn func_250_renamed() {}"
            });
            black_box(tool.execute(input, noop_update()))
        });
    });
}

fn bench_edit_multi(c: &mut Criterion) {
    let tmp = tempfile::TempDir::new().unwrap();
    let lines: Vec<String> = (1..=500).map(|i| format!("fn func_{}() {{}}", i)).collect();
    let original = lines.join("\n");

    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mq);

    c.bench_function("edit_multi_5x_500_lines", |b| {
        b.iter(|| {
            std::fs::write(tmp.path().join("code.rs"), &original).unwrap();
            let input = serde_json::json!({
                "path": "code.rs",
                "edits": [
                    {"old_text": "fn func_50() {}", "new_text": "fn a() {}"},
                    {"old_text": "fn func_150() {}", "new_text": "fn b() {}"},
                    {"old_text": "fn func_250() {}", "new_text": "fn c() {}"},
                    {"old_text": "fn func_350() {}", "new_text": "fn d() {}"},
                    {"old_text": "fn func_450() {}", "new_text": "fn e() {}"},
                ]
            });
            black_box(tool.execute(input, noop_update()))
        });
    });
}

fn bench_write(c: &mut Criterion) {
    let tmp = tempfile::TempDir::new().unwrap();
    let content = "x".repeat(10_000);

    let tool = WriteTool::new(tmp.path().to_path_buf());

    c.bench_function("write_10kb", |b| {
        b.iter(|| {
            let input = serde_json::json!({"path": "out.txt", "content": &content});
            black_box(tool.execute(input, noop_update()))
        });
    });
}

fn bench_diff_small(c: &mut Criterion) {
    let old: String = (1..=100).map(|i| format!("line {}\n", i)).collect();
    let new = old.replace("line 50\n", "CHANGED 50\n");

    c.bench_function("diff_100_lines_1_change", |b| {
        b.iter(|| black_box(nerv::tools::diff::unified_diff(&old, &new, "a", "b")));
    });
}

fn bench_diff_large(c: &mut Criterion) {
    let old: String = (1..=2000).map(|i| format!("line {}\n", i)).collect();
    let new = old
        .replace("line 100\n", "CHANGED 100\n")
        .replace("line 500\n", "CHANGED 500\n")
        .replace("line 1500\n", "CHANGED 1500\n");

    c.bench_function("diff_2000_lines_3_changes", |b| {
        b.iter(|| black_box(nerv::tools::diff::unified_diff(&old, &new, "a", "b")));
    });
}

fn bench_mutation_queue(c: &mut Criterion) {
    let mq = FileMutationQueue::new();
    let path = std::path::PathBuf::from("/tmp/nerv-bench-mq");

    c.bench_function("mutation_queue_uncontended", |b| {
        b.iter(|| {
            mq.with(&path, || black_box(42));
        });
    });
}

criterion_group! {
    name = tools;
    config = fast();
    targets =
        bench_read_small,
        bench_read_large,
        bench_edit_single,
        bench_edit_multi,
        bench_write,
        bench_diff_small,
        bench_diff_large,
        bench_mutation_queue,
}
criterion_main!(tools);
