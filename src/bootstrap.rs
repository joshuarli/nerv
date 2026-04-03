//! Shared setup for interactive and headless modes.
//! Constructs the agent, tools, session, and model registry from disk config.

use std::path::Path;
use std::sync::Arc;

use crate::agent::agent::Agent;
use crate::core::agent_session::AgentSession;
use crate::core::config::NervConfig;
use crate::core::model_registry::ModelRegistry;
use crate::core::resource_loader::LoadedResources;
use crate::core::tool_registry::ToolRegistry;
use crate::index::SOURCE_EXTENSIONS;
use crate::session::SessionManager;
use crate::tools::{
    CodemapTool, EditTool, EpshTool, FileMutationQueue, FindTool, GrepTool, LsTool, MemoryTool,
    ReadTool, SymbolsTool, WriteTool,
};

/// Everything needed to run the agent, constructed from disk config.
pub struct Bootstrap {
    pub session: AgentSession,
    pub config: NervConfig,
    pub model_registry: Arc<ModelRegistry>,
    pub resources: LoadedResources,
    pub cancel_flag: crate::agent::provider::CancelFlag,
    /// Shared mid-turn injection slot — same Arc as agent.midturn_inject.
    pub midturn_inject: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    /// Warnings from config validation (unknown model ids, etc.).
    pub config_warnings: Vec<String>,
    /// Background symbol index scan — join after the first TUI frame renders
    /// so the UI appears before blocking on the scan completing.
    pub symbols_handle: Option<std::thread::JoinHandle<()>>,
}

pub struct BootstrapOptions {
    /// Include memory tool (disabled in headless/eval mode).
    pub memory: bool,
    /// Enable permission prompts.
    pub permissions: bool,
    /// Talk mode: disable all project context, tools, memory, and symbol index.
    /// Provides a plain conversational assistant experience.
    pub talk_mode: bool,
}

impl Default for BootstrapOptions {
    fn default() -> Self {
        Self { memory: true, permissions: true, talk_mode: false }
    }
}


/// Construct agent + tools + session from disk config.
/// Both interactive and headless modes call this.
pub fn bootstrap(cwd: &Path, nerv_dir: &Path, opts: BootstrapOptions) -> Bootstrap {
    // Config is fast (1.5ms) and needed by the registry thread.
    let config = NervConfig::load(nerv_dir);

    // Registry thread: Keychain subprocess calls are the dominant startup cost
    // (~88ms). Spawn immediately after config so they overlap with everything else.
    let config_for_registry = config.clone();
    let nerv_dir_for_registry = nerv_dir.to_path_buf();
    let registry_handle = std::thread::Builder::new()
        .name("nerv-registry".into())
        .spawn(move || {
            ModelRegistry::new(&config_for_registry, &nerv_dir_for_registry)
        })
        .expect("failed to spawn registry thread");

    // SessionManager opens sessions.db (DDL on first run, ~19ms cold).
    // Spawn alongside the registry so the SQLite open overlaps with Keychain.
    let repo_dir_for_session = crate::repo_data_dir(cwd);
    let session_mgr_handle = std::thread::Builder::new()
        .name("nerv-session-mgr".into())
        .spawn(move || SessionManager::new(&repo_dir_for_session))
        .expect("failed to spawn session-mgr thread");

    // Index + resource threads (skipped in talk mode).
    let (symbols, symbols_handle, resources_handle) = if !opts.talk_mode {
        // SymbolsTool::new performs tree-sitter initialisation. The SQLite cache
        // open is deferred into the index thread via open_cache().
        let symbols_tool = Arc::new(SymbolsTool::new(cwd.to_path_buf()));
        let symbol_index = symbols_tool.index();
        let idx = symbol_index.clone();
        let root = cwd.to_path_buf();
        let repo_root = crate::find_repo_root(cwd);
        let repo_dir = crate::repo_data_dir(cwd);
        let index_handle = std::thread::Builder::new()
            .name("nerv-index".into())
            .stack_size(1024 * 1024)
            .spawn(move || {
                if let Ok(mut index) = idx.write() {
                    if let Some(ref rr) = repo_root {
                        if crate::repo_fingerprint(rr).is_some() {
                            index.open_cache(&repo_dir, rr);
                        }
                    }
                    index.force_index_dir(&root);
                }
            })
            .expect("failed to spawn index thread");

        let cwd_buf = cwd.to_path_buf();
        let nerv_dir_buf = nerv_dir.to_path_buf();
        let resources_handle = std::thread::Builder::new()
            .name("nerv-resources".into())
            .spawn(move || {
                crate::core::resource_loader::load_resources(&cwd_buf, &nerv_dir_buf)
            })
            .expect("failed to spawn resources thread");

        (Some((symbols_tool, symbol_index)), Some(index_handle), Some(resources_handle))
    } else {
        (None, None, None)
    };

    // Join registry — was 88ms blocking on main thread, now overlaps with
    // tree-sitter init + index cache open + resource loading.
    let model_registry = Arc::new(registry_handle.join().expect("nerv-registry thread panicked"));

    // In talk mode we skip all project context, memory, and the symbol index
    // scan — the session is a plain conversational assistant with no tools.
    let resources = match resources_handle {
        Some(handle) => handle.join().expect("resources thread panicked"),
        None => LoadedResources {
            context_files: Vec::new(),
            system_prompt: None,
            append_prompts: Vec::new(),
            memory: None,
            skills: Vec::new(),
        },
    };

    let mutation_queue = Arc::new(FileMutationQueue::new());
    let mut tool_registry = ToolRegistry::new();
    // Share the same provider registry Arc so login/logout updates are immediately
    // reflected in model_registry.available_models() without rebuilding the
    // registry.
    let mut agent = Agent::new(model_registry.provider_registry.clone());

    if let Some((symbols_tool, symbol_index)) = symbols {
        let tools: Vec<Arc<dyn crate::agent::agent::AgentTool>> = {
            let mut t: Vec<Arc<dyn crate::agent::agent::AgentTool>> = vec![
                Arc::new(ReadTool::new(cwd.to_path_buf())),
                Arc::new(EpshTool::new(cwd.to_path_buf())),
                Arc::new(EditTool::new(cwd.to_path_buf(), mutation_queue.clone())),
                Arc::new(WriteTool::new(cwd.to_path_buf())),
                Arc::new(GrepTool::new(cwd.to_path_buf())),
                Arc::new(FindTool::new(cwd.to_path_buf())),
                Arc::new(LsTool::new(cwd.to_path_buf())),
                symbols_tool,
                Arc::new(CodemapTool::new(cwd.to_path_buf(), symbol_index.clone())),
            ];
            if opts.memory {
                t.push(Arc::new(MemoryTool::new(nerv_dir.to_path_buf())));
            }
            t
        };

        for tool in tools {
            tool_registry.register(tool);
        }

        // After file-writing tools, update the symbol index for the affected file.
        // For bash, mark the index dirty so the next symbols call does a full rescan.
        let project_root = cwd.to_path_buf();
        agent.set_post_tool_fn(Some(Arc::new(move |tool_name, args| match tool_name {
            "edit" | "write" => {
                if let Some(path_str) = args.get("path").and_then(|v| v.as_str()) {
                    let path = if path_str.starts_with('/') {
                        std::path::PathBuf::from(path_str)
                    } else {
                        project_root.join(path_str)
                    };
                    if path.extension().is_some_and(|e| SOURCE_EXTENSIONS.contains(&e.to_str().unwrap_or("")))
                        && let Ok(mut index) = symbol_index.write()
                    {
                        index.index_file(&path);
                    }
                }
            }
            "bash" => {
                if let Ok(mut index) = symbol_index.write() {
                    index.mark_dirty();
                }
            }
            _ => {}
        })));
    }

    let cancel_flag = agent.cancel.clone();
    let midturn_inject = agent.midturn_inject.clone();

    let session_manager = session_mgr_handle.join().expect("nerv-session-mgr thread panicked");

    let mut session = AgentSession::new(
        agent,
        session_manager,
        tool_registry,
        model_registry.clone(),
        resources.clone(),
        cwd.to_path_buf(),
        config.clone(),
    );
    session.permissions_enabled = opts.permissions;
    session.talk_mode = opts.talk_mode;

    // Apply default thinking level from config (true = on, false = off).
    if let Some(enabled) = config.default_thinking {
        use crate::agent::types::ThinkingLevel;
        session.agent.set_thinking_level(
            if enabled { ThinkingLevel::On } else { ThinkingLevel::Off });
    }

    // Apply default effort level from config ("low", "medium", "high", "max").
    if let Some(effort) = config.default_effort_level {
        session.agent.set_effort_level(Some(effort));
    }

    // Apply auto_compact setting from config (default: true).
    if let Some(enabled) = config.auto_compact {
        session.compaction.auto_compact = enabled;
    }

    // Validate configured model ids against the known model list.
    let known_ids: Vec<&str> = model_registry.all_models().iter().map(|m| m.id.as_str()).collect();
    let config_warnings = config.validate_model_ids(&known_ids);

    Bootstrap { session, config, model_registry, resources, cancel_flag, midturn_inject, config_warnings, symbols_handle }
}

/// Resolve a model by name (fuzzy match or provider/id).
pub fn resolve_model(registry: &ModelRegistry, name: &str) -> Option<crate::agent::types::Model> {
    if let Some((p, m)) = name.split_once('/') {
        // Try exact provider/id split first, but fall back to find_model with
        // the full string for models whose id contains a slash (e.g. OpenRouter
        // models like "qwen/qwen3.6-plus-preview:free").
        registry.get_model(p, m).or_else(|| registry.find_model(name)).cloned()
    } else {
        registry.find_model(name).cloned()
    }
}
