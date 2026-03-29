use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("rate limited (retry after {retry_after_ms:?}ms)")]
    RateLimited { retry_after_ms: Option<u64> },

    #[error("overloaded")]
    Overloaded,

    #[error("authentication failed: {message}")]
    Auth { message: String },

    #[error("server error ({status}): {message}")]
    Server { status: u16, message: String },

    #[error("request cancelled")]
    Cancelled,

    #[error("network error: {0}")]
    Network(String),

    #[error("SSE parse error: {message}")]
    SseParse { message: String },
}

impl ProviderError {
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::RateLimited { .. } | Self::Overloaded | Self::Server { status: 500..=599, .. }
        )
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("file not found: {path}")]
    FileNotFound { path: PathBuf },

    #[error("permission denied: {path}")]
    PermissionDenied { path: PathBuf },

    #[error("invalid arguments: {message}")]
    InvalidArguments { message: String },

    #[error("validation failed: {message}")]
    ValidationFailed { message: String },

    #[error("process failed (exit {exit_code:?}): {message}")]
    ProcessFailed { exit_code: Option<i32>, message: String },

    #[error("tool execution cancelled")]
    Cancelled,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("corrupt session file {path}: {reason}")]
    Corrupt { path: PathBuf, reason: String },

    #[error("migration failed (v{from}→v{to}): {reason}")]
    MigrationFailed { from: u32, to: u32, reason: String },

    #[error("session I/O error: {0}")]
    Io(#[from] std::io::Error),
}
