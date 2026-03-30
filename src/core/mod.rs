pub mod agent_session;
pub mod auth;
pub mod compaction_controller;
pub mod config;
pub mod local_models;
pub mod model_registry;
pub mod notifications;
pub mod permissions;
pub mod resource_loader;
pub mod retry;
pub mod session_runner;
pub mod skills;
pub mod system_prompt;
pub mod tool_registry;

pub use agent_session::{
    AgentSession, AgentSessionEvent, CompactionReason, SessionCommand,
};
pub use compaction_controller::CompactionController;
pub use config::NervConfig;
pub use model_registry::ModelRegistry;
pub use session_runner::session_task;
pub use tool_registry::{ToolDefinition, ToolRegistry};
