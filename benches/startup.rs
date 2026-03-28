use criterion::{Criterion, black_box, criterion_group, criterion_main};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use nerv::agent::agent::Agent;
use nerv::agent::provider::ProviderRegistry;
use nerv::core::*;
use nerv::session::SessionManager;
use nerv::tools::*;

fn fast() -> Criterion {
    Criterion::default()
        .warm_up_time(Duration::from_millis(300))
        .measurement_time(Duration::from_secs(2))
}

fn nerv_dir() -> PathBuf {
    nerv::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".nerv")
}

fn bench_config_load(c: &mut Criterion) {
    let dir = nerv_dir();
    c.bench_function("config_load", |b| {
        b.iter(|| black_box(NervConfig::load(&dir)));
    });
}

fn bench_model_registry(c: &mut Criterion) {
    let dir = nerv_dir();
    let config = NervConfig::load(&dir);
    c.bench_function("model_registry_new", |b| {
        b.iter(|| {
            let mut auth = nerv::core::auth::AuthStorage::load(&dir);
            black_box(ModelRegistry::new(&config, &mut auth))
        });
    });
}

fn bench_resource_loader(c: &mut Criterion) {
    let cwd = std::env::current_dir().unwrap();
    let dir = nerv_dir();
    c.bench_function("load_resources", |b| {
        b.iter(|| black_box(nerv::core::resource_loader::load_resources(&cwd, &dir)));
    });
}

fn bench_tool_registry(c: &mut Criterion) {
    let cwd = std::env::current_dir().unwrap();
    let mutation_queue = Arc::new(FileMutationQueue::new());
    c.bench_function("tool_registry_setup", |b| {
        b.iter(|| {
            let mut registry = ToolRegistry::new();
            for tool in [
                Arc::new(ReadTool::new(cwd.clone())) as Arc<dyn nerv::agent::agent::AgentTool>,
                Arc::new(BashTool::new(cwd.clone())),
                Arc::new(EditTool::new(cwd.clone(), mutation_queue.clone())),
                Arc::new(WriteTool::new(cwd.clone())),
                Arc::new(GrepTool::new(cwd.clone())),
                Arc::new(FindTool::new(cwd.clone())),
                Arc::new(LsTool::new(cwd.clone())),
            ] {
                registry.register(ToolDefinition { tool });
            }
            black_box(registry)
        });
    });
}

fn bench_session_manager_new(c: &mut Criterion) {
    let dir = nerv_dir();
    let cwd = std::env::current_dir().unwrap();
    c.bench_function("session_manager_new", |b| {
        b.iter(|| {
            let mut sm = SessionManager::new(&dir);
            sm.new_session(&cwd).ok();
            black_box(sm)
        });
    });
}

fn bench_agent_new(c: &mut Criterion) {
    c.bench_function("agent_new", |b| {
        b.iter(|| {
            let registry = Arc::new(std::sync::RwLock::new(ProviderRegistry::new()));
            black_box(Agent::new(registry))
        });
    });
}

fn bench_system_prompt_build(c: &mut Criterion) {
    let cwd = std::env::current_dir().unwrap();
    let dir = nerv_dir();
    let resources = nerv::core::resource_loader::load_resources(&cwd, &dir);
    let tool_names = vec!["read", "edit", "write", "bash", "grep", "find", "ls"];
    let snippets = Vec::new();
    let guidelines = Vec::new();

    c.bench_function("system_prompt_build", |b| {
        b.iter(|| {
            black_box(nerv::core::system_prompt::build_system_prompt(
                &cwd,
                &resources,
                &tool_names,
                &snippets,
                &guidelines,
            ))
        });
    });
}

fn bench_list_sessions(c: &mut Criterion) {
    let dir = nerv_dir();
    let sm = SessionManager::new(&dir);
    c.bench_function("list_sessions", |b| {
        b.iter(|| black_box(sm.list_sessions()));
    });
}

fn bench_find_repo_root(c: &mut Criterion) {
    let cwd = std::env::current_dir().unwrap();
    c.bench_function("find_repo_root", |b| {
        b.iter(|| black_box(nerv::find_repo_root(&cwd)));
    });
}

/// Full startup sequence minus terminal/TUI (everything up to first render).
fn bench_full_startup(c: &mut Criterion) {
    let cwd = std::env::current_dir().unwrap();
    let dir = nerv_dir();

    c.bench_function("full_startup_no_tui", |b| {
        b.iter(|| {
            let config = NervConfig::load(&dir);
            let mut auth = nerv::core::auth::AuthStorage::load(&dir);
            let model_registry = Arc::new(ModelRegistry::new(&config, &mut auth));
            let resources = nerv::core::resource_loader::load_resources(&cwd, &dir);
            let _skills = resources.skills.clone();

            let mutation_queue = Arc::new(FileMutationQueue::new());
            let mut tool_registry = ToolRegistry::new();
            for tool in [
                Arc::new(ReadTool::new(cwd.clone())) as Arc<dyn nerv::agent::agent::AgentTool>,
                Arc::new(BashTool::new(cwd.clone())),
                Arc::new(EditTool::new(cwd.clone(), mutation_queue.clone())),
                Arc::new(WriteTool::new(cwd.clone())),
                Arc::new(GrepTool::new(cwd.clone())),
                Arc::new(FindTool::new(cwd.clone())),
                Arc::new(LsTool::new(cwd.clone())),
            ] {
                tool_registry.register(ToolDefinition { tool });
            }

            let provider_registry = Arc::new(std::sync::RwLock::new(
                model_registry.provider_registry.clone(),
            ));
            let agent = Agent::new(provider_registry);

            let mut session_manager = SessionManager::new(&dir);
            session_manager.new_session(&cwd).ok();

            let session = AgentSession::new(
                agent,
                session_manager,
                tool_registry,
                model_registry,
                resources,
                cwd.clone(),
            );
            black_box(session)
        });
    });
}

criterion_group!(
    name = startup_benches;
    config = fast();
    targets =
    bench_config_load,
    bench_model_registry,
    bench_resource_loader,
    bench_tool_registry,
    bench_session_manager_new,
    bench_agent_new,
    bench_system_prompt_build,
    bench_list_sessions,
    bench_find_repo_root,
    bench_full_startup,
);

criterion_main!(startup_benches);
