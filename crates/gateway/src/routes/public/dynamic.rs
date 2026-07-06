use axum::{
    body::{to_bytes, Body},
    extract::State,
    http::{header::CONTENT_TYPE, Request, StatusCode},
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
    errors::{action_error_status, execution_error_status, invocation_error, public_error},
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

    let route_match = match state
        .control_service
        .route_registry()
        .resolve(method.as_str(), &path)
    {
        Some(route_match) => route_match,
        None => {
            if state.control_service.route_registry().path_exists(&path) {
                return public_error(
                    StatusCode::METHOD_NOT_ALLOWED,
                    "method_not_allowed",
                    format!("{} is not allowed for {}", method, path),
                    None,
                );
            }

            return public_error(
                StatusCode::NOT_FOUND,
                "route_not_configured",
                format!("{} {} is not configured", method, path),
                None,
            );
        }
    };
    let route = route_match.definition;

    let action = match state.control_service.resolve_action(&route.action) {
        Ok(action) => action,
        Err(error) => {
            return public_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "action_not_found",
                format!("{}: {}", route.action, error),
                None,
            );
        }
    };

    let content_type = request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(normalize_media_type);

    let body = match to_bytes(request.into_body(), usize::MAX).await {
        Ok(body) => body,
        Err(error) => {
            return public_error(
                StatusCode::BAD_REQUEST,
                "failed_to_read_request_body",
                error.to_string(),
                None,
            );
        }
    };

    let (input, request_media_type) = if let ActionKind::Api(api) = &action.kind {
        match parse_request_body(api, content_type.as_deref(), &body) {
            Ok(parsed) => parsed,
            Err(RequestBodyError::UnsupportedMediaType(media_type)) => {
                return public_error(
                    StatusCode::UNSUPPORTED_MEDIA_TYPE,
                    "unsupported_media_type",
                    format!("{} is not supported for {}", media_type, route.action),
                    None,
                );
            }
            Err(RequestBodyError::InvalidBody { code, message }) => {
                return public_error(StatusCode::BAD_REQUEST, code, message, None);
            }
        }
    } else {
        (Value::Null, "application/json".to_string())
    };

    if let ActionKind::Api(api) = &action.kind {
        if let Err(error) = validate_request(
            api,
            &route_match.path_params,
            &query_params,
            &input,
            &request_media_type,
        ) {
            return public_error(
                StatusCode::BAD_REQUEST,
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

    let policy = match ryvus_execution::ExecutionPolicy::from_action_policy(&action.policy) {
        Ok(policy) => policy,
        Err(error) => {
            return public_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "invalid_execution_policy",
                error.to_string(),
                None,
            );
        }
    };

    let record = match state.execution_service.execute(action, &request, &policy) {
        Ok(record) => record,
        Err(error) => {
            let status = execution_error_status(&error);

            return invocation_error(
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

        return invocation_error(
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

    if let ActionKind::Api(api) = &action.kind {
        return encode_response(api, output);
    }

    Json(output).into_response()
}

enum RequestBodyError {
    UnsupportedMediaType(String),
    InvalidBody { code: &'static str, message: String },
}

fn parse_request_body(
    api: &ryvus_protocol::ApiAction,
    content_type: Option<&str>,
    body: &[u8],
) -> Result<(Value, String), RequestBodyError> {
    let media_type = content_type
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| first_media_type(&api.consumes));

    if !api.consumes.iter().any(|declared| declared == &media_type) {
        return Err(RequestBodyError::UnsupportedMediaType(media_type));
    }

    match media_type.as_str() {
        "application/json" if body.is_empty() && api.request_schema.is_none() => {
            Ok((Value::Null, media_type))
        }
        _ if body.is_empty() => Err(RequestBodyError::InvalidBody {
            code: "invalid_request_body",
            message: "request body is required".to_string(),
        }),
        "application/json" => serde_json::from_slice(body)
            .map(|value| (value, media_type))
            .map_err(|error| RequestBodyError::InvalidBody {
                code: "invalid_json_body",
                message: error.to_string(),
            }),
        "text/plain" => String::from_utf8(body.to_vec())
            .map(|value| (Value::String(value), media_type))
            .map_err(|error| RequestBodyError::InvalidBody {
                code: "invalid_request_body",
                message: error.to_string(),
            }),
        "application/x-www-form-urlencoded" => {
            let fields = url::form_urlencoded::parse(body)
                .into_owned()
                .map(|(name, value)| (name, Value::String(value)))
                .collect();
            Ok((Value::Object(fields), media_type))
        }
        _ => Err(RequestBodyError::UnsupportedMediaType(media_type)),
    }
}

fn encode_response(api: &ryvus_protocol::ApiAction, output: Value) -> Response {
    match first_media_type(&api.produces).as_str() {
        "text/plain" => match output {
            Value::String(value) => {
                ([(CONTENT_TYPE, "text/plain; charset=utf-8")], value).into_response()
            }
            value => (
                [(CONTENT_TYPE, "text/plain; charset=utf-8")],
                value.to_string(),
            )
                .into_response(),
        },
        _ => Json(output).into_response(),
    }
}

fn normalize_media_type(value: &str) -> String {
    value
        .split(';')
        .next()
        .unwrap_or(value)
        .trim()
        .to_ascii_lowercase()
}

fn first_media_type(values: &[String]) -> String {
    values
        .first()
        .cloned()
        .unwrap_or_else(|| "application/json".to_string())
}
