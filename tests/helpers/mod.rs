//! Shared test helpers — mock provider, mock tools, session setup.

use std::sync::{Arc, RwLock};

use nerv::agent::agent::{Agent, AgentTool, ToolResult, UpdateCallback};
use nerv::agent::provider::*;
use nerv::agent::types::*;
use nerv::core::agent_session::AgentSession;
use nerv::core::model_registry::ModelRegistry;
use nerv::core::resource_loader::LoadedResources;
use nerv::core::tool_registry::{ToolDefinition, ToolRegistry};
use nerv::core::*;
use nerv::errors::ToolError;
use nerv::session::SessionManager;
use tempfile::TempDir;

pub fn noop_update() -> UpdateCallback {
    Arc::new(|_| {})
}

pub fn noop_cancel() -> CancelFlag {
    new_cancel_flag()
}

pub struct MockProvider {
    responses: std::sync::Mutex<Vec<Vec<ProviderEvent>>>,
    /// Captured wire tools from each request (for verifying tool pruning etc.)
    pub captured_tools: std::sync::Mutex<Vec<Vec<WireTool>>>,
}

impl MockProvider {
    pub fn new(responses: Vec<Vec<ProviderEvent>>) -> Self {
        Self {
            responses: std::sync::Mutex::new(responses),
            captured_tools: std::sync::Mutex::new(Vec::new()),
        }
    }
}

impl Provider for MockProvider {
    fn name(&self) -> &str {
        "mock"
    }
    fn stream_completion(
        &self,
        request: &CompletionRequest,
        _cancel: &CancelFlag,
        on_event: &mut dyn FnMut(ProviderEvent),
    ) -> Result<(), nerv::errors::ProviderError> {
        self.captured_tools
            .lock()
            .unwrap()
            .push(request.tools.clone());
        for event in self.responses.lock().unwrap().remove(0) {
            on_event(event);
        }
        Ok(())
    }
}

pub struct EchoTool;

impl AgentTool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "Echoes input"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "text": { "type": "string" } },
            "required": ["text"]
        })
    }
    fn validate(&self, _input: &serde_json::Value) -> Result<(), ToolError> {
        Ok(())
    }
    fn execute(&self, input: serde_json::Value, _update: UpdateCallback, _cancel: &CancelFlag) -> ToolResult {
        ToolResult::ok(format!(
            "echo: {}",
            input["text"].as_str().unwrap_or("(no input)")
        ))
    }
}

pub fn simple_response(text: &str) -> Vec<ProviderEvent> {
    vec![
        ProviderEvent::UsageUpdate(Usage {
            input: 100,
            output: 20,
            ..Default::default()
        }),
        ProviderEvent::TextDelta(text.to_string()),
        ProviderEvent::MessageStop {
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input: 100,
                output: 20,
                ..Default::default()
            },
        },
    ]
}

pub fn tool_call_response(tool_id: &str, tool_name: &str, args: &str) -> Vec<ProviderEvent> {
    vec![
        ProviderEvent::ToolCallStart {
            id: tool_id.to_string(),
            name: tool_name.to_string(),
        },
        ProviderEvent::ToolCallArgsDelta {
            id: tool_id.to_string(),
            delta: args.to_string(),
        },
        ProviderEvent::ToolCallEnd {
            id: tool_id.to_string(),
        },
        ProviderEvent::MessageStop {
            stop_reason: StopReason::ToolUse,
            usage: Usage {
                input: 100,
                output: 30,
                ..Default::default()
            },
        },
    ]
}

pub fn error_response(msg: &str) -> Vec<ProviderEvent> {
    vec![ProviderEvent::MessageStop {
        stop_reason: StopReason::Error {
            message: msg.to_string(),
        },
        usage: Usage::default(),
    }]
}

pub fn thinking_then_text(thinking: &str, text: &str) -> Vec<ProviderEvent> {
    vec![
        ProviderEvent::ThinkingDelta(thinking.to_string()),
        ProviderEvent::TextDelta(text.to_string()),
        ProviderEvent::MessageStop {
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input: 100,
                output: 40,
                ..Default::default()
            },
        },
    ]
}

pub fn chunked_response(chunks: &[&str]) -> Vec<ProviderEvent> {
    let mut events: Vec<ProviderEvent> = chunks
        .iter()
        .map(|c| ProviderEvent::TextDelta(c.to_string()))
        .collect();
    events.push(ProviderEvent::MessageStop {
        stop_reason: StopReason::EndTurn,
        usage: Usage {
            input: 100,
            output: 20,
            ..Default::default()
        },
    });
    events
}

pub fn test_model() -> Model {
    Model {
        id: "test-model".into(),
        name: "Test".into(),
        provider_name: "mock".into(),
        context_window: 100_000,
        max_output_tokens: 4_000,
        reasoning: false,
        supports_adaptive_thinking: false,
        supports_xhigh: false,
        pricing: ModelPricing {
            input: 1.0,
            output: 2.0,
            cache_read: 0.0,
            cache_write: 0.0,
        },
    }
}

pub fn empty_resources() -> LoadedResources {
    LoadedResources {
        context_files: Vec::new(),
        system_prompt: None,
        append_prompts: Vec::new(),
        memory: None,
        skills: Vec::new(),
    }
}

/// Create a mock AgentSession with canned provider responses.
pub fn mock_session(
    responses: Vec<Vec<ProviderEvent>>,
    with_echo_tool: bool,
) -> (TempDir, AgentSession, crossbeam_channel::Sender<AgentSessionEvent>) {
    let tmp = TempDir::new().unwrap();
    let nerv_dir = tmp.path().join(".nerv");
    std::fs::create_dir_all(&nerv_dir).unwrap();

    let provider = Arc::new(MockProvider::new(responses));
    let mut registry = ProviderRegistry::new();
    registry.register("mock", provider);

    let mut agent = Agent::new(Arc::new(RwLock::new(registry)));
    agent.state.model = Some(test_model());

    let model_registry = Arc::new(ModelRegistry::empty());
    let mut tool_registry = ToolRegistry::new();
    if with_echo_tool {
        tool_registry.register(ToolDefinition {
            tool: Arc::new(EchoTool),
        });
    }

    let session_manager = SessionManager::new(&nerv_dir);
    let resources = empty_resources();

    let mut session = AgentSession::new(
        agent,
        session_manager,
        tool_registry,
        model_registry,
        resources,
        tmp.path().to_path_buf(),
    );
    // Prevent mock provider from being consumed by background session-naming calls.
    session.disable_session_naming();

    let (tx, _rx) = crossbeam_channel::unbounded();
    (tmp, session, tx)
}
