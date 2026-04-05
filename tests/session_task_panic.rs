/// Integration test: session thread panic causes silent exit.
///
/// The bug: if session_task panics, event_tx drops, and the main event loop's
/// `let Ok(event) = msg else { break }` fires — silently exiting nerv with no
/// error message shown to the user.
///
/// The fix: session_task catches panics via `std::panic::catch_unwind` and
/// sends a recoverable Status error event before exiting, so the main loop can
/// display "internal error" rather than silently disappearing.
mod helpers;

use std::sync::{Arc, RwLock};

use helpers::*;
use nerv::agent::agent::{AgentTool, ToolResult};
use nerv::agent::provider::*;
use nerv::agent::types::*;
use nerv::core::agent_session::AgentSession;
use nerv::core::config::NervConfig;
use nerv::core::model_registry::ModelRegistry;
use nerv::core::tool_registry::ToolRegistry;
use nerv::core::{AgentSessionEvent, SessionCommand, session_task};
use nerv::errors::ToolError;
use nerv::session::SessionManager;
use tempfile::TempDir;

/// A provider that panics mid-stream — simulates an unexpected internal error
/// (e.g. a malformed response causing an unwrap in parsing code).
struct PanickingProvider;

impl Provider for PanickingProvider {
    fn name(&self) -> &str {
        "panicking"
    }
    fn stream_completion(
        &self,
        _request: &CompletionRequest,
        _cancel: &CancelFlag,
        _on_event: &mut dyn FnMut(ProviderEvent),
    ) -> Result<(), nerv::errors::ProviderError> {
        panic!("simulated internal provider panic");
    }
}

fn panicking_session() -> (TempDir, AgentSession) {
    let tmp = TempDir::new().unwrap();
    let nerv_dir = tmp.path().join(".nerv");
    std::fs::create_dir_all(&nerv_dir).unwrap();

    let provider = Arc::new(PanickingProvider);
    let mut registry = ProviderRegistry::new();
    registry.register("panicking", provider);

    let mut agent = nerv::agent::agent::Agent::new(Arc::new(RwLock::new(registry)));
    agent.set_model(Some(Model {
        id: "panic-model".into(),
        name: "Panic".into(),
        provider_name: "panicking".into(),
        context_window: 100_000,
        max_output_tokens: 4_000,
        reasoning: false,
        supports_adaptive_thinking: false,
        supports_xhigh: false,
        pricing: ModelPricing { input: 0.0, output: 0.0, cache_read: 0.0, cache_write: 0.0 },
    }));

    let session_manager = SessionManager::new(&nerv_dir);
    let mut session = AgentSession::new(
        agent,
        session_manager,
        ToolRegistry::new(),
        Arc::new(ModelRegistry::empty()),
        empty_resources(),
        tmp.path().to_path_buf(),
        NervConfig::default(),
    );
    session.disable_session_naming();
    (tmp, session)
}

/// A panic in session_task sends a Status error event before
/// the channel disconnects, so the main loop can display an error message
/// instead of silently exiting.
///
/// To make this test pass, session_task must wrap its body in
/// std::panic::catch_unwind and send a Status{is_error:true} event on panic.
#[test]
fn session_task_panic_sends_error_event_after_fix() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));

    let (_tmp, session) = panicking_session();
    let (cmd_tx, cmd_rx) = crossbeam_channel::bounded::<SessionCommand>(32);
    let (event_tx, event_rx) = crossbeam_channel::bounded::<AgentSessionEvent>(64);

    std::thread::spawn(move || {
        session_task(cmd_rx, event_tx, session);
    });

    cmd_tx.send(SessionCommand::Prompt { text: "trigger panic".into() }).unwrap();

    let mut saw_status_error = false;
    loop {
        match event_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(AgentSessionEvent::Status { is_error: true, .. }) => {
                saw_status_error = true;
            }
            Ok(_) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                panic!("timed out waiting for session thread");
            }
        }
    }

    std::panic::set_hook(prev);

    assert!(
        saw_status_error,
        "session_task should send a Status error event when it panics, not disconnect silently"
    );
}
