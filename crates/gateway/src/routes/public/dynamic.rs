use axum::{
    body::{to_bytes, Body},
    extract::State,
    http::{header::CONTENT_TYPE, HeaderMap, Request, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use std::{collections::HashMap, time::Duration};

use crate::cache::AuthorizerCacheKey;
use ryvus_protocol::{
    ActionKind, AuthorizerParameter, AuthorizerParameterLocation, AuthorizerSecurity,
    InvocationContext, InvocationRequest, InvocationStatus, PROTOCOL_VERSION,
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
    let ActionKind::Api(api) = &action.kind else {
        return public_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "route_action_not_api",
            format!("{} is not an Api action", route.action),
            None,
        );
    };

    let content_type = request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(normalize_media_type);
    let headers = request_headers(request.headers());

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

    let (input, request_media_type) = match parse_request_body(api, content_type.as_deref(), &body)
    {
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
    };

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

    let mut context = InvocationContext::default();

    if let Some(authorizer_name) = &api.authorizer {
        let authorizer = match state.control_service.resolve_authorizer(authorizer_name) {
            Ok(authorizer) => authorizer,
            Err(error) => {
                return public_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "authorizer_not_found",
                    format!("{}: {}", authorizer_name, error),
                    None,
                );
            }
        };

        if let Err(error) = validate_authorizer_security(authorizer, &headers, &query_params) {
            return public_error(StatusCode::UNAUTHORIZED, "unauthorized", error, None);
        }

        if let Err(error) = validate_authorizer_parameters(authorizer, &headers, &query_params) {
            return public_error(
                StatusCode::BAD_REQUEST,
                "request_validation_failed",
                error,
                None,
            );
        }

        let authorizer_event = serde_json::json!({
            "body": input,
            "path_params": route_match.path_params,
            "query_params": query_params,
            "headers": headers,
            "method": method.as_str(),
            "path": path,
        });

        let cache_key = authorizer_cache_key(authorizer, authorizer_name, &headers, &query_params);
        if let Some(key) = cache_key.as_ref() {
            if let Some(AuthorizerDecision::Allow {
                principal_id,
                context: authorizer_context,
            }) = state.authorizer_cache.get(key)
            {
                context.metadata = serde_json::json!({
                    "authorizer": {
                        "name": authorizer_name,
                        "principal_id": principal_id,
                        "context": authorizer_context,
                    }
                });
                return invoke_api_action(
                    &state,
                    action,
                    &route.action,
                    api,
                    invocation_id,
                    input,
                    route_match.path_params,
                    query_params,
                    context,
                );
            }
        }

        match execute_authorizer(&state, authorizer, authorizer_name, authorizer_event) {
            Ok(AuthorizerDecision::Allow {
                principal_id,
                context: authorizer_context,
            }) => {
                if let Some((key, ttl)) =
                    cache_key.and_then(|key| authorizer_cache_ttl(authorizer).map(|ttl| (key, ttl)))
                {
                    state.authorizer_cache.put(
                        key,
                        AuthorizerDecision::Allow {
                            principal_id: principal_id.clone(),
                            context: authorizer_context.clone(),
                        },
                        ttl,
                    );
                }

                context.metadata = serde_json::json!({
                    "authorizer": {
                        "name": authorizer_name,
                        "principal_id": principal_id,
                        "context": authorizer_context,
                    }
                });
            }
            Ok(AuthorizerDecision::Deny {
                status,
                code,
                reason,
            }) => {
                return public_error(status, code, reason, None);
            }
            Err(error) => {
                return public_error(error.status, "authorizer_failed", error.message, None);
            }
        }
    }

    invoke_api_action(
        &state,
        action,
        &route.action,
        api,
        invocation_id,
        input,
        route_match.path_params,
        query_params,
        context,
    )
}

fn invoke_api_action(
    state: &AppState,
    action: &ryvus_protocol::ActionDefinition,
    action_name: &str,
    api: &ryvus_protocol::ApiAction,
    invocation_id: String,
    input: Value,
    path_params: HashMap<String, String>,
    query_params: HashMap<String, String>,
    context: InvocationContext,
) -> Response {
    let event = serde_json::json!({
        "body": input,
        "path_params": path_params,
        "query_params": query_params,
    });

    let request = InvocationRequest {
        protocol_version: PROTOCOL_VERSION.to_string(),
        invocation_id: invocation_id.clone(),
        event,
        context,
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
                format!("{}: {}", action_name, error),
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

    encode_response(api, output)
}

#[derive(Clone)]
pub enum AuthorizerDecision {
    Allow {
        principal_id: Option<String>,
        context: serde_json::Map<String, Value>,
    },
    Deny {
        status: StatusCode,
        code: &'static str,
        reason: String,
    },
}

fn authorizer_cache_ttl(authorizer: &ryvus_protocol::ActionDefinition) -> Option<Duration> {
    let ActionKind::Authorizer(authorizer) = &authorizer.kind else {
        return None;
    };

    authorizer
        .cache
        .as_ref()
        .filter(|cache| cache.ttl_seconds > 0)
        .map(|cache| Duration::from_secs(cache.ttl_seconds))
}

fn authorizer_cache_key(
    authorizer: &ryvus_protocol::ActionDefinition,
    authorizer_name: &str,
    headers: &serde_json::Map<String, Value>,
    query_params: &HashMap<String, String>,
) -> Option<AuthorizerCacheKey> {
    authorizer_cache_ttl(authorizer)?;

    let ActionKind::Authorizer(authorizer) = &authorizer.kind else {
        return None;
    };

    if authorizer.security.is_empty() && authorizer.parameters.is_empty() {
        return None;
    }

    let cookies = parse_cookies(headers);
    let mut parts = vec![("authorizer".to_string(), authorizer_name.to_string())];

    for security in &authorizer.security {
        if security.security_type == "http"
            && security
                .scheme
                .as_deref()
                .is_some_and(|scheme| scheme.eq_ignore_ascii_case("bearer"))
        {
            parts.push((
                "security:header:authorization".to_string(),
                headers
                    .get("authorization")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            ));
        } else if security.security_type == "apiKey" {
            let Some(name) = security.name.as_ref() else {
                continue;
            };
            parts.push((
                format!(
                    "security:{}:{}",
                    security
                        .location
                        .as_ref()
                        .map(authorizer_location_name)
                        .unwrap_or("unknown"),
                    name.to_ascii_lowercase()
                ),
                value_from_location(
                    security.location.as_ref(),
                    name,
                    headers,
                    query_params,
                    &cookies,
                ),
            ));
        }
    }

    for parameter in &authorizer.parameters {
        parts.push((
            format!(
                "parameter:{}:{}",
                authorizer_location_name(&parameter.location),
                parameter.name.to_ascii_lowercase()
            ),
            value_from_location(
                Some(&parameter.location),
                &parameter.name,
                headers,
                query_params,
                &cookies,
            ),
        ));
    }

    Some(AuthorizerCacheKey(parts))
}

fn authorizer_location_name(location: &AuthorizerParameterLocation) -> &'static str {
    match location {
        AuthorizerParameterLocation::Header => "header",
        AuthorizerParameterLocation::Query => "query",
        AuthorizerParameterLocation::Cookie => "cookie",
    }
}

fn value_from_location(
    location: Option<&AuthorizerParameterLocation>,
    name: &str,
    headers: &serde_json::Map<String, Value>,
    query_params: &HashMap<String, String>,
    cookies: &HashMap<String, String>,
) -> String {
    match location {
        Some(AuthorizerParameterLocation::Header) => headers
            .get(&name.to_ascii_lowercase())
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        Some(AuthorizerParameterLocation::Query) => {
            query_params.get(name).cloned().unwrap_or_default()
        }
        Some(AuthorizerParameterLocation::Cookie) => cookies.get(name).cloned().unwrap_or_default(),
        None => String::new(),
    }
}

struct AuthorizerFailure {
    status: StatusCode,
    message: String,
}

enum RequestBodyError {
    UnsupportedMediaType(String),
    InvalidBody { code: &'static str, message: String },
}

fn execute_authorizer(
    state: &AppState,
    authorizer: &ryvus_protocol::ActionDefinition,
    authorizer_name: &str,
    event: Value,
) -> Result<AuthorizerDecision, AuthorizerFailure> {
    let request = InvocationRequest {
        protocol_version: PROTOCOL_VERSION.to_string(),
        invocation_id: InvocationRequest::new(Value::Null).invocation_id,
        event,
        context: InvocationContext::default(),
    };

    let policy = ryvus_execution::ExecutionPolicy::from_action_policy(&authorizer.policy).map_err(
        |error| AuthorizerFailure {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: error.to_string(),
        },
    )?;

    let record = state
        .execution_service
        .execute(authorizer, &request, &policy)
        .map_err(|error| AuthorizerFailure {
            status: execution_error_status(&error),
            message: error.to_string(),
        })?;

    let result = record.result.invocation_result;

    if result.status != InvocationStatus::Success {
        return Err(match result.error {
            Some(error) => AuthorizerFailure {
                status: if error.code == "Timeout" {
                    action_error_status(&error.code)
                } else {
                    StatusCode::INTERNAL_SERVER_ERROR
                },
                message: error.message,
            },
            None => AuthorizerFailure {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                message: format!("authorizer `{authorizer_name}` failed"),
            },
        });
    }

    parse_authorizer_decision(result.output.unwrap_or(Value::Null)).map_err(|message| {
        AuthorizerFailure {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message,
        }
    })
}

fn parse_authorizer_decision(output: Value) -> Result<AuthorizerDecision, String> {
    let effect = output
        .get("effect")
        .and_then(Value::as_str)
        .ok_or_else(|| "authorizer output requires string effect".to_string())?;

    match effect {
        "allow" => {
            let context = match output.get("context") {
                Some(Value::Object(context)) => context.clone(),
                Some(_) => {
                    return Err("authorizer context must be an object".to_string());
                }
                None => serde_json::Map::new(),
            };

            Ok(AuthorizerDecision::Allow {
                principal_id: output
                    .get("principal_id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                context,
            })
        }
        "deny" => Ok(AuthorizerDecision::Deny {
            status: StatusCode::FORBIDDEN,
            code: "forbidden",
            reason: output
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("forbidden")
                .to_string(),
        }),
        "unauthorized" => Ok(AuthorizerDecision::Deny {
            status: StatusCode::UNAUTHORIZED,
            code: "unauthorized",
            reason: output
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("unauthorized")
                .to_string(),
        }),
        other => Err(format!("unsupported authorizer effect `{other}`")),
    }
}

fn request_headers(headers: &HeaderMap) -> serde_json::Map<String, Value> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            value.to_str().ok().map(|value| {
                (
                    name.as_str().to_ascii_lowercase(),
                    Value::String(value.to_string()),
                )
            })
        })
        .collect()
}

fn validate_authorizer_parameters(
    authorizer: &ryvus_protocol::ActionDefinition,
    headers: &serde_json::Map<String, Value>,
    query_params: &HashMap<String, String>,
) -> Result<(), String> {
    let ActionKind::Authorizer(authorizer) = &authorizer.kind else {
        return Ok(());
    };

    let cookies = parse_cookies(headers);

    for parameter in &authorizer.parameters {
        if !parameter.required {
            continue;
        }

        if !authorizer_parameter_exists(parameter, headers, query_params, &cookies) {
            return Err(format!(
                "required authorizer parameter `{}` is missing",
                parameter.name
            ));
        }
    }

    Ok(())
}

fn validate_authorizer_security(
    authorizer: &ryvus_protocol::ActionDefinition,
    headers: &serde_json::Map<String, Value>,
    query_params: &HashMap<String, String>,
) -> Result<(), String> {
    let ActionKind::Authorizer(authorizer) = &authorizer.kind else {
        return Ok(());
    };

    if authorizer.security.is_empty() {
        return Ok(());
    }

    let cookies = parse_cookies(headers);

    if authorizer
        .security
        .iter()
        .any(|security| authorizer_security_exists(security, headers, query_params, &cookies))
    {
        return Ok(());
    }

    Err("authorizer security credentials are required".to_string())
}

fn authorizer_security_exists(
    security: &AuthorizerSecurity,
    headers: &serde_json::Map<String, Value>,
    query_params: &HashMap<String, String>,
    cookies: &HashMap<String, String>,
) -> bool {
    if security.security_type == "http"
        && security
            .scheme
            .as_deref()
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("bearer"))
    {
        return headers
            .get("authorization")
            .and_then(Value::as_str)
            .is_some_and(|value| value.trim().starts_with("Bearer "));
    }

    if security.security_type == "apiKey" {
        let Some(name) = security.name.as_ref() else {
            return false;
        };

        return match security.location {
            Some(AuthorizerParameterLocation::Header) => headers
                .get(&name.to_ascii_lowercase())
                .and_then(Value::as_str)
                .is_some_and(|value| !value.trim().is_empty()),
            Some(AuthorizerParameterLocation::Query) => query_params
                .get(name)
                .is_some_and(|value| !value.trim().is_empty()),
            Some(AuthorizerParameterLocation::Cookie) => cookies
                .get(name)
                .is_some_and(|value| !value.trim().is_empty()),
            None => false,
        };
    }

    false
}

fn authorizer_parameter_exists(
    parameter: &AuthorizerParameter,
    headers: &serde_json::Map<String, Value>,
    query_params: &HashMap<String, String>,
    cookies: &HashMap<String, String>,
) -> bool {
    match parameter.location {
        AuthorizerParameterLocation::Header => headers
            .get(&parameter.name.to_ascii_lowercase())
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty()),
        AuthorizerParameterLocation::Query => query_params
            .get(&parameter.name)
            .is_some_and(|value| !value.is_empty()),
        AuthorizerParameterLocation::Cookie => cookies
            .get(&parameter.name)
            .is_some_and(|value| !value.is_empty()),
    }
}

fn parse_cookies(headers: &serde_json::Map<String, Value>) -> HashMap<String, String> {
    headers
        .get("cookie")
        .and_then(Value::as_str)
        .unwrap_or("")
        .split(';')
        .filter_map(|part| {
            let (name, value) = part.trim().split_once('=')?;
            Some((name.to_string(), value.to_string()))
        })
        .collect()
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
