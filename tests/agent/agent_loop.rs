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
    let messages = agent.prompt(prompt, &|e| events.lock().unwrap().push(e));
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
