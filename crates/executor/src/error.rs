use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExecutorError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("process failed: command={command}, exit_code={exit_code:?}, stderr={stderr}")]
    ProcessFailed {
        command: String,
        exit_code: Option<i32>,
        stderr: String,
    },

    #[error("process timed out: command={command}, timeout_ms={timeout_ms}")]
    ProcessTimedOut { command: String, timeout_ms: u128 },

    #[error("invalid protocol version: expected={expected}, actual={actual}")]
    InvalidProtocolVersion { expected: String, actual: String },

    #[error("process completed without emitting invocation result")]
    MissingInvocationResult,
}

pub type ExecutorResult<T> = Result<T, ExecutorError>;
