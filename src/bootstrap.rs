//! Shared setup for interactive and headless modes.
//! Constructs the agent, tools, session, and model registry from disk config.

use std::path::Path;
use std::sync::Arc;

use crate::agent::agent::Agent;
use crate::core::config::NervConfig;
use crate::core::model_registry::ModelRegistry;
use crate::core::resource_loader::LoadedResources;
use crate::core::tool_registry::{ToolDefinition, ToolRegistry};
use crate::core::agent_session::AgentSession;
use crate::session::SessionManager;
use crate::tools::*;

/// Everything needed to run the agent, constructed from disk config.
pub struct Bootstrap {
    pub session: AgentSession,
    pub config: NervConfig,
    pub model_registry: Arc<ModelRegistry>,
    pub resources: LoadedResources,
    pub cancel_flag: crate::agent::provider::CancelFlag,
    /// Warnings from config validation (unknown model ids, etc.).
    pub config_warnings: Vec<String>,
}

pub struct BootstrapOptions {
    /// Include memory tool (disabled in headless/eval mode).
    pub memory: bool,
    /// Enable permission prompts.
    pub permissions: bool,
}

impl Default for BootstrapOptions {
    fn default() -> Self {
        Self {
            memory: true,
            permissions: true,
        }
    }
}

/// Construct agent + tools + session from disk config.
/// Both interactive and headless modes call this.
pub fn bootstrap(cwd: &Path, nerv_dir: &Path, opts: BootstrapOptions) -> Bootstrap {
    // Symbol index scan is the most expensive startup cost — kick it off
    // immediately so it runs in parallel with config/auth/resource loading.
    // The mutex serves as the join: if `symbols` is called before this
    // finishes, it blocks on lock() until the scan completes.
    let symbols_tool = Arc::new(SymbolsTool::new(cwd.to_path_buf()));
    let symbol_index = symbols_tool.index();
    {
        let idx = symbol_index.clone();
        let root = cwd.to_path_buf();
        std::thread::spawn(move || {
            if let Ok(mut index) = idx.lock() {
                index.force_index_dir(&root);
            }
        });
    }

    let config = NervConfig::load(nerv_dir);
    let mut auth = crate::core::auth::AuthStorage::load(nerv_dir);
    let model_registry = Arc::new(ModelRegistry::new(&config, &mut auth));
    let resources = crate::core::resource_loader::load_resources(cwd, nerv_dir);

    let mutation_queue = Arc::new(FileMutationQueue::new());
    let mut tool_registry = ToolRegistry::new();

    let tools: Vec<Arc<dyn crate::agent::agent::AgentTool>> = {
        let mut t: Vec<Arc<dyn crate::agent::agent::AgentTool>> = vec![
            Arc::new(ReadTool::new(cwd.to_path_buf())),
            Arc::new(BashTool::new(cwd.to_path_buf())),
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
        tool_registry.register(ToolDefinition { tool });
    }

    let provider_registry = Arc::new(std::sync::RwLock::new(
        model_registry.provider_registry.clone(),
    ));
    let mut agent = Agent::new(provider_registry);

    // After file-writing tools, update the symbol index for the affected file.
    // For bash, mark the index dirty so the next symbols call does a full rescan.
    {
        let idx = symbol_index.clone();
        let project_root = cwd.to_path_buf();
        agent.state.post_tool_fn = Some(Arc::new(move |tool_name, args| {
            match tool_name {
                "edit" | "write" => {
                    if let Some(path_str) = args.get("path").and_then(|v| v.as_str()) {
                        let path = if path_str.starts_with('/') {
                            std::path::PathBuf::from(path_str)
                        } else {
                            project_root.join(path_str)
                        };
                        if path.extension().is_some_and(|e| e == "rs") {
                            if let Ok(mut index) = idx.lock() {
                                index.index_file(&path);
                            }
                        }
                    }
                }
                "bash" => {
                    if let Ok(mut index) = idx.lock() {
                        index.mark_dirty();
                    }
                }
                _ => {}
            }
        }));
    }
    let cancel_flag = agent.cancel.clone();

    let session_manager = SessionManager::new(nerv_dir);

    let mut session = AgentSession::new(
        agent,
        session_manager,
        tool_registry,
        model_registry.clone(),
        resources.clone(),
        cwd.to_path_buf(),
    );
    session.permissions_enabled = opts.permissions;

    // Apply default thinking level from config (true = on, false = off).
    if let Some(enabled) = config.default_thinking_level {
        use crate::agent::types::ThinkingLevel;
        session.agent.state.thinking_level = if enabled { ThinkingLevel::On } else { ThinkingLevel::Off };
    }

    // Apply default effort level from config ("low", "medium", "high", "max").
    if let Some(effort) = config.default_effort_level {
        session.agent.state.effort_level = Some(effort);
    }

    // Validate configured model ids against the known model list.
    let known_ids: Vec<&str> = model_registry.all_models().iter().map(|m| m.id.as_str()).collect();
    let config_warnings = config.validate_model_ids(&known_ids);

    Bootstrap {
        session,
        config,
        model_registry,
        resources,
        cancel_flag,
        config_warnings,
    }
}

/// Resolve a model by name (fuzzy match or provider/id).
pub fn resolve_model(registry: &ModelRegistry, name: &str) -> Option<crate::agent::types::Model> {
    if let Some((p, m)) = name.split_once('/') {
        registry.get_model(p, m).cloned()
    } else {
        registry.find_model(name).cloned()
    }
}
