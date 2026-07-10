use std::collections::HashMap;

use ryvus_protocol::{
    ActionDefinition, ActionKind, AuthorizerParameter, AuthorizerParameterLocation,
    AuthorizerSecurity,
};
use serde_json::Value;

use super::decision::AuthorizationDecision;

pub fn parse_authorizer_decision(output: Value) -> Result<AuthorizationDecision, String> {
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

            Ok(AuthorizationDecision::Allow {
                principal_id: output
                    .get("principal_id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                context,
            })
        }
        "deny" => Ok(AuthorizationDecision::Deny {
            status: axum::http::StatusCode::FORBIDDEN,
            code: "forbidden",
            reason: output
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("forbidden")
                .to_string(),
        }),
        "unauthorized" => Ok(AuthorizationDecision::Deny {
            status: axum::http::StatusCode::UNAUTHORIZED,
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

pub fn validate_authorizer_parameters(
    authorizer: &ActionDefinition,
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

pub fn validate_authorizer_security(
    authorizer: &ActionDefinition,
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

pub fn parse_cookies(headers: &serde_json::Map<String, Value>) -> HashMap<String, String> {
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

pub fn location_name(location: &AuthorizerParameterLocation) -> &'static str {
    match location {
        AuthorizerParameterLocation::Header => "header",
        AuthorizerParameterLocation::Query => "query",
        AuthorizerParameterLocation::Cookie => "cookie",
    }
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
