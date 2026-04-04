//! Plan mode tests — tool restriction and system prompt injection.

use std::sync::Arc;

use nerv::core::agent_session::AgentSessionEvent;
use nerv::core::tool_registry::ToolRegistry;

use crate::helpers::*;

#[test]
fn set_active_filters_tools() {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(EchoTool));

    // No active filter → all tools returned
    let all = registry.active_tools();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].name(), "echo");

    // Filter to a name that doesn't match → empty
    registry.set_active(&["read"]);
    assert!(registry.active_tools().is_empty());

    // Filter to matching name → returned
    registry.set_active(&["echo"]);
    assert_eq!(registry.active_tools().len(), 1);

    // Clear filter → all tools again
    registry.set_active(&[]);
    assert_eq!(registry.active_tools().len(), 1);
}

#[test]
fn plan_mode_restricts_tools_and_injects_prompt() {
    let (_tmp, mut session, tx) =
        mock_session(
            vec![
                simple_response("plan output"),
                // correction prompt injected when "plan output" has no questions JSON block
                simple_response("{\"questions\":[]}"),
                simple_response("edit done"),
            ],
            true,
        );

    // Enable plan mode
    session.set_plan_mode(true, &tx);

    // The tool registry should now only return read-only tools.
    // Our mock session only has EchoTool, which is not in the plan-mode allow list,
    // so active_tools should be empty.
    let active = session.tool_registry.active_tools();
    assert!(
        active.is_empty(),
        "echo tool should be excluded in plan mode, got {} tools",
        active.len()
    );

    // Run a prompt — the system prompt should contain the plan mode section
    session.prompt("what files exist?".into(), &tx);
    assert!(
        session.agent.state.system_prompt.contains("# Plan Mode"),
        "system prompt should contain plan mode instructions"
    );

    // Disable plan mode
    session.set_plan_mode(false, &tx);
    let active = session.tool_registry.active_tools();
    assert_eq!(active.len(), 1, "all tools should be restored");

    // Run another prompt — plan mode section should be gone
    session.prompt("now edit something".into(), &tx);
    assert!(
        !session.agent.state.system_prompt.contains("# Plan Mode"),
        "system prompt should not contain plan mode after disabling"
    );
}

#[test]
fn plan_mode_event_emitted() {
    let (_tmp, mut session, _tx) = mock_session(vec![], false);
    let (tx, rx) = crossbeam_channel::unbounded();

    session.set_plan_mode(true, &tx);
    let event = rx.try_recv().unwrap();
    assert!(
        matches!(event, AgentSessionEvent::PlanModeChanged { enabled: true }),
        "should emit PlanModeChanged {{ enabled: true }}"
    );

    session.set_plan_mode(false, &tx);
    let event = rx.try_recv().unwrap();
    assert!(
        matches!(event, AgentSessionEvent::PlanModeChanged { enabled: false }),
        "should emit PlanModeChanged {{ enabled: false }}"
    );
}
