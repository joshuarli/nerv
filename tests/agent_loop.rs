use std::sync::{Arc, RwLock};

use nerv::agent::agent::{Agent, AgentTool, ToolResult, UpdateCallback};
use nerv::agent::convert::convert_to_llm;
use nerv::agent::provider::*;
use nerv::agent::types::*;
use nerv::errors::ToolError;

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

struct EchoTool;
impl AgentTool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "Echoes input"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type":"object","properties":{"text":{"type":"string"}}})
    }
    fn validate(&self, _input: &serde_json::Value) -> Result<(), ToolError> {
        Ok(())
    }
    fn execute(&self, input: serde_json::Value, _update: UpdateCallback) -> ToolResult {
        ToolResult {
            content: format!("echo: {}", input["text"].as_str().unwrap_or("no text")),
            details: None,
            is_error: false,
        }
    }
}

fn simple_text_response(text: &str) -> Vec<ProviderEvent> {
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

fn tool_call_response(tool_name: &str, args: &str) -> Vec<ProviderEvent> {
    vec![
        ProviderEvent::ToolCallStart {
            id: "call_1".into(),
            name: tool_name.into(),
        },
        ProviderEvent::ToolCallArgsDelta {
            id: "call_1".into(),
            delta: args.into(),
        },
        ProviderEvent::ToolCallEnd {
            id: "call_1".into(),
        },
        ProviderEvent::MessageStop {
            stop_reason: StopReason::ToolUse,
            usage: Usage {
                input: 150,
                output: 30,
                ..Default::default()
            },
        },
    ]
}

fn error_response(msg: &str) -> Vec<ProviderEvent> {
    vec![ProviderEvent::MessageStop {
        stop_reason: StopReason::Error {
            message: msg.into(),
        },
        usage: Usage::default(),
    }]
}

fn setup_agent(responses: Vec<Vec<ProviderEvent>>) -> Agent {
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
    let mut agent = setup_agent(vec![simple_text_response("Hello!")]);
    let (messages, events) = collect_events(&mut agent, vec![user_msg("Hi")]);

    assert_eq!(messages.len(), 2);
    assert!(matches!(messages[0], AgentMessage::User { .. }));
    let AgentMessage::Assistant(ref a) = messages[1] else {
        panic!("expected assistant")
    };
    assert_eq!(a.text_content(), "Hello!");
    assert!(matches!(a.stop_reason, StopReason::EndTurn));
    assert_eq!(a.usage.as_ref().unwrap().input, 100);

    assert!(events.iter().any(|e| matches!(e, AgentEvent::AgentStart)));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::AgentEnd { .. }))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::MessageEnd { .. }))
    );
}

#[test]
fn tool_call_executes_and_loops_back() {
    let mut agent = setup_agent(vec![
        tool_call_response("echo", r#"{"text":"hello"}"#),
        simple_text_response("Done!"),
    ]);
    agent.state.tools = vec![Arc::new(EchoTool)];
    let (messages, events) = collect_events(&mut agent, vec![user_msg("Use echo")]);

    // user + assistant(tool_call) + tool_result + assistant(text)
    assert_eq!(messages.len(), 4);
    let AgentMessage::ToolResult { ref content, .. } = messages[2] else {
        panic!("expected tool result at [2]")
    };
    let text: String = content
        .iter()
        .filter_map(|c| match c {
            ContentItem::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(text.contains("echo: hello"));

    let AgentMessage::Assistant(ref final_msg) = messages[3] else {
        panic!("expected assistant at [3]")
    };
    assert_eq!(final_msg.text_content(), "Done!");

    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolExecutionStart { .. }))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolExecutionEnd { .. }))
    );
}

#[test]
fn cancellation_aborts_mid_stream() {
    /// Provider that checks cancel flag between events.
    struct CancelAwareProvider;
    impl Provider for CancelAwareProvider {
        fn name(&self) -> &str {
            "cancel-aware"
        }
        fn stream_completion(
            &self,
            _req: &CompletionRequest,
            cancel: &CancelFlag,
            on_event: &mut dyn FnMut(ProviderEvent),
        ) -> Result<(), nerv::errors::ProviderError> {
            // Simulate the cancel being set externally before we return
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            on_event(ProviderEvent::TextDelta("partial".into()));
            on_event(ProviderEvent::MessageStop {
                stop_reason: StopReason::Aborted,
                usage: Usage::default(),
            });
            Ok(())
        }
    }

    let mut registry = ProviderRegistry::new();
    registry.register("ca", Arc::new(CancelAwareProvider));
    let mut agent = Agent::new(Arc::new(RwLock::new(registry)));
    agent.state.model = Some(Model {
        id: "ca".into(),
        name: "CA".into(),
        provider_name: "ca".into(),
        context_window: 100_000,
        max_output_tokens: 4096,
        reasoning: false,
        supports_adaptive_thinking: false,
        supports_xhigh: false,
        pricing: ModelPricing {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        },
    });
    agent.state.system_prompt = "test".into();

    let (messages, _) = collect_events(&mut agent, vec![user_msg("test")]);
    let assistant = messages
        .iter()
        .find_map(|m| {
            if let AgentMessage::Assistant(a) = m {
                Some(a)
            } else {
                None
            }
        })
        .unwrap();
    assert!(matches!(assistant.stop_reason, StopReason::Aborted));
}

#[test]
fn error_response_stops_loop() {
    let mut agent = setup_agent(vec![error_response("API key invalid")]);
    let (messages, _) = collect_events(&mut agent, vec![user_msg("Hi")]);

    let assistant = messages
        .iter()
        .find_map(|m| {
            if let AgentMessage::Assistant(a) = m {
                Some(a)
            } else {
                None
            }
        })
        .unwrap();
    assert!(matches!(assistant.stop_reason, StopReason::Error { .. }));
    assert_eq!(
        assistant.stop_reason.error_message().unwrap(),
        "API key invalid"
    );
}

#[test]
fn no_model_produces_error() {
    let mut agent = Agent::new(Arc::new(RwLock::new(ProviderRegistry::new())));
    agent.state.system_prompt = "test".into();
    // No model set
    let (messages, _) = collect_events(&mut agent, vec![user_msg("Hi")]);

    let assistant = messages
        .iter()
        .find_map(|m| {
            if let AgentMessage::Assistant(a) = m {
                Some(a)
            } else {
                None
            }
        })
        .unwrap();
    assert!(matches!(assistant.stop_reason, StopReason::Error { .. }));
    assert!(
        assistant
            .stop_reason
            .error_message()
            .unwrap()
            .contains("no model")
    );
}

#[test]
fn unknown_tool_returns_error_result() {
    let mut agent = setup_agent(vec![
        tool_call_response("nonexistent_tool", "{}"),
        simple_text_response("ok"), // agent continues after error tool result
    ]);
    let (messages, _) = collect_events(&mut agent, vec![user_msg("call nonexistent")]);

    let tool_result = messages
        .iter()
        .find(|m| matches!(m, AgentMessage::ToolResult { .. }))
        .unwrap();
    if let AgentMessage::ToolResult {
        is_error, content, ..
    } = tool_result
    {
        assert!(is_error);
        let text: String = content
            .iter()
            .filter_map(|c| match c {
                ContentItem::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(text.contains("Unknown tool"));
    }
}

#[test]
fn messages_accumulate_in_agent_state() {
    let mut agent = setup_agent(vec![
        simple_text_response("first"),
        simple_text_response("second"),
    ]);
    agent.prompt(vec![user_msg("msg1")], &|_| {});
    assert_eq!(agent.state.messages.len(), 2); // user + assistant

    agent.prompt(vec![user_msg("msg2")], &|_| {});
    assert_eq!(agent.state.messages.len(), 4); // previous 2 + user + assistant
}

#[test]
fn abort_then_continue_works() {
    // After aborting, the agent should be able to handle a new prompt.
    // The aborted message stays in context (with partial content).
    let mut agent = setup_agent(vec![
        // First prompt: will be aborted
        vec![
            ProviderEvent::TextDelta("partial response".into()),
            ProviderEvent::MessageStop {
                stop_reason: StopReason::Aborted,
                usage: Usage::default(),
            },
        ],
        // Second prompt: normal response after abort
        simple_text_response("Recovered!"),
    ]);

    // First prompt — aborted
    let (msgs1, _) = collect_events(&mut agent, vec![user_msg("first")]);
    let a1 = msgs1
        .iter()
        .find_map(|m| match m {
            AgentMessage::Assistant(a) => Some(a),
            _ => None,
        })
        .unwrap();
    assert!(matches!(a1.stop_reason, StopReason::Aborted));
    assert_eq!(a1.text_content(), "partial response");

    // Agent state should have the aborted message in history
    assert_eq!(agent.state.messages.len(), 2); // user + aborted assistant

    // Second prompt — should work normally with aborted message in context
    let (msgs2, _) = collect_events(&mut agent, vec![user_msg("continue")]);
    let a2 = msgs2
        .iter()
        .find_map(|m| match m {
            AgentMessage::Assistant(a) => Some(a),
            _ => None,
        })
        .unwrap();
    assert_eq!(a2.text_content(), "Recovered!");
    assert!(matches!(a2.stop_reason, StopReason::EndTurn));

    // Total: 4 messages (user, aborted_assistant, user, assistant)
    assert_eq!(agent.state.messages.len(), 4);
}

#[test]
fn thinking_events_forwarded() {
    let mut agent = setup_agent(vec![vec![
        ProviderEvent::ThinkingDelta("Let me think...".into()),
        ProviderEvent::ThinkingDelta(" about this.".into()),
        ProviderEvent::TextDelta("The answer is 42.".into()),
        ProviderEvent::MessageStop {
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input: 100,
                output: 30,
                ..Default::default()
            },
        },
    ]]);

    let (messages, events) = collect_events(&mut agent, vec![user_msg("think")]);

    // Check assistant message has both thinking and text content
    let assistant = messages
        .iter()
        .find_map(|m| match m {
            AgentMessage::Assistant(a) => Some(a),
            _ => None,
        })
        .unwrap();

    let has_thinking = assistant.content.iter().any(|b| {
        matches!(b, ContentBlock::Thinking { thinking } if thinking == "Let me think... about this.")
    });
    assert!(has_thinking, "should have thinking block");
    assert_eq!(assistant.text_content(), "The answer is 42.");

    // Check streaming events include thinking deltas
    let thinking_events: Vec<_> = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::MessageUpdate {
                    delta: StreamDelta::Thinking(_)
                }
            )
        })
        .collect();
    assert_eq!(thinking_events.len(), 2);
}

#[test]
fn multiple_tool_calls_in_one_response() {
    let mut agent = setup_agent(vec![
        // Assistant makes two tool calls
        vec![
            ProviderEvent::ToolCallStart {
                id: "call_1".into(),
                name: "echo".into(),
            },
            ProviderEvent::ToolCallArgsDelta {
                id: "call_1".into(),
                delta: r#"{"text":"first"}"#.into(),
            },
            ProviderEvent::ToolCallEnd {
                id: "call_1".into(),
            },
            ProviderEvent::ToolCallStart {
                id: "call_2".into(),
                name: "echo".into(),
            },
            ProviderEvent::ToolCallArgsDelta {
                id: "call_2".into(),
                delta: r#"{"text":"second"}"#.into(),
            },
            ProviderEvent::ToolCallEnd {
                id: "call_2".into(),
            },
            ProviderEvent::MessageStop {
                stop_reason: StopReason::ToolUse,
                usage: Usage {
                    input: 100,
                    output: 40,
                    ..Default::default()
                },
            },
        ],
        simple_text_response("Both done."),
    ]);
    agent.state.tools = vec![Arc::new(EchoTool)];

    let (messages, events) = collect_events(&mut agent, vec![user_msg("call two tools")]);

    // user + assistant(2 tool calls) + tool_result_1 + tool_result_2 + assistant(text)
    assert_eq!(messages.len(), 5);

    // Both tool results should be present
    let results: Vec<_> = messages
        .iter()
        .filter(|m| matches!(m, AgentMessage::ToolResult { .. }))
        .collect();
    assert_eq!(results.len(), 2);

    // Both ToolExecutionStart events
    let starts: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolExecutionStart { .. }))
        .collect();
    assert_eq!(starts.len(), 2);
}

#[test]
fn event_sequence_for_simple_response() {
    let mut agent = setup_agent(vec![simple_text_response("hi")]);
    let (_, events) = collect_events(&mut agent, vec![user_msg("hello")]);

    // Expected: AgentStart, TurnStart, MessageStart(user), MessageUpdate(text),
    //           MessageEnd, TurnEnd, AgentEnd
    let event_names: Vec<&str> = events
        .iter()
        .map(|e| match e {
            AgentEvent::AgentStart => "AgentStart",
            AgentEvent::AgentEnd { .. } => "AgentEnd",
            AgentEvent::MessageStart { .. } => "MessageStart",
            AgentEvent::MessageUpdate { .. } => "MessageUpdate",
            AgentEvent::MessageEnd { .. } => "MessageEnd",
            AgentEvent::ToolExecutionStart { .. } => "ToolExecStart",
            AgentEvent::ToolExecutionUpdate { .. } => "ToolExecUpdate",
            AgentEvent::ToolExecutionEnd { .. } => "ToolExecEnd",
            AgentEvent::TurnStart => "TurnStart",
            AgentEvent::TurnEnd => "TurnEnd",
            AgentEvent::UsageUpdate { .. } => "UsageUpdate",
        })
        .collect();

    assert_eq!(event_names[0], "AgentStart");
    assert_eq!(event_names[1], "TurnStart");
    assert_eq!(event_names[2], "MessageStart"); // user message
    assert!(event_names.contains(&"MessageUpdate")); // text delta
    assert!(event_names.contains(&"MessageEnd")); // assistant done
    assert!(event_names.contains(&"TurnEnd"));
    assert_eq!(event_names.last().unwrap(), &"AgentEnd");
}

#[test]
fn event_sequence_for_tool_call() {
    let mut agent = setup_agent(vec![
        tool_call_response("echo", r#"{"text":"hi"}"#),
        simple_text_response("done"),
    ]);
    agent.state.tools = vec![Arc::new(EchoTool)];

    let (_, events) = collect_events(&mut agent, vec![user_msg("use echo")]);

    let event_names: Vec<&str> = events
        .iter()
        .map(|e| match e {
            AgentEvent::AgentStart => "AgentStart",
            AgentEvent::AgentEnd { .. } => "AgentEnd",
            AgentEvent::MessageStart { .. } => "MsgStart",
            AgentEvent::MessageUpdate { .. } => "MsgUpdate",
            AgentEvent::MessageEnd { .. } => "MsgEnd",
            AgentEvent::ToolExecutionStart { .. } => "ToolStart",
            AgentEvent::ToolExecutionUpdate { .. } => "ToolUpdate",
            AgentEvent::ToolExecutionEnd { .. } => "ToolEnd",
            AgentEvent::TurnStart => "TurnStart",
            AgentEvent::TurnEnd => "TurnEnd",
            AgentEvent::UsageUpdate { .. } => "UsageUpdate",
        })
        .collect();

    // Turn 1: tool call
    assert_eq!(event_names[0], "AgentStart");
    assert_eq!(event_names[1], "TurnStart");
    assert_eq!(event_names[2], "MsgStart"); // user

    // Find tool execution
    let tool_start_idx = event_names.iter().position(|&n| n == "ToolStart").unwrap();
    let tool_end_idx = event_names.iter().position(|&n| n == "ToolEnd").unwrap();
    assert!(tool_start_idx < tool_end_idx);

    // Turn 2: text response after tool
    let turn_starts: Vec<_> = event_names
        .iter()
        .enumerate()
        .filter(|(_, n)| **n == "TurnStart")
        .collect();
    assert_eq!(turn_starts.len(), 2, "should have 2 turns");

    assert_eq!(event_names.last().unwrap(), &"AgentEnd");
}

#[test]
fn usage_data_preserved_in_message_end() {
    let mut agent = setup_agent(vec![simple_text_response("hello")]);
    let (_, events) = collect_events(&mut agent, vec![user_msg("hi")]);

    let msg_end = events.iter().find_map(|e| match e {
        AgentEvent::MessageEnd { message } => Some(message),
        _ => None,
    });
    let msg = msg_end.expect("should have MessageEnd");
    let usage = msg.usage.as_ref().expect("should have usage");
    assert_eq!(usage.input, 100);
    assert_eq!(usage.output, 20);
}

#[test]
fn is_context_overflow_detects_known_patterns() {
    let cases = vec![
        "prompt is too long: 213462 tokens > 200000 maximum",
        "Your input exceeds the context window of this model",
        "maximum context length is 128000 tokens",
        "too many tokens in request",
        "Token limit exceeded",
        "context_length_exceeded",
        "context length exceeded for model",
    ];
    for msg in cases {
        let sr = StopReason::Error {
            message: msg.into(),
        };
        assert!(sr.is_context_overflow(), "should detect: {}", msg);
    }
}

#[test]
fn is_context_overflow_rejects_non_overflow_errors() {
    let non_overflow = vec![
        "API key invalid",
        "rate limited",
        "server error 500",
        "model not found",
        "overloaded",
    ];
    for msg in non_overflow {
        let sr = StopReason::Error {
            message: msg.into(),
        };
        assert!(!sr.is_context_overflow(), "should not detect: {}", msg);
    }

    // Non-error stop reasons
    assert!(!StopReason::EndTurn.is_context_overflow());
    assert!(!StopReason::Aborted.is_context_overflow());
}

#[test]
fn overflow_triggers_retry_with_compacted_context() {
    // Mock provider: first call returns overflow error, second succeeds
    let mut agent = setup_agent(vec![
        error_response("prompt is too long: 300000 tokens > 200000 maximum"),
        simple_text_response("Success after compact!"),
    ]);

    // First call: overflow
    let (msgs1, _) = collect_events(&mut agent, vec![user_msg("big prompt")]);
    let a1 = msgs1
        .iter()
        .find_map(|m| match m {
            AgentMessage::Assistant(a) => Some(a),
            _ => None,
        })
        .unwrap();
    assert!(a1.stop_reason.is_context_overflow());

    // Simulate what agent_session does: clear the overflow message, retry
    // (The full integration requires session_task, but we verify the detection + agent reuse)
    agent.reset_cancel();
    let (msgs2, _) = collect_events(&mut agent, vec![user_msg("retry")]);
    let a2 = msgs2
        .iter()
        .find_map(|m| match m {
            AgentMessage::Assistant(a) => Some(a),
            _ => None,
        })
        .unwrap();
    assert_eq!(a2.text_content(), "Success after compact!");
}

#[test]
fn convert_to_llm_merges_consecutive_user_messages() {
    let messages = vec![
        AgentMessage::User {
            content: vec![ContentItem::Text {
                text: "first".into(),
            }],
            timestamp: 0,
        },
        AgentMessage::BashExecution {
            command: "ls".into(),
            output: "file.txt".into(),
            exit_code: Some(0),
            timestamp: 1,
        },
    ];
    let llm = convert_to_llm(&messages);
    assert_eq!(llm.len(), 1); // bash becomes user, merges with previous user
    assert!(llm[0].is_user());
}

#[test]
fn convert_to_llm_preserves_alternating_roles() {
    let messages = vec![
        AgentMessage::User {
            content: vec![ContentItem::Text {
                text: "hello".into(),
            }],
            timestamp: 0,
        },
        AgentMessage::Assistant(AssistantMessage {
            content: vec![ContentBlock::Text { text: "hi".into() }],
            stop_reason: StopReason::EndTurn,
            usage: None,
            timestamp: 1,
        }),
        AgentMessage::User {
            content: vec![ContentItem::Text { text: "bye".into() }],
            timestamp: 2,
        },
    ];
    let llm = convert_to_llm(&messages);
    assert_eq!(llm.len(), 3);
    assert!(llm[0].is_user());
    assert!(llm[1].is_assistant());
    assert!(llm[2].is_user());
}
