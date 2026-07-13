use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use ryvus_execution::{ExecutionServiceError, ExecutorError};
use ryvus_protocol::ExecutionAttempt;
use serde_json::Value;

pub fn public_error(
    status: StatusCode,
    error: &str,
    message: impl Into<String>,
    details: Option<Value>,
) -> Response {
    let mut body = serde_json::json!({
        "error": error,
        "message": message.into(),
    });

    if let Some(details) = details {
        body["details"] = details;
    }

    (status, Json(body)).into_response()
}

pub fn execution_error(
    status: StatusCode,
    attempt: &ExecutionAttempt,
    error: &str,
    message: impl Into<String>,
    details: Option<Value>,
) -> Response {
    let mut body = serde_json::json!({
        "execution_id": attempt.execution_id,
        "attempt_id": attempt.attempt_id,
        "attempt_number": attempt.attempt_number,
        "error": error,
        "message": message.into(),
    });

    if let Some(details) = details {
        body["details"] = details;
    }

    (status, Json(body)).into_response()
}

pub fn error_attempt(error: &ExecutionServiceError) -> Option<&ExecutionAttempt> {
    match error {
        ExecutionServiceError::Executor(
            ExecutorError::ProcessTimedOut { attempt, .. }
            | ExecutorError::RuntimeCancelled { attempt }
            | ExecutorError::RuntimeTimedOut { attempt }
            | ExecutorError::RuntimeReadinessTimedOut { attempt, .. },
        ) => Some(attempt),
        _ => None,
    }
}

pub fn execution_error_status(error: &ExecutionServiceError) -> StatusCode {
    match error {
        ExecutionServiceError::Executor(
            ExecutorError::ProcessTimedOut { .. }
            | ExecutorError::RuntimeReadinessTimedOut { .. }
            | ExecutorError::RuntimeTimedOut { .. },
        ) => StatusCode::GATEWAY_TIMEOUT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

pub fn action_error_status(code: &str) -> StatusCode {
    if code == "Timeout" {
        StatusCode::GATEWAY_TIMEOUT
    } else if code == "TypeError" || code.ends_with("ValidationError") {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    }
}
