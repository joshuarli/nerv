use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use nerv::agent::agent::AgentTool;
use nerv::agent::provider::{CancelFlag, new_cancel_flag};
use nerv::index::SymbolIndex;
use nerv::index::codemap::{self, CodemapParams, Depth};
use nerv::tools::*;

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

fn noop_cancel() -> CancelFlag {
    new_cancel_flag()
}

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn src_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src").leak()
}

// Real-repo index built once and reused across codemap benches.
fn real_index() -> SymbolIndex {
    let mut idx = SymbolIndex::new();
    idx.force_index_dir(src_dir());
    idx
}

fn bench_read_small(c: &mut Criterion) {
    // src/bootstrap.rs: small real source file
    let tool = ReadTool::new(repo_root().to_path_buf());
    let input = serde_json::json!({"path": "src/bootstrap.rs"});

    c.bench_function("read_bootstrap_rs", |b| {
        b.iter(|| black_box(tool.execute(input.clone(), &noop_cancel())));
    });
}

fn bench_read_large(c: &mut Criterion) {
    // src/main.rs: larger real source file
    let tool = ReadTool::new(repo_root().to_path_buf());
    let input = serde_json::json!({"path": "src/main.rs"});

    c.bench_function("read_main_rs", |b| {
        b.iter(|| black_box(tool.execute(input.clone(), &noop_cancel())));
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
            black_box(tool.execute(input, &noop_cancel()))
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
            black_box(tool.execute(input, &noop_cancel()))
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
            black_box(tool.execute(input, &noop_cancel()))
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

fn bench_codemap_signatures(c: &mut Criterion) {
    let index = real_index();
    let params = CodemapParams { query: "", kind: None, file: None, depth: Depth::Signatures };
    c.bench_function("codemap_signatures_src", |b| {
        b.iter(|| black_box(codemap::codemap(&index, src_dir(), &params)));
    });
}

fn bench_codemap_full(c: &mut Criterion) {
    let index = real_index();
    let params = CodemapParams { query: "", kind: None, file: None, depth: Depth::Full };
    c.bench_function("codemap_full_src", |b| {
        b.iter(|| black_box(codemap::codemap(&index, src_dir(), &params)));
    });
}

fn bench_codemap_single_file(c: &mut Criterion) {
    let index = real_index();
    let file = repo_root().join("src/bootstrap.rs");
    let params = CodemapParams { query: "", kind: None, file: Some(&file), depth: Depth::Full };
    c.bench_function("codemap_full_bootstrap_rs", |b| {
        b.iter(|| black_box(codemap::codemap(&index, src_dir(), &params)));
    });
}

fn bench_codemap_query_filter(c: &mut Criterion) {
    let index = real_index();
    let params = CodemapParams { query: "agent", kind: None, file: None, depth: Depth::Full };
    c.bench_function("codemap_full_query_agent", |b| {
        b.iter(|| black_box(codemap::codemap(&index, src_dir(), &params)));
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
        bench_codemap_signatures,
        bench_codemap_full,
        bench_codemap_single_file,
        bench_codemap_query_filter,
}
criterion_main!(tools);
