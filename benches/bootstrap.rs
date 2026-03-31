//! Benchmarks for startup bootstrap phases.
//!
//! Runs against the real repo (cwd) and real ~/.nerv so results reflect actual
//! startup cost rather than a synthetic tempdir.
//!
//! Run with:
//!   cargo bench --bench bootstrap

use std::path::PathBuf;
use std::time::Duration;

use nerv::tools::SymbolsTool;

use criterion::{Criterion, black_box, criterion_group, criterion_main};

fn real_paths() -> (PathBuf, PathBuf) {
    let cwd = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let nerv_dir = nerv::nerv_dir().to_path_buf();
    (cwd, nerv_dir)
}

fn bench_load_resources(c: &mut Criterion) {
    let (cwd, nerv_dir) = real_paths();
    c.bench_function("load_resources", |b| {
        b.iter(|| {
            nerv::core::resource_loader::load_resources(black_box(&cwd), black_box(&nerv_dir))
        });
    });
}

fn bench_config_chain(c: &mut Criterion) {
    let (_, nerv_dir) = real_paths();
    c.bench_function("config_chain", |b| {
        b.iter(|| {
            let config = nerv::core::config::NervConfig::load(black_box(&nerv_dir));
            let mut auth = nerv::core::auth::AuthStorage::load(&nerv_dir);
            let registry =
                nerv::core::model_registry::ModelRegistry::new(&config, &mut auth, &nerv_dir);
            black_box(registry);
        });
    });
}

fn bench_bootstrap_serial(c: &mut Criterion) {
    let (cwd, nerv_dir) = real_paths();
    c.bench_function("bootstrap_serial", |b| {
        b.iter(|| {
            let config = nerv::core::config::NervConfig::load(&nerv_dir);
            let mut auth = nerv::core::auth::AuthStorage::load(&nerv_dir);
            let registry =
                nerv::core::model_registry::ModelRegistry::new(&config, &mut auth, &nerv_dir);
            let resources = nerv::core::resource_loader::load_resources(&cwd, &nerv_dir);
            black_box((registry, resources));
        });
    });
}

fn bench_symbol_cache_open(c: &mut Criterion) {
    let (cwd, nerv_dir) = real_paths();
    c.bench_function("symbol_cache_open", |b| {
        b.iter(|| black_box(SymbolsTool::new_with_cache(cwd.clone(), &nerv_dir)));
    });
}

fn bench_session_manager_new(c: &mut Criterion) {
    // Use a tempdir so each iteration creates a fresh DB rather than hitting an
    // already-initialised one (which would skip the DDL path).
    let tmp = tempfile::TempDir::new().unwrap();
    c.bench_function("session_manager_new", |b| {
        b.iter(|| black_box(nerv::session::SessionManager::new(black_box(tmp.path()))));
    });
}

fn bench_bootstrap_parallel(c: &mut Criterion) {
    let (cwd, nerv_dir) = real_paths();
    c.bench_function("bootstrap_parallel", |b| {
        b.iter(|| {
            let cwd2 = cwd.clone();
            let nerv2 = nerv_dir.clone();
            let handle = std::thread::spawn(move || {
                nerv::core::resource_loader::load_resources(&cwd2, &nerv2)
            });
            let config = nerv::core::config::NervConfig::load(&nerv_dir);
            let mut auth = nerv::core::auth::AuthStorage::load(&nerv_dir);
            let registry =
                nerv::core::model_registry::ModelRegistry::new(&config, &mut auth, &nerv_dir);
            let resources = handle.join().unwrap();
            black_box((registry, resources));
        });
    });
}

fn slow() -> Criterion {
    Criterion::default()
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(5))
}

criterion_group! {
    name = benches;
    config = slow();
    targets =
        bench_symbol_cache_open,
        bench_session_manager_new,
        bench_load_resources,
        bench_config_chain,
        bench_bootstrap_serial,
        bench_bootstrap_parallel
}
criterion_main!(benches);
