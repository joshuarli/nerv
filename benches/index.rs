use criterion::{Criterion, black_box, criterion_group, criterion_main};
use std::path::{Path, PathBuf};
use std::time::Duration;

use nerv::index::SymbolIndex;


fn pgo_criterion() -> Criterion {
    // For `make pgo-profile`: just hit the hot paths, no statistical rigor needed.
    Criterion::default()
        .warm_up_time(Duration::from_millis(1))
        .measurement_time(Duration::from_millis(10))
        .sample_size(10)
}

fn slow() -> Criterion {
    if std::env::var("PGO_PROFILE").is_ok() { return pgo_criterion(); }
    // Indexing the repo takes longer than a typical microbench
    Criterion::default()
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(5))
        .sample_size(20)
}

fn fast() -> Criterion {
    if std::env::var("PGO_PROFILE").is_ok() { return pgo_criterion(); }
    Criterion::default()
        .warm_up_time(Duration::from_millis(200))
        .measurement_time(Duration::from_secs(2))
}

/// Path to the project src/ directory — used as the indexing target so we get
/// realistic file counts, symbol densities, and parse complexity.
fn src_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src").leak()
}

// --------------------------------------------------------------------------
// Indexing benchmarks
// --------------------------------------------------------------------------

fn bench_force_index_dir(c: &mut Criterion) {
    let src = src_dir();

    // Cold index: no mtime cache — parse every file from scratch.
    // This is the worst-case startup cost.
    c.bench_function("index/cold_full_src", |b| {
        b.iter(|| {
            let mut idx = SymbolIndex::new();
            idx.force_index_dir(black_box(src));
            black_box(idx)
        });
    });
}

fn bench_incremental_reindex(c: &mut Criterion) {
    let src = src_dir();

    // Warm index: files haven't changed, mtime cache should skip re-parsing.
    // Simulates the common case: session already running, second prompt arrives.
    c.bench_function("index/warm_reindex_src", |b| {
        // Build the initial index outside the timed region
        let mut idx = SymbolIndex::new();
        idx.force_index_dir(src);

        b.iter(|| {
            // Re-index with same files — should hit cache for everything
            idx.force_index_dir(black_box(src));
            black_box(&idx);
        });
    });
}

// --------------------------------------------------------------------------
// Query benchmarks (index is already built)
// --------------------------------------------------------------------------

fn bench_symbol_queries(c: &mut Criterion) {
    let src = src_dir();
    let mut idx = SymbolIndex::new();
    idx.force_index_dir(src);

    let mut group = c.benchmark_group("index/query");

    // Exact name lookup — common for `symbols` tool calls
    group.bench_function("exact_transform_context", |b| {
        b.iter(|| {
            black_box(idx.search(black_box("transform_context"), None, None))
        });
    });

    // Prefix/substring match — triggers more scanning
    group.bench_function("substring_agent", |b| {
        b.iter(|| {
            black_box(idx.search(black_box("agent"), None, None))
        });
    });

    // Single-char query — worst case (matches many symbols)
    group.bench_function("substring_single_char", |b| {
        b.iter(|| {
            black_box(idx.search(black_box("e"), None, None))
        });
    });

    // Miss — name that doesn't exist
    group.bench_function("miss_no_match", |b| {
        b.iter(|| {
            black_box(idx.search(black_box("xyzzy_no_such_symbol_8472"), None, None))
        });
    });

    group.finish();
}

// --------------------------------------------------------------------------
// codemap render benchmarks
// --------------------------------------------------------------------------

fn bench_codemap_render(c: &mut Criterion) {
    use nerv::index::codemap::{CodemapParams, Depth, render, search, SearchResult};

    let src = src_dir();
    let mut idx = SymbolIndex::new();
    idx.force_index_dir(src);

    let mut group = c.benchmark_group("codemap/render");

    // Signatures mode: no file I/O needed (just format stored fields)
    group.bench_function("signatures_all_symbols", |b| {
        let params = CodemapParams { query: "", kind: None, file: None, depth: Depth::Signatures };
        let results = match search(&idx, &params) {
            SearchResult::Found(r) => r,
            _ => panic!("expected results"),
        };
        let paths: Vec<&Path> = results.iter().map(|s| s.file.as_path()).collect();
        let sources = idx.sources_for(&paths);
        b.iter(|| {
            black_box(render(black_box(&results), src, &Depth::Signatures, &sources))
        });
    });

    // Full mode, warm cache: sources pre-fetched from index — no disk I/O in render()
    group.bench_function("full_all_symbols_warm", |b| {
        let params = CodemapParams { query: "", kind: None, file: None, depth: Depth::Full };
        let results = match search(&idx, &params) {
            SearchResult::Found(r) => r,
            _ => panic!("expected results"),
        };
        let paths: Vec<&Path> = results.iter().map(|s| s.file.as_path()).collect();
        let sources = idx.sources_for(&paths);
        b.iter(|| {
            black_box(render(black_box(&results), src, &Depth::Full, &sources))
        });
    });

    // Full mode, cold cache: no pre-fetched sources, render reads files via parallel threads
    group.bench_function("full_all_symbols_cold", |b| {
        let params = CodemapParams { query: "", kind: None, file: None, depth: Depth::Full };
        let results = match search(&idx, &params) {
            SearchResult::Found(r) => r,
            _ => panic!("expected results"),
        };
        let empty: std::collections::HashMap<PathBuf, std::sync::Arc<String>> = std::collections::HashMap::new();
        b.iter(|| {
            black_box(render(black_box(&results), src, &Depth::Full, &empty))
        });
    });

    // Full mode, warm cache, targeted query (realistic single-symbol lookup)
    group.bench_function("full_transform_context_warm", |b| {
        let params = CodemapParams { query: "transform_context", kind: None, file: None, depth: Depth::Full };
        let results = match search(&idx, &params) {
            SearchResult::Found(r) => r,
            _ => panic!("no transform_context found"),
        };
        let paths: Vec<&Path> = results.iter().map(|s| s.file.as_path()).collect();
        let sources = idx.sources_for(&paths);
        b.iter(|| {
            black_box(render(black_box(&results), src, &Depth::Full, &sources))
        });
    });

    // Full mode, cold cache, targeted query
    group.bench_function("full_transform_context_cold", |b| {
        let params = CodemapParams { query: "transform_context", kind: None, file: None, depth: Depth::Full };
        let results = match search(&idx, &params) {
            SearchResult::Found(r) => r,
            _ => panic!("no transform_context found"),
        };
        let empty: std::collections::HashMap<PathBuf, std::sync::Arc<String>> = std::collections::HashMap::new();
        b.iter(|| {
            black_box(render(black_box(&results), src, &Depth::Full, &empty))
        });
    });

    group.finish();
}

criterion_group!(
    name = index_slow;
    config = slow();
    targets = bench_force_index_dir,
);
criterion_group!(
    name = index_fast;
    config = fast();
    targets = bench_incremental_reindex, bench_symbol_queries, bench_codemap_render,
);
criterion_main!(index_slow, index_fast);
