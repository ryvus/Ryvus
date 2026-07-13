use axum::{http::StatusCode, response::IntoResponse, Json};
use ryvus_protocol::ExecutionAttempt;
use serde::Serialize;
use thiserror::Error;

use crate::WorkerError;

#[derive(Debug, Error)]
pub enum RuntimeHostError {
    #[error("unsupported invocation protocol version '{actual}'")]
    InvalidProtocolVersion { actual: String },
    #[error("invalid invocation identity: {0}")]
    InvalidIdentity(String),
    #[error("invocation deadline has expired")]
    DeadlineExpired,
    #[error("invocation sender budget is zero")]
    EmptyDeadlineBudget,
    #[error("runtime host clock is behind the sender beyond the allowed tolerance")]
    ClockSkew,
    #[error("runtime host is not accepting invocations")]
    Unavailable,
    #[error("runtime host is already processing an invocation")]
    Busy,
    #[error("worker error: {0}")]
    Worker(#[from] WorkerError),
    #[error("worker supervision task failed: {0}")]
    Supervision(#[from] tokio::task::JoinError),
    #[error("invocation timed out")]
    TimedOut,
    #[error("runtime response attempt mismatch: expected=({expected}), actual=({actual})")]
    AttemptMismatch {
        expected: ExecutionAttempt,
        actual: ExecutionAttempt,
    },
    #[error("runtime returned protocol version '{actual}'")]
    WorkerProtocolMismatch { actual: String },
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
}

impl IntoResponse for RuntimeHostError {
    fn into_response(self) -> axum::response::Response {
        let (status, code) = match &self {
            Self::InvalidProtocolVersion { .. } | Self::InvalidIdentity(_) | Self::ClockSkew => {
                (StatusCode::BAD_REQUEST, "RUNTIME_PROTOCOL_ERROR")
            }
            Self::DeadlineExpired | Self::EmptyDeadlineBudget => {
                (StatusCode::REQUEST_TIMEOUT, "RUNTIME_DEADLINE_EXPIRED")
            }
            Self::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "RUNTIME_UNAVAILABLE"),
            Self::Busy => (StatusCode::CONFLICT, "RUNTIME_BUSY"),
            Self::TimedOut => (StatusCode::GATEWAY_TIMEOUT, "RUNTIME_TIMED_OUT"),
            Self::Worker(_) | Self::Supervision(_) => {
                (StatusCode::BAD_GATEWAY, "RUNTIME_WORKER_ERROR")
            }
            Self::AttemptMismatch { .. } | Self::WorkerProtocolMismatch { .. } => {
                (StatusCode::BAD_GATEWAY, "RUNTIME_PROTOCOL_ERROR")
            }
        };
        let message = self.to_string();
        (status, Json(ErrorBody { code, message })).into_response()
    }
}
