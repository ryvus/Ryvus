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

    #[error("invalid protocol version: expected={expected}, actual={actual}")]
    InvalidProtocolVersion { expected: String, actual: String },
}

pub type ExecutorResult<T> = Result<T, ExecutorError>;
