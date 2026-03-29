use nerv::agent::provider::new_cancel_flag;
use nerv::agent::types::*;
use nerv::core::retry::{RetryManager, RetrySettings};

fn make_error_message(error_text: &str) -> AssistantMessage {
    AssistantMessage {
        content: vec![],
        stop_reason: StopReason::Error { message: error_text.into() },
        usage: None,
        timestamp: 0,
    }
}

fn make_success_message() -> AssistantMessage {
    AssistantMessage {
        content: vec![ContentBlock::Text { text: "ok".into() }],
        stop_reason: StopReason::EndTurn,
        usage: None,
        timestamp: 0,
    }
}

#[test]
fn retryable_errors_are_detected() {
    let mgr = RetryManager::new(RetrySettings::default());
    assert!(mgr.is_retryable(&make_error_message("server overloaded")));
    assert!(mgr.is_retryable(&make_error_message("rate limit exceeded")));
    assert!(mgr.is_retryable(&make_error_message("HTTP 529")));
    assert!(mgr.is_retryable(&make_error_message("503 Service Unavailable")));
    assert!(mgr.is_retryable(&make_error_message("500 Internal Server Error")));
}

#[test]
fn non_retryable_errors() {
    let mgr = RetryManager::new(RetrySettings::default());
    assert!(!mgr.is_retryable(&make_error_message("invalid API key")));
    assert!(!mgr.is_retryable(&make_success_message()));
}

#[test]
fn disabled_retry_never_retries() {
    let mgr = RetryManager::new(RetrySettings { enabled: false, ..RetrySettings::default() });
    assert!(!mgr.is_retryable(&make_error_message("overloaded")));
}

#[test]
fn max_attempts_respected() {
    let mut mgr = RetryManager::new(RetrySettings {
        max_attempts: 2,
        initial_delay_ms: 1,
        ..RetrySettings::default()
    });
    assert!(mgr.is_retryable(&make_error_message("overloaded")));
    let cancel = new_cancel_flag();
    mgr.wait(&cancel);
    mgr.wait(&cancel);
    assert!(!mgr.is_retryable(&make_error_message("overloaded")));
}

#[test]
fn reset_clears_attempt_count() {
    let mut mgr = RetryManager::new(RetrySettings {
        max_attempts: 1,
        initial_delay_ms: 1,
        ..RetrySettings::default()
    });
    let cancel = new_cancel_flag();
    mgr.wait(&cancel);
    assert!(!mgr.is_retryable(&make_error_message("overloaded")));
    mgr.reset();
    assert!(mgr.is_retryable(&make_error_message("overloaded")));
}
