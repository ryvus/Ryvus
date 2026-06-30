use axum::{
    body::{to_bytes, Body},
    extract::State,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use std::collections::HashMap;

use ryvus_execution_service::ExecutionServiceError;
use ryvus_executor::ExecutorError;
use ryvus_protocol::{ActionKind, ApiAction, ApiQueryParam, InvocationStatus};
use serde_json::Value;

use crate::state::AppState;

pub async fn handle_dynamic_route(
    State(state): State<AppState>,
    request: Request<Body>,
) -> Response {
    let method = request.method().clone();
    let uri = request.uri().clone();
    let path = uri.path().to_string();

    let query_params: HashMap<String, String> =
        url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();

    let route_match = match state.route_registry.resolve(&method, &path) {
        Some(route_match) => route_match,
        None => {
            if state.route_registry.path_exists(&path) {
                return (
                    StatusCode::METHOD_NOT_ALLOWED,
                    Json(serde_json::json!({
                        "error": "method_not_allowed",
                        "method": method.to_string(),
                        "path": path,
                    })),
                )
                    .into_response();
            }

            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": "route_not_configured",
                    "method": method.to_string(),
                    "path": path,
                })),
            )
                .into_response();
        }
    };
    let route = route_match.definition;

    let action = match state.action_service.resolve_action(&route.action) {
        Ok(action) => action,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "action_not_found",
                    "action": route.action,
                    "message": error.to_string(),
                })),
            )
                .into_response();
        }
    };

    let body = match to_bytes(request.into_body(), usize::MAX).await {
        Ok(body) => body,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "failed_to_read_request_body",
                    "message": error.to_string(),
                })),
            )
                .into_response();
        }
    };

    let input = if body.is_empty() {
        Value::Null
    } else {
        match serde_json::from_slice(&body) {
            Ok(value) => value,
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({
                        "error": "invalid_json_body",
                        "message": error.to_string(),
                    })),
                )
                    .into_response();
            }
        }
    };

    if let ActionKind::Api(api) = &action.kind {
        if let Err(error) = validate_request(api, &query_params, &input) {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "request_validation_failed",
                    "message": error,
                })),
            )
                .into_response();
        }
    }

    let event = serde_json::json!({
        "body": input,
        "path_params": route_match.path_params,
        "query_params": query_params,
    });

    let record = match state.execution_service.execute_event(action, event) {
        Ok(record) => record,
        Err(error) => {
            let status = execution_error_status(&error);

            return (
                status,
                Json(serde_json::json!({
                    "error": "execution_failed",
                    "action": route.action,
                    "message": error.to_string(),
                })),
            )
                .into_response();
        }
    };

    let invocation_result = record.result.invocation_result;

    if invocation_result.status != InvocationStatus::Success {
        let status = invocation_result
            .error
            .as_ref()
            .map(|error| action_error_status(&error.code))
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

        return (
            status,
            Json(serde_json::json!({
                "error": "action_failed",
                "action": route.action,
                "details": invocation_result.error,
            })),
        )
            .into_response();
    }

    let output = invocation_result.output.unwrap_or(serde_json::Value::Null);

    Json(output).into_response()
}

fn execution_error_status(error: &ExecutionServiceError) -> StatusCode {
    match error {
        ExecutionServiceError::Executor(ExecutorError::ProcessTimedOut { .. }) => {
            StatusCode::GATEWAY_TIMEOUT
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn action_error_status(code: &str) -> StatusCode {
    if code == "TypeError" || code.ends_with("ValidationError") {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    }
}

fn validate_request(
    api: &ApiAction,
    query_params: &HashMap<String, String>,
    body: &Value,
) -> Result<(), String> {
    validate_query_params(&api.query_params, query_params)?;

    if method_allows_request_body(&api.method) {
        if let Some(schema) = &api.request_schema {
            validate_body(schema, body)?;
        }
    }

    Ok(())
}

fn validate_query_params(
    expected: &[ApiQueryParam],
    actual: &HashMap<String, String>,
) -> Result<(), String> {
    for param in expected {
        let Some(value) = actual.get(&param.name) else {
            if param.required {
                return Err(format!("missing required query parameter `{}`", param.name));
            }

            continue;
        };

        validate_query_param(&param.name, value, &param.schema)?;
    }

    Ok(())
}

fn validate_query_param(name: &str, value: &str, schema: &Value) -> Result<(), String> {
    match schema.get("type").and_then(Value::as_str) {
        Some("string") => Ok(()),
        Some("integer") => value
            .parse::<i64>()
            .map(|_| ())
            .map_err(|_| format!("query parameter `{name}` must be an integer")),
        Some("number") => value
            .parse::<f64>()
            .map(|_| ())
            .map_err(|_| format!("query parameter `{name}` must be a number")),
        Some("boolean") => validate_bool(value)
            .then_some(())
            .ok_or_else(|| format!("query parameter `{name}` must be a boolean")),
        _ => Ok(()),
    }
}

fn validate_body(schema: &Value, body: &Value) -> Result<(), String> {
    let validator = jsonschema::validator_for(schema)
        .map_err(|error| format!("invalid request schema: {error}"))?;

    if validator.is_valid(body) {
        return Ok(());
    }

    let errors = validator
        .iter_errors(body)
        .map(|error| error.to_string())
        .collect::<Vec<_>>();

    Err(errors.join("; "))
}

fn validate_bool(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "true" | "false" | "1" | "0" | "yes" | "no" | "on" | "off"
    )
}

fn method_allows_request_body(method: &str) -> bool {
    matches!(
        method.to_ascii_uppercase().as_str(),
        "POST" | "PUT" | "PATCH"
    )
}
