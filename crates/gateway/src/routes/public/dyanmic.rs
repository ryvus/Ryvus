use axum::{
    body::{to_bytes, Body},
    extract::State,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
    Json,
};

use crate::state::AppState;

pub async fn handle_dynamic_route(
    State(state): State<AppState>,
    request: Request<Body>,
) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();

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

    let route_match = match state.route_registry.resolve(&method, &path) {
        Some(route_match) => route_match,
        None => {
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
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": "action_not_found",
                    "action": route.action,
                    "message": error.to_string(),
                })),
            )
                .into_response();
        }
    };

    let input = if body.is_empty() {
        serde_json::Value::Null
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
    let event = serde_json::json!({
        "body": input,
        "path_params": route_match.path_params,
        "query_params": {},
    });

    let record = match state.execution_service.execute_event(action, event) {
        Ok(record) => record,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "execution_failed",
                    "action": route.action,
                    "message": error.to_string(),
                })),
            )
                .into_response();
        }
    };

    let output = record
        .result
        .invocation_result
        .output
        .unwrap_or(serde_json::Value::Null);

    Json(output).into_response()
}
