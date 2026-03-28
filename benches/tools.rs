use criterion::{black_box, criterion_group, criterion_main, Criterion};
use std::sync::Arc;
use std::time::Duration;

use nerv::agent::agent::{AgentTool, UpdateCallback};
use nerv::agent::provider::{CancelFlag, new_cancel_flag};
use nerv::tools::*;

fn fast() -> Criterion {
    Criterion::default()
        .warm_up_time(Duration::from_millis(200))
        .measurement_time(Duration::from_secs(2))
}

fn noop_update() -> UpdateCallback {
    Arc::new(|_| {})
}

fn noop_cancel() -> CancelFlag {
    new_cancel_flag()
}

fn bench_read_small(c: &mut Criterion) {
    let tmp = tempfile::TempDir::new().unwrap();
    let lines: Vec<String> = (1..=100).map(|i| format!("line {}", i)).collect();
    std::fs::write(tmp.path().join("small.txt"), lines.join("\n")).unwrap();

    let tool = ReadTool::new(tmp.path().to_path_buf());
    let input = serde_json::json!({"path": "small.txt"});

    c.bench_function("read_100_lines", |b| {
        b.iter(|| black_box(tool.execute(input.clone(), noop_update(), &noop_cancel())));
    });
}

fn bench_read_large(c: &mut Criterion) {
    let tmp = tempfile::TempDir::new().unwrap();
    let lines: Vec<String> = (1..=5000).map(|i| format!("line {}", i)).collect();
    std::fs::write(tmp.path().join("large.txt"), lines.join("\n")).unwrap();

    let tool = ReadTool::new(tmp.path().to_path_buf());
    let input = serde_json::json!({"path": "large.txt"});

    c.bench_function("read_5000_lines", |b| {
        b.iter(|| black_box(tool.execute(input.clone(), noop_update(), &noop_cancel())));
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
            black_box(tool.execute(input, noop_update(), &noop_cancel()))
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
            black_box(tool.execute(input, noop_update(), &noop_cancel()))
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
            black_box(tool.execute(input, noop_update(), &noop_cancel()))
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

// --- Codemap benchmarks ---

use nerv::index::codemap::{self, CodemapParams, Depth};
use nerv::index::SymbolIndex;

fn make_rust_project(file_count: usize, fns_per_file: usize, body_lines: usize) -> (tempfile::TempDir, SymbolIndex) {
    let tmp = tempfile::TempDir::new().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();

    for f in 0..file_count {
        let mut source = String::new();
        for i in 0..fns_per_file {
            source.push_str(&format!("fn func_{}_{}() {{\n", f, i));
            for j in 0..body_lines {
                source.push_str(&format!("    let _{} = {};\n", j, j));
            }
            source.push_str("}\n\n");
        }
        std::fs::write(src.join(format!("mod_{}.rs", f)), &source).unwrap();
    }

    let mut index = SymbolIndex::new();
    index.force_index_dir(tmp.path());
    (tmp, index)
}

fn bench_codemap_signatures_small(c: &mut Criterion) {
    // 5 files × 10 fns = 50 symbols, signatures mode
    let (tmp, index) = make_rust_project(5, 10, 5);
    let params = CodemapParams {
        query: "",
        kind: None,
        file: None,
        depth: Depth::Signatures,
    };
    c.bench_function("codemap_signatures_50_syms", |b| {
        b.iter(|| black_box(codemap::codemap(&index, tmp.path(), &params)));
    });
}

fn bench_codemap_full_small(c: &mut Criterion) {
    // 5 files × 10 fns × 5 body lines = 50 symbols, full mode
    let (tmp, index) = make_rust_project(5, 10, 5);
    let params = CodemapParams {
        query: "",
        kind: None,
        file: None,
        depth: Depth::Full,
    };
    c.bench_function("codemap_full_50_syms", |b| {
        b.iter(|| black_box(codemap::codemap(&index, tmp.path(), &params)));
    });
}

fn bench_codemap_full_large(c: &mut Criterion) {
    // 20 files × 30 fns × 10 body lines = 600 symbols, full mode (will hit budget)
    let (tmp, index) = make_rust_project(20, 30, 10);
    let params = CodemapParams {
        query: "",
        kind: None,
        file: None,
        depth: Depth::Full,
    };
    c.bench_function("codemap_full_600_syms", |b| {
        b.iter(|| black_box(codemap::codemap(&index, tmp.path(), &params)));
    });
}

fn bench_codemap_single_file(c: &mut Criterion) {
    // Typical use: codemap on one file with 30 functions
    let (tmp, index) = make_rust_project(10, 30, 8);
    let file = tmp.path().join("src/mod_0.rs");
    let params = CodemapParams {
        query: "",
        kind: None,
        file: Some(&file),
        depth: Depth::Full,
    };
    c.bench_function("codemap_full_single_file_30_fns", |b| {
        b.iter(|| black_box(codemap::codemap(&index, tmp.path(), &params)));
    });
}

fn bench_codemap_query_filter(c: &mut Criterion) {
    // Query filter narrows results
    let (tmp, index) = make_rust_project(10, 30, 8);
    let params = CodemapParams {
        query: "func_0_",
        kind: None,
        file: None,
        depth: Depth::Full,
    };
    c.bench_function("codemap_full_query_filter_30_of_300", |b| {
        b.iter(|| black_box(codemap::codemap(&index, tmp.path(), &params)));
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
        bench_codemap_signatures_small,
        bench_codemap_full_small,
        bench_codemap_full_large,
        bench_codemap_single_file,
        bench_codemap_query_filter,
}
criterion_main!(tools);
