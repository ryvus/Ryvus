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

    #[error("process failed to start: command={command}, error={io_error}")]
    ProcessStartFailed {
        command: String,
        io_error: std::io::Error,
    },

    #[error("runtime source file not found for {runtime} action {action}: {path}")]
    RuntimeSourceMissing {
        runtime: String,
        action: String,
        path: String,
    },

    #[error("process timed out: command={command}, timeout_ms={timeout_ms}")]
    ProcessTimedOut { command: String, timeout_ms: u128 },

    #[error("invalid protocol version: expected={expected}, actual={actual}")]
    InvalidProtocolVersion { expected: String, actual: String },

    #[error("process completed without emitting invocation result")]
    MissingInvocationResult,

    #[error("runtime lifecycle '{lifecycle}' is not supported")]
    UnsupportedLifecycle { lifecycle: String },

    #[error("runtime target cannot be acquired by the local runtime manager: {target}")]
    UnsupportedRuntimeTarget { target: String },

    #[error("runtime startup failed: command={command}, exit_code={exit_code:?}, stdout={stdout}, stderr={stderr}")]
    RuntimeStartupFailed {
        command: String,
        exit_code: Option<i32>,
        stdout: String,
        stderr: String,
    },

    #[error("runtime readiness timed out: endpoint={endpoint}, timeout_ms={timeout_ms}, stdout={stdout}, stderr={stderr}")]
    RuntimeReadinessTimedOut {
        endpoint: String,
        timeout_ms: u128,
        stdout: String,
        stderr: String,
    },

    #[error("runtime handle not found: {runtime_id}")]
    RuntimeHandleNotFound { runtime_id: String },

    #[error("HTTP transport error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("runtime returned HTTP status {status}: {body}")]
    HttpStatus { status: u16, body: String },

    #[error("runtime response invocation id mismatch: expected={expected}, actual={actual}")]
    InvocationIdMismatch { expected: String, actual: String },

    #[error("invocation failed ({invocation}); runtime release also failed ({release})")]
    InvocationAndRelease { invocation: String, release: String },

    #[error("HTTP transport worker panicked")]
    HttpWorkerPanicked,

    #[error("runtime invocation was cancelled: {invocation_id}")]
    RuntimeCancelled { invocation_id: String },

    #[error("runtime invocation timed out: {invocation_id}")]
    RuntimeTimedOut { invocation_id: String },

    #[error("runtime is already processing an invocation")]
    RuntimeBusy,

    #[error("runtime pool capacity was not available before the invocation deadline")]
    RuntimePoolExhausted,

    #[error("runtime manager is shutting down")]
    RuntimeUnavailable,

    #[error("cancellation is not supported for externally managed runtime {endpoint}")]
    UnsupportedCancellation { endpoint: String },
}

pub type ExecutorResult<T> = Result<T, ExecutorError>;

#[derive(Debug, Error)]
pub enum ExecutionServiceError {
    #[error("executor error: {0}")]
    Executor(#[from] ExecutorError),

    #[error("invalid execution policy: {0}")]
    InvalidPolicy(String),

    #[error("persistence error: {0}")]
    Persistence(Box<dyn std::error::Error + Send + Sync + 'static>),
}

impl From<Box<dyn std::error::Error + Send + Sync + 'static>> for ExecutionServiceError {
    fn from(error: Box<dyn std::error::Error + Send + Sync + 'static>) -> Self {
        Self::Persistence(error)
    }
}

pub type ExecutionServiceResult<T> = Result<T, ExecutionServiceError>;
