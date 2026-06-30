use axum::{
    body::{to_bytes, Body},
    extract::State,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use std::collections::HashMap;

use ryvus_protocol::{
    ActionKind, InvocationContext, InvocationRequest, InvocationStatus, PROTOCOL_VERSION,
};
use serde_json::Value;

use crate::state::AppState;

use super::{
    errors::{action_error_status, execution_error_status, public_error},
    validation::validate_request,
};

pub async fn handle_dynamic_route(
    State(state): State<AppState>,
    request: Request<Body>,
) -> Response {
    let invocation_id = InvocationRequest::new(Value::Null).invocation_id;
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
                return public_error(
                    StatusCode::METHOD_NOT_ALLOWED,
                    &invocation_id,
                    "method_not_allowed",
                    format!("{} is not allowed for {}", method, path),
                    None,
                );
            }

            return public_error(
                StatusCode::NOT_FOUND,
                &invocation_id,
                "route_not_configured",
                format!("{} {} is not configured", method, path),
                None,
            );
        }
    };
    let route = route_match.definition;

    let action = match state.action_service.resolve_action(&route.action) {
        Ok(action) => action,
        Err(error) => {
            return public_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &invocation_id,
                "action_not_found",
                format!("{}: {}", route.action, error),
                None,
            );
        }
    };

    let body = match to_bytes(request.into_body(), usize::MAX).await {
        Ok(body) => body,
        Err(error) => {
            return public_error(
                StatusCode::BAD_REQUEST,
                &invocation_id,
                "failed_to_read_request_body",
                error.to_string(),
                None,
            );
        }
    };

    let input = if body.is_empty() {
        Value::Null
    } else {
        match serde_json::from_slice(&body) {
            Ok(value) => value,
            Err(error) => {
                return public_error(
                    StatusCode::BAD_REQUEST,
                    &invocation_id,
                    "invalid_json_body",
                    error.to_string(),
                    None,
                );
            }
        }
    };

    if let ActionKind::Api(api) = &action.kind {
        if let Err(error) = validate_request(api, &query_params, &input) {
            return public_error(
                StatusCode::BAD_REQUEST,
                &invocation_id,
                "request_validation_failed",
                error,
                None,
            );
        }
    }

    let event = serde_json::json!({
        "body": input,
        "path_params": route_match.path_params,
        "query_params": query_params,
    });

    let request = InvocationRequest {
        protocol_version: PROTOCOL_VERSION.to_string(),
        invocation_id: invocation_id.clone(),
        event,
        context: InvocationContext::default(),
    };

    let record = match state.execution_service.execute(action, &request) {
        Ok(record) => record,
        Err(error) => {
            let status = execution_error_status(&error);

            return public_error(
                status,
                &invocation_id,
                "execution_failed",
                format!("{}: {}", route.action, error),
                None,
            );
        }
    };

    let invocation_result = record.result.invocation_result;

    if invocation_result.status != InvocationStatus::Success {
        let status = invocation_result
            .error
            .as_ref()
            .map(|error| action_error_status(&error.code))
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

        return public_error(
            status,
            &invocation_id,
            "action_failed",
            invocation_result
                .error
                .as_ref()
                .map(|error| error.message.clone())
                .unwrap_or_else(|| "action failed".to_string()),
            invocation_result
                .error
                .and_then(|error| serde_json::to_value(error).ok()),
        );
    }

    let output = invocation_result.output.unwrap_or(serde_json::Value::Null);

    Json(output).into_response()
}
