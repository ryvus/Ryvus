use std::collections::HashMap;

use ryvus_protocol::{ApiAction, ApiQueryParam};
use serde_json::Value;

pub fn validate_request(
    api: &ApiAction,
    path_params: &HashMap<String, String>,
    query_params: &HashMap<String, String>,
    body: &Value,
    media_type: &str,
) -> Result<(), String> {
    validate_path_params(path_params)?;
    validate_query_params(&api.query_params, query_params)?;

    if method_allows_request_body(&api.method) && media_type != "text/plain" {
        if let Some(schema) = &api.request_schema {
            validate_body(schema, body)?;
        }
    }

    Ok(())
}

fn validate_path_params(path_params: &HashMap<String, String>) -> Result<(), String> {
    for (name, value) in path_params {
        if is_blank(value) {
            return Err(format!("path parameter `{name}` cannot be empty"));
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

        if param.required && is_blank(value) {
            return Err(format!("query parameter `{}` cannot be empty", param.name));
        }

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

fn is_blank(value: &str) -> bool {
    if value.trim().is_empty() {
        return true;
    }

    let encoded = format!("value={value}");
    url::form_urlencoded::parse(encoded.as_bytes())
        .next()
        .is_some_and(|(_, decoded)| decoded.trim().is_empty())
}

fn method_allows_request_body(method: &str) -> bool {
    matches!(
        method.to_ascii_uppercase().as_str(),
        "POST" | "PUT" | "PATCH"
    )
}
