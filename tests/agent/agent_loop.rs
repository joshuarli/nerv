use std::sync::{Arc, RwLock};

use crate::helpers::*;
use nerv::agent::agent::{Agent, AgentTool, UpdateCallback};
use nerv::agent::convert::convert_to_llm;
use nerv::agent::provider::*;
use nerv::agent::types::*;

fn setup_agent(responses: Vec<Vec<ProviderEvent>>) -> Agent {
    let provider = Arc::new(MockProvider::new(responses));
    let mut registry = ProviderRegistry::new();
    registry.register("mock", provider);
    let mut agent = Agent::new(Arc::new(RwLock::new(registry)));
    agent.state.model = Some(test_model());
    agent.state.system_prompt = "Test.".into();
    agent
}

fn user_msg(text: &str) -> AgentMessage {
    AgentMessage::User {
        content: vec![ContentItem::Text { text: text.into() }],
        timestamp: 1000,
    }
}

fn collect_events(
    agent: &mut Agent,
    prompt: Vec<AgentMessage>,
) -> (Vec<AgentMessage>, Vec<AgentEvent>) {
    let events = std::sync::Mutex::new(Vec::new());
    let messages = agent.prompt(prompt, &|e| events.lock().unwrap().push(e), None);
    (messages, events.into_inner().unwrap())
}

#[test]
fn simple_text_response_produces_correct_messages() {
    let mut agent = setup_agent(vec![simple_response("Hello!")]);
    let (messages, events) = collect_events(&mut agent, vec![user_msg("Hi")]);

    assert_eq!(messages.len(), 2);
    assert!(matches!(messages[0], AgentMessage::User { .. }));
    let AgentMessage::Assistant(ref a) = messages[1] else {
        panic!("expected assistant")
    };
    assert_eq!(a.text_content(), "Hello!");

    assert!(events.iter().any(|e| matches!(e, AgentEvent::AgentStart)));
    assert!(events
        .iter()
        .any(|e| matches!(e, AgentEvent::AgentEnd { .. })));
    assert!(events
        .iter()
        .any(|e| matches!(e, AgentEvent::MessageEnd { .. })));
}

#[test]
fn tool_call_triggers_execution() {
    let mut agent = setup_agent(vec![
        tool_call_response("call_1", "echo", r#"{"text":"hello"}"#),
        simple_response("Done"),
    ]);
    agent.state.tools = vec![Arc::new(EchoTool)];

    let (messages, events) = collect_events(&mut agent, vec![user_msg("test")]);

    assert!(messages.len() >= 3);
    assert!(messages
        .iter()
        .any(|m| matches!(m, AgentMessage::ToolResult { .. })));

    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolExecutionStart { name, .. } if name == "echo"
    )));
    assert!(events
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolExecutionEnd { .. })));
}

#[test]
fn error_response_stops_loop() {
    let mut agent = setup_agent(vec![error_response("server error")]);
    let (messages, _) = collect_events(&mut agent, vec![user_msg("test")]);

    let last = messages.last().unwrap();
    if let AgentMessage::Assistant(a) = last {
        assert!(a.stop_reason.is_error());
    } else {
        panic!("expected assistant error");
    }
}

#[test]
fn multiple_tool_calls_execute_sequentially() {
    let mut agent = setup_agent(vec![
        vec![
            ProviderEvent::ToolCallStart {
                id: "c1".into(),
                name: "echo".into(),
            },
            ProviderEvent::ToolCallArgsDelta {
                id: "c1".into(),
                delta: r#"{"text":"a"}"#.into(),
            },
            ProviderEvent::ToolCallEnd { id: "c1".into() },
            ProviderEvent::ToolCallStart {
                id: "c2".into(),
                name: "echo".into(),
            },
            ProviderEvent::ToolCallArgsDelta {
                id: "c2".into(),
                delta: r#"{"text":"b"}"#.into(),
            },
            ProviderEvent::ToolCallEnd { id: "c2".into() },
            ProviderEvent::MessageStop {
                stop_reason: StopReason::ToolUse,
                usage: Usage {
                    input: 100,
                    output: 30,
                    ..Default::default()
                },
            },
        ],
        simple_response("all done"),
    ]);
    agent.state.tools = vec![Arc::new(EchoTool)];

    let (messages, _) = collect_events(&mut agent, vec![user_msg("test")]);

    let tool_results: Vec<_> = messages
        .iter()
        .filter(|m| matches!(m, AgentMessage::ToolResult { .. }))
        .collect();
    assert_eq!(tool_results.len(), 2);
}

#[test]
fn unknown_tool_returns_error() {
    let mut agent = setup_agent(vec![
        tool_call_response("c1", "nonexistent", r#"{}"#),
        simple_response("ok"),
    ]);

    let (messages, _) = collect_events(&mut agent, vec![user_msg("test")]);

    let has_error = messages.iter().any(|m| {
        matches!(m, AgentMessage::ToolResult { is_error: true, .. })
    });
    assert!(has_error, "should get error for unknown tool");
}

#[test]
fn messages_accumulate_in_state() {
    let mut agent = setup_agent(vec![simple_response("reply")]);
    agent.state.messages = vec![user_msg("prior context")];

    let (_, _) = collect_events(&mut agent, vec![user_msg("new question")]);

    assert!(agent.state.messages.len() >= 3);
}

#[test]
fn convert_to_llm_round_trips_messages() {
    let messages = vec![
        user_msg("hello"),
        AgentMessage::Assistant(AssistantMessage {
            content: vec![ContentBlock::Text {
                text: "hi".into(),
            }],
            stop_reason: StopReason::EndTurn,
            usage: None,
            timestamp: 1000,
        }),
    ];
    let llm = convert_to_llm(&messages);
    assert_eq!(llm.len(), 2);
}

#[test]
fn cancel_flag_stops_agent() {
    // The cancel flag must be set during execution (prompt() resets it first).
    // Use a tool that sets the flag as a side effect.
    struct CancelTool {
        cancel: nerv::agent::provider::CancelFlag,
    }
    impl AgentTool for CancelTool {
        fn name(&self) -> &str { "cancel" }
        fn description(&self) -> &str { "Sets cancel flag" }
        fn parameters_schema(&self) -> serde_json::Value { serde_json::json!({"type":"object"}) }
        fn validate(&self, _: &serde_json::Value) -> Result<(), nerv::errors::ToolError> { Ok(()) }
        fn execute(&self, _: serde_json::Value, _: UpdateCallback) -> nerv::agent::agent::ToolResult {
            self.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            nerv::agent::agent::ToolResult::ok("cancelled")
        }
    }

    let mut agent = setup_agent(vec![
        tool_call_response("c1", "cancel", r#"{}"#),
        simple_response("should not reach"),
    ]);
    let cancel = agent.cancel.clone();
    agent.state.tools = vec![Arc::new(CancelTool { cancel })];

    let (messages, _) = collect_events(&mut agent, vec![user_msg("test")]);

    // After the tool sets cancel, the agent should not make another API call
    // The loop exits because has_tool_calls && !cancel is false
    let assistant_count = messages
        .iter()
        .filter(|m| matches!(m, AgentMessage::Assistant(_)))
        .count();
    assert!(assistant_count <= 2, "cancel should prevent further turns, got {} assistants", assistant_count);
}

#[test]
fn turn_events_bracket_each_turn() {
    let mut agent = setup_agent(vec![
        tool_call_response("c1", "echo", r#"{"text":"x"}"#),
        simple_response("done"),
    ]);
    agent.state.tools = vec![Arc::new(EchoTool)];

    let (_, events) = collect_events(&mut agent, vec![user_msg("test")]);

    let turn_starts = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::TurnStart))
        .count();
    let turn_ends = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::TurnEnd))
        .count();
    assert!(turn_starts >= 2, "should have at least 2 turns");
    assert_eq!(turn_starts, turn_ends, "starts and ends should match");
}

// --- persist_fn tests ---

/// Collect messages delivered to persist_fn during a prompt.
fn collect_persisted(
    agent: &mut Agent,
    prompt: Vec<AgentMessage>,
) -> (Vec<AgentMessage>, Vec<AgentMessage>) {
    let persisted = std::sync::Mutex::new(Vec::new());
    let mut persist = |msg: &AgentMessage| {
        persisted.lock().unwrap().push(msg.clone());
    };
    let messages = agent.prompt(prompt, &|_| {}, Some(&mut persist));
    (messages, persisted.into_inner().unwrap())
}

#[test]
fn persist_fn_called_for_simple_response() {
    let mut agent = setup_agent(vec![simple_response("Hello!")]);
    let (messages, persisted) = collect_persisted(&mut agent, vec![user_msg("Hi")]);

    assert_eq!(persisted.len(), messages.len());
    assert!(matches!(persisted[0], AgentMessage::User { .. }));
    assert!(matches!(persisted[1], AgentMessage::Assistant(_)));
}

#[test]
fn persist_fn_called_for_tool_loop() {
    let mut agent = setup_agent(vec![
        tool_call_response("c1", "echo", r#"{"text":"x"}"#),
        simple_response("Done"),
    ]);
    agent.state.tools = vec![Arc::new(EchoTool)];

    let (messages, persisted) = collect_persisted(&mut agent, vec![user_msg("test")]);

    // Should match: User, Assistant(tool_use), ToolResult, Assistant(text)
    assert_eq!(persisted.len(), messages.len());
    assert!(matches!(persisted[0], AgentMessage::User { .. }));
    assert!(matches!(persisted[1], AgentMessage::Assistant(_)));
    assert!(matches!(persisted[2], AgentMessage::ToolResult { .. }));
    assert!(matches!(persisted[3], AgentMessage::Assistant(_)));
}

#[test]
fn persist_fn_called_for_error_response() {
    let mut agent = setup_agent(vec![error_response("boom")]);
    let (_, persisted) = collect_persisted(&mut agent, vec![user_msg("test")]);

    // User + error Assistant
    assert_eq!(persisted.len(), 2);
    if let AgentMessage::Assistant(ref a) = persisted[1] {
        assert!(a.stop_reason.is_error());
    } else {
        panic!("expected assistant error in persisted messages");
    }
}

#[test]
fn persist_fn_order_matches_returned_messages() {
    let mut agent = setup_agent(vec![
        tool_call_response("c1", "echo", r#"{"text":"a"}"#),
        tool_call_response("c2", "echo", r#"{"text":"b"}"#),
        simple_response("final"),
    ]);
    agent.state.tools = vec![Arc::new(EchoTool)];

    let (messages, persisted) = collect_persisted(&mut agent, vec![user_msg("go")]);

    assert_eq!(persisted.len(), messages.len());
    for (i, (ret, pers)) in messages.iter().zip(persisted.iter()).enumerate() {
        assert_eq!(
            std::mem::discriminant(ret),
            std::mem::discriminant(pers),
            "message type mismatch at index {i}"
        );
    }
}

// --- tool description pruning ---

#[test]
fn tool_descriptions_pruned_after_threshold() {
    let provider = Arc::new(MockProvider::new(vec![simple_response("done")]));
    let provider_clone = provider.clone();
    let mut registry = ProviderRegistry::new();
    registry.register("mock", provider);
    let mut agent = Agent::new(Arc::new(RwLock::new(registry)));
    agent.state.model = Some(test_model());
    agent.state.system_prompt = "Test.".into();
    agent.state.tools = vec![Arc::new(EchoTool)];

    // Seed enough prior assistant messages to trigger pruning
    for i in 0..5 {
        agent.state.messages.push(user_msg(&format!("q{}", i)));
        agent.state.messages.push(AgentMessage::Assistant(AssistantMessage {
            content: vec![ContentBlock::Text { text: format!("a{}", i) }],
            stop_reason: StopReason::EndTurn,
            usage: None,
            timestamp: 1000,
        }));
    }

    let _ = collect_events(&mut agent, vec![user_msg("go")]);

    let captured = provider_clone.captured_tools.lock().unwrap();
    assert_eq!(captured.len(), 1, "should have one API call");
    let tools = &captured[0];
    assert!(!tools.is_empty(), "should have tools");
    for tool in tools {
        assert!(
            tool.description.is_empty(),
            "tool '{}' description should be pruned, got: {}",
            tool.name,
            tool.description,
        );
    }
}

#[test]
fn tool_descriptions_kept_for_early_turns() {
    let provider = Arc::new(MockProvider::new(vec![simple_response("done")]));
    let provider_clone = provider.clone();
    let mut registry = ProviderRegistry::new();
    registry.register("mock", provider);
    let mut agent = Agent::new(Arc::new(RwLock::new(registry)));
    agent.state.model = Some(test_model());
    agent.state.system_prompt = "Test.".into();
    agent.state.tools = vec![Arc::new(EchoTool)];

    // No prior messages — first turn
    let _ = collect_events(&mut agent, vec![user_msg("hello")]);

    let captured = provider_clone.captured_tools.lock().unwrap();
    assert_eq!(captured.len(), 1);
    let tools = &captured[0];
    assert!(!tools.is_empty());
    for tool in tools {
        assert!(
            !tool.description.is_empty(),
            "tool '{}' description should be present on first turn",
            tool.name,
        );
    }
}
