use criterion::{Criterion, black_box, criterion_group, criterion_main};
use std::path::Path;
use std::time::Duration;

use nerv::index::SymbolIndex;

fn slow() -> Criterion {
    // Indexing the repo takes longer than a typical microbench
    Criterion::default()
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(5))
        .sample_size(20)
}

fn fast() -> Criterion {
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

criterion_group!(
    name = index_slow;
    config = slow();
    targets = bench_force_index_dir,
);
criterion_group!(
    name = index_fast;
    config = fast();
    targets = bench_incremental_reindex, bench_symbol_queries,
);
criterion_main!(index_slow, index_fast);
