use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use ryvus_execution_service::ExecutionServiceError;
use ryvus_executor::ExecutorError;
use serde_json::Value;

pub fn public_error(
    status: StatusCode,
    invocation_id: &str,
    error: &str,
    message: impl Into<String>,
    details: Option<Value>,
) -> Response {
    let mut body = serde_json::json!({
        "invocation_id": invocation_id,
        "error": error,
        "message": message.into(),
    });

    if let Some(details) = details {
        body["details"] = details;
    }

    (status, Json(body)).into_response()
}

pub fn execution_error_status(error: &ExecutionServiceError) -> StatusCode {
    match error {
        ExecutionServiceError::Executor(ExecutorError::ProcessTimedOut { .. }) => {
            StatusCode::GATEWAY_TIMEOUT
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

pub fn action_error_status(code: &str) -> StatusCode {
    if code == "TypeError" || code.ends_with("ValidationError") {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    }
}
