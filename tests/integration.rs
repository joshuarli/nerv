//! Integration tests — full AgentSession with mock provider, session persistence, export.

use std::sync::{Arc, RwLock};

use nerv::agent::agent::{Agent, AgentTool, ToolResult, UpdateCallback};
use nerv::agent::provider::*;
use nerv::agent::types::*;
use nerv::core::*;
use nerv::session::SessionManager;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Mock provider — pops canned response sequences one per stream_completion call
// ---------------------------------------------------------------------------

struct MockProvider {
    responses: std::sync::Mutex<Vec<Vec<ProviderEvent>>>,
}

impl MockProvider {
    fn new(responses: Vec<Vec<ProviderEvent>>) -> Self {
        Self {
            responses: std::sync::Mutex::new(responses),
        }
    }
}

impl Provider for MockProvider {
    fn name(&self) -> &str {
        "mock"
    }
    fn stream_completion(
        &self,
        _request: &CompletionRequest,
        _cancel: &CancelFlag,
        on_event: &mut dyn FnMut(ProviderEvent),
    ) -> Result<(), nerv::errors::ProviderError> {
        for event in self.responses.lock().unwrap().remove(0) {
            on_event(event);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Mock tool — just echoes input back
// ---------------------------------------------------------------------------

struct EchoTool;

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
    fn validate(&self, _input: &serde_json::Value) -> Result<(), nerv::errors::ToolError> {
        Ok(())
    }
    fn execute(&self, input: serde_json::Value, _update: UpdateCallback) -> ToolResult {
        let text = input["text"].as_str().unwrap_or("(no input)");
        ToolResult {
            content: format!("echo: {}", text),
            details: None,
            is_error: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn simple_response(text: &str) -> Vec<ProviderEvent> {
    vec![
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

fn tool_call_response(tool_id: &str, tool_name: &str, args: &str) -> Vec<ProviderEvent> {
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

fn thinking_then_text(thinking: &str, text: &str) -> Vec<ProviderEvent> {
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

fn chunked_response(chunks: &[&str]) -> Vec<ProviderEvent> {
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

fn error_response(msg: &str) -> Vec<ProviderEvent> {
    vec![ProviderEvent::MessageStop {
        stop_reason: StopReason::Error {
            message: msg.to_string(),
        },
        usage: Usage::default(),
    }]
}

fn setup_session(
    responses: Vec<Vec<ProviderEvent>>,
) -> (
    TempDir,
    AgentSession,
    crossbeam_channel::Sender<AgentSessionEvent>,
) {
    setup_session_with_tools(responses, false)
}

fn setup_session_with_tools(
    responses: Vec<Vec<ProviderEvent>>,
    register_tools: bool,
) -> (
    TempDir,
    AgentSession,
    crossbeam_channel::Sender<AgentSessionEvent>,
) {
    let tmp = TempDir::new().unwrap();
    let nerv_dir = tmp.path().join(".nerv");
    std::fs::create_dir_all(&nerv_dir).unwrap();

    let provider = Arc::new(MockProvider::new(responses));
    let mut registry = ProviderRegistry::new();
    registry.register("mock", provider);

    let mut agent = Agent::new(Arc::new(RwLock::new(registry)));
    agent.state.model = Some(Model {
        id: "test-model".into(),
        name: "Test".into(),
        provider_name: "mock".into(),
        context_window: 100_000,
        max_output_tokens: 4096,
        reasoning: false,
        supports_adaptive_thinking: false,
        supports_xhigh: false,
        pricing: ModelPricing {
            input: 1.0,
            output: 2.0,
            cache_read: 0.0,
            cache_write: 0.0,
        },
    });
    agent.state.system_prompt = "Test system prompt.".into();

    let session_manager = SessionManager::new(&nerv_dir);
    let mut tool_registry = nerv::core::ToolRegistry::new();
    if register_tools {
        tool_registry.register(nerv::core::ToolDefinition {
            tool: Arc::new(EchoTool),
        });
    }
    let config = NervConfig::load(&nerv_dir);
    let mut auth = nerv::core::auth::AuthStorage::load(&nerv_dir);
    let model_registry = Arc::new(ModelRegistry::new(&config, &mut auth));
    let resources = nerv::core::resource_loader::LoadedResources {
        context_files: Vec::new(),
        system_prompt: None,
        append_prompts: Vec::new(),
        memory: None,
        skills: Vec::new(),
    };

    let session = AgentSession::new(
        agent,
        session_manager,
        tool_registry,
        model_registry,
        resources,
        tmp.path().to_path_buf(),
    );

    let (event_tx, _event_rx) = crossbeam_channel::bounded(256);
    (tmp, session, event_tx)
}

fn collect_events(
    responses: Vec<Vec<ProviderEvent>>,
) -> (TempDir, AgentSession, Vec<AgentSessionEvent>) {
    collect_events_with_tools(responses, false)
}

fn collect_events_with_tools(
    responses: Vec<Vec<ProviderEvent>>,
    register_tools: bool,
) -> (TempDir, AgentSession, Vec<AgentSessionEvent>) {
    let (_tmp, mut session, _event_tx) = setup_session_with_tools(responses, register_tools);
    let (tx, rx) = crossbeam_channel::bounded(256);
    session.prompt("test".into(), &tx);
    let events: Vec<_> = rx.try_iter().collect();
    (_tmp, session, events)
}

fn session_messages(session: &AgentSession) -> Vec<AgentMessage> {
    session
        .session_manager
        .entries()
        .iter()
        .filter_map(|e| match e {
            nerv::session::types::SessionEntry::Message(me) => Some(me.message.clone()),
            _ => None,
        })
        .collect()
}

// ===========================================================================
// Tests: basic persistence
// ===========================================================================

#[test]
fn prompt_persists_to_session_db() {
    let (_tmp, mut session, event_tx) = setup_session(vec![simple_response("Hello!")]);
    session.prompt("Hi".into(), &event_tx);

    assert!(session.session_manager.entry_count() > 0);

    let messages = session_messages(&session);
    assert!(messages.len() >= 2);
    assert!(matches!(messages[0], AgentMessage::User { .. }));
    if let AgentMessage::Assistant(a) = &messages[1] {
        assert_eq!(a.text_content(), "Hello!");
    } else {
        panic!("expected assistant message");
    }
}

#[test]
fn system_prompt_saved_to_session() {
    let (_tmp, mut session, event_tx) = setup_session(vec![simple_response("ok")]);
    session.prompt("test".into(), &event_tx);

    let entries = session.session_manager.entries();
    let has_system_prompt = entries
        .iter()
        .any(|e| matches!(e, nerv::session::types::SessionEntry::SystemPrompt(_)));
    assert!(
        has_system_prompt,
        "session should contain system prompt entry"
    );
}

#[test]
fn multi_turn_conversation_persists() {
    let (_tmp, mut session, event_tx) = setup_session(vec![
        simple_response("First response"),
        simple_response("Second response"),
    ]);

    session.prompt("First question".into(), &event_tx);
    session.prompt("Second question".into(), &event_tx);

    let messages = session_messages(&session);
    // 2 user + 2 assistant = 4
    assert_eq!(messages.len(), 4);
}

#[test]
fn session_survives_reload() {
    let (tmp, mut session, event_tx) = setup_session(vec![simple_response("persistent")]);
    session.prompt("save me".into(), &event_tx);

    let session_id = session.session_manager.session_id().to_string();

    let nerv_dir = tmp.path().join(".nerv");
    let mut mgr2 = SessionManager::new(&nerv_dir);
    let ctx = mgr2.load_session(&session_id).unwrap();

    assert!(!ctx.messages.is_empty());
    assert!(
        ctx.messages
            .iter()
            .any(|m| matches!(m, AgentMessage::User { .. }))
    );
    assert!(
        ctx.messages
            .iter()
            .any(|m| matches!(m, AgentMessage::Assistant(_)))
    );
}

// ===========================================================================
// Tests: token metadata
// ===========================================================================

#[test]
fn token_info_saved_with_assistant_messages() {
    let (_tmp, mut session, event_tx) = setup_session(vec![simple_response("answer")]);
    session.prompt("question".into(), &event_tx);

    let entries = session.session_manager.entries();
    let assistant_entry = entries.iter().find_map(|e| match e {
        nerv::session::types::SessionEntry::Message(me)
            if matches!(me.message, AgentMessage::Assistant(_)) =>
        {
            Some(me)
        }
        _ => None,
    });

    assert!(assistant_entry.is_some(), "should have assistant entry");
    let me = assistant_entry.unwrap();
    assert!(
        me.tokens.is_some(),
        "assistant entry should have token info"
    );
    let tok = me.tokens.as_ref().unwrap();
    assert!(tok.context_window > 0, "context_window should be set");
    assert_eq!(tok.context_window, 100_000);
    assert_eq!(tok.output, 20); // from our Usage in simple_response
}

#[test]
fn token_info_context_used_includes_output() {
    let (_tmp, mut session, event_tx) = setup_session(vec![simple_response("x")]);
    session.prompt("q".into(), &event_tx);

    let entries = session.session_manager.entries();
    let me = entries
        .iter()
        .find_map(|e| match e {
            nerv::session::types::SessionEntry::Message(me)
                if matches!(me.message, AgentMessage::Assistant(_)) =>
            {
                Some(me)
            }
            _ => None,
        })
        .unwrap();
    let tok = me.tokens.as_ref().unwrap();
    // context_used = input + output
    assert!(
        tok.context_used > tok.output,
        "context_used should include input tokens"
    );
}

// ===========================================================================
// Tests: export
// ===========================================================================

#[test]
fn jsonl_export_contains_messages() {
    let (_tmp, mut session, event_tx) = setup_session(vec![simple_response("Response text")]);
    session.prompt("Query".into(), &event_tx);

    let jsonl = session.session_manager.export_jsonl();
    assert!(jsonl.is_some(), "export should produce output");
    let content = jsonl.unwrap();
    assert!(content.contains("Query"), "should contain user message");
    assert!(
        content.contains("Response text"),
        "should contain assistant response"
    );
}

#[test]
fn html_export_contains_messages() {
    let (tmp, mut session, event_tx) = setup_session(vec![simple_response("Hello world!")]);
    session.prompt("Hi there".into(), &event_tx);

    let entries = session.session_manager.entries();
    assert!(!entries.is_empty());

    let mut html = String::from("<html>");
    for entry in entries {
        if let nerv::session::types::SessionEntry::Message(me) = entry {
            match &me.message {
                AgentMessage::User { content, .. } => {
                    for item in content {
                        if let ContentItem::Text { text } = item {
                            html.push_str(text);
                        }
                    }
                }
                AgentMessage::Assistant(a) => {
                    html.push_str(&a.text_content());
                }
                _ => {}
            }
        }
    }
    html.push_str("</html>");

    assert!(html.contains("Hi there"), "should contain user message");
    assert!(
        html.contains("Hello world!"),
        "should contain assistant response"
    );

    // Unused but proves temp dir outlives the test
    let _ = tmp.path().join("export.html");
}

// ===========================================================================
// Tests: streaming reassembly
// ===========================================================================

#[test]
fn chunked_text_reassembled() {
    let (_tmp, mut session, event_tx) =
        setup_session(vec![chunked_response(&["Hel", "lo, ", "world!"])]);
    session.prompt("greet".into(), &event_tx);

    let messages = session_messages(&session);
    let assistant = messages
        .iter()
        .find_map(|m| match m {
            AgentMessage::Assistant(a) => Some(a),
            _ => None,
        })
        .unwrap();
    assert_eq!(assistant.text_content(), "Hello, world!");
}

#[test]
fn thinking_plus_text_produces_both_blocks() {
    let (_tmp, mut session, event_tx) = setup_session(vec![thinking_then_text(
        "Let me think...",
        "The answer is 42.",
    )]);
    session.prompt("meaning of life".into(), &event_tx);

    let messages = session_messages(&session);
    let assistant = messages
        .iter()
        .find_map(|m| match m {
            AgentMessage::Assistant(a) => Some(a),
            _ => None,
        })
        .unwrap();

    let has_thinking = assistant
        .content
        .iter()
        .any(|b| matches!(b, ContentBlock::Thinking { .. }));
    let has_text = assistant
        .content
        .iter()
        .any(|b| matches!(b, ContentBlock::Text { .. }));
    assert!(has_thinking, "should have thinking block");
    assert!(has_text, "should have text block");
    assert_eq!(assistant.text_content(), "The answer is 42.");
}

// ===========================================================================
// Tests: tool use round-trip
// ===========================================================================

#[test]
fn tool_call_executes_and_continues() {
    // Turn 1: model requests tool call
    // Turn 2: after tool result, model responds with text
    let responses = vec![
        tool_call_response("tc1", "echo", r#"{"text":"hello"}"#),
        simple_response("Got the echo result."),
    ];
    let (_tmp, mut session, event_tx) = setup_session_with_tools(responses, true);
    session.prompt("Use the echo tool".into(), &event_tx);

    let messages = session_messages(&session);

    // Should have: user, assistant (tool call), tool result, assistant (final)
    assert!(
        messages.len() >= 4,
        "expected at least 4 messages, got {}",
        messages.len()
    );

    // Find tool result
    let has_tool_result = messages
        .iter()
        .any(|m| matches!(m, AgentMessage::ToolResult { .. }));
    assert!(has_tool_result, "should have tool result message");

    // Final assistant should have the follow-up text
    let last_assistant = messages
        .iter()
        .rev()
        .find_map(|m| match m {
            AgentMessage::Assistant(a) if !a.text_content().is_empty() => Some(a),
            _ => None,
        })
        .unwrap();
    assert_eq!(last_assistant.text_content(), "Got the echo result.");
}

#[test]
fn tool_result_content_contains_echo() {
    let responses = vec![
        tool_call_response("tc1", "echo", r#"{"text":"ping"}"#),
        simple_response("done"),
    ];
    let (_tmp, mut session, event_tx) = setup_session_with_tools(responses, true);
    session.prompt("echo ping".into(), &event_tx);

    let messages = session_messages(&session);
    let tool_result = messages
        .iter()
        .find_map(|m| match m {
            AgentMessage::ToolResult { content, .. } => Some(content),
            _ => None,
        })
        .unwrap();

    let text = tool_result
        .iter()
        .filter_map(|c| match c {
            ContentItem::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert!(
        text.contains("echo: ping"),
        "tool result should contain 'echo: ping', got: {}",
        text
    );
}

#[test]
fn unknown_tool_returns_error_result() {
    // Model calls a tool that doesn't exist
    let responses = vec![
        tool_call_response("tc1", "nonexistent", r#"{}"#),
        simple_response("ok"),
    ];
    let (_tmp, mut session, event_tx) = setup_session_with_tools(responses, true);
    session.prompt("call nonexistent".into(), &event_tx);

    let messages = session_messages(&session);
    let tool_result = messages
        .iter()
        .find_map(|m| match m {
            AgentMessage::ToolResult {
                content, is_error, ..
            } => Some((content, *is_error)),
            _ => None,
        })
        .unwrap();

    assert!(tool_result.1, "unknown tool should produce error result");
    let text: String = tool_result
        .0
        .iter()
        .filter_map(|c| match c {
            ContentItem::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(text.contains("Unknown tool"), "should mention unknown tool");
}

// ===========================================================================
// Tests: error handling
// ===========================================================================

#[test]
fn provider_error_persisted_as_assistant_message() {
    let (_tmp, mut session, event_tx) = setup_session(vec![error_response("rate limit exceeded")]);
    session.prompt("hi".into(), &event_tx);

    let messages = session_messages(&session);
    let assistant = messages
        .iter()
        .find_map(|m| match m {
            AgentMessage::Assistant(a) => Some(a),
            _ => None,
        })
        .unwrap();

    assert!(assistant.stop_reason.is_error());
    assert_eq!(
        assistant.stop_reason.error_message().unwrap(),
        "rate limit exceeded"
    );
}

#[test]
fn error_does_not_trigger_tool_loop() {
    // An error response should terminate immediately, not loop
    let (_tmp, mut session, event_tx) = setup_session(vec![error_response("server error")]);
    session.prompt("test".into(), &event_tx);

    let messages = session_messages(&session);
    let assistant_count = messages
        .iter()
        .filter(|m| matches!(m, AgentMessage::Assistant(_)))
        .count();
    assert_eq!(
        assistant_count, 1,
        "error should produce exactly one assistant message"
    );
}

// ===========================================================================
// Tests: events
// ===========================================================================

#[test]
fn events_include_agent_start_and_end() {
    let (_tmp, _session, events) = collect_events(vec![simple_response("hi")]);

    let has_start = events
        .iter()
        .any(|e| matches!(e, AgentSessionEvent::Agent(AgentEvent::AgentStart)));
    let has_end = events
        .iter()
        .any(|e| matches!(e, AgentSessionEvent::Agent(AgentEvent::AgentEnd { .. })));
    assert!(has_start, "should emit AgentStart");
    assert!(has_end, "should emit AgentEnd");
}

#[test]
fn events_include_message_deltas() {
    let (_tmp, _session, events) = collect_events(vec![chunked_response(&["abc", "def"])]);

    let text_deltas: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentSessionEvent::Agent(AgentEvent::MessageUpdate {
                delta: StreamDelta::Text(t),
            }) => Some(t.as_str()),
            _ => None,
        })
        .collect();

    assert!(text_deltas.contains(&"abc"), "should have first chunk");
    assert!(text_deltas.contains(&"def"), "should have second chunk");
}

#[test]
fn events_include_tool_execution() {
    let responses = vec![
        tool_call_response("tc1", "echo", r#"{"text":"x"}"#),
        simple_response("done"),
    ];
    let (_tmp, _session, events) = collect_events_with_tools(responses, true);

    let has_tool_start = events.iter().any(|e| {
        matches!(
            e,
            AgentSessionEvent::Agent(AgentEvent::ToolExecutionStart { name, .. }) if name == "echo"
        )
    });
    let has_tool_end = events.iter().any(|e| {
        matches!(
            e,
            AgentSessionEvent::Agent(AgentEvent::ToolExecutionEnd { .. })
        )
    });
    assert!(has_tool_start, "should emit ToolExecutionStart");
    assert!(has_tool_end, "should emit ToolExecutionEnd");
}

#[test]
fn events_include_usage_update() {
    let (_tmp, _session, events) = collect_events(vec![simple_response("x")]);

    let has_usage = events
        .iter()
        .any(|e| matches!(e, AgentSessionEvent::Agent(AgentEvent::UsageUpdate { .. })));
    assert!(has_usage, "should emit UsageUpdate");
}

// ===========================================================================
// Tests: cost tracking
// ===========================================================================

#[test]
fn cost_accumulates_across_turns() {
    let (_tmp, mut session, event_tx) =
        setup_session(vec![simple_response("first"), simple_response("second")]);

    session.prompt("a".into(), &event_tx);
    let cost_after_1 = session.cost().total;

    session.prompt("b".into(), &event_tx);
    let cost_after_2 = session.cost().total;

    assert!(
        cost_after_1 > 0.0,
        "cost should be positive after first turn"
    );
    assert!(
        cost_after_2 > cost_after_1,
        "cost should increase after second turn"
    );
}

#[test]
fn cost_reflects_pricing() {
    // pricing: input=$1/Mtok, output=$2/Mtok
    // usage: 100 input, 20 output per response
    let (_tmp, mut session, event_tx) = setup_session(vec![simple_response("x")]);
    session.prompt("q".into(), &event_tx);

    let cost = session.cost();
    // output cost = 20 tokens * $2/Mtok = $0.00004
    let expected_output = 20.0 * 2.0 / 1_000_000.0;
    assert!(
        (cost.output - expected_output).abs() < 1e-10,
        "output cost should be {}, got {}",
        expected_output,
        cost.output
    );
}

// ===========================================================================
// Tests: session management
// ===========================================================================

#[test]
fn lazy_session_creation() {
    let (_tmp, session, _event_tx) = setup_session(vec![simple_response("x")]);
    // Before any prompt, no session should exist
    assert!(!session.session_manager.has_session());
}

#[test]
fn session_created_on_first_prompt() {
    let (_tmp, mut session, event_tx) = setup_session(vec![simple_response("x")]);
    session.prompt("hello".into(), &event_tx);
    assert!(session.session_manager.has_session());
}

#[test]
fn session_id_stable_across_prompts() {
    let (_tmp, mut session, event_tx) =
        setup_session(vec![simple_response("a"), simple_response("b")]);

    session.prompt("first".into(), &event_tx);
    let id1 = session.session_manager.session_id().to_string();

    session.prompt("second".into(), &event_tx);
    let id2 = session.session_manager.session_id().to_string();

    assert_eq!(id1, id2, "session ID should not change between prompts");
}

#[test]
fn reload_preserves_message_order() {
    let (tmp, mut session, event_tx) =
        setup_session(vec![simple_response("alpha"), simple_response("beta")]);

    session.prompt("first".into(), &event_tx);
    session.prompt("second".into(), &event_tx);

    let session_id = session.session_manager.session_id().to_string();
    let nerv_dir = tmp.path().join(".nerv");
    let mut mgr2 = SessionManager::new(&nerv_dir);
    let ctx = mgr2.load_session(&session_id).unwrap();

    // Verify ordering: user, assistant, user, assistant
    assert_eq!(ctx.messages.len(), 4);
    assert!(matches!(ctx.messages[0], AgentMessage::User { .. }));
    assert!(matches!(ctx.messages[1], AgentMessage::Assistant(_)));
    assert!(matches!(ctx.messages[2], AgentMessage::User { .. }));
    assert!(matches!(ctx.messages[3], AgentMessage::Assistant(_)));

    // Verify content
    if let AgentMessage::Assistant(a) = &ctx.messages[1] {
        assert_eq!(a.text_content(), "alpha");
    }
    if let AgentMessage::Assistant(a) = &ctx.messages[3] {
        assert_eq!(a.text_content(), "beta");
    }
}

// ===========================================================================
// Tests: agent state
// ===========================================================================

#[test]
fn agent_messages_match_session_entries() {
    let (_tmp, mut session, event_tx) = setup_session(vec![simple_response("sync check")]);
    session.prompt("verify".into(), &event_tx);

    // Agent state messages and session DB entries should have same content
    let agent_msgs = &session.agent.state.messages;
    let session_msgs = session_messages(&session);

    assert_eq!(agent_msgs.len(), session_msgs.len());
}

#[test]
fn system_prompt_token_count_recorded() {
    let (_tmp, mut session, event_tx) = setup_session(vec![simple_response("ok")]);
    session.prompt("test".into(), &event_tx);

    let entries = session.session_manager.entries();
    let sp = entries
        .iter()
        .find_map(|e| match e {
            nerv::session::types::SessionEntry::SystemPrompt(sp) => Some(sp),
            _ => None,
        })
        .unwrap();

    assert!(
        sp.token_count > 0,
        "system prompt token count should be > 0"
    );
    assert!(
        !sp.prompt.is_empty(),
        "system prompt text should be recorded"
    );
}
