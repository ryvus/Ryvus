use std::collections::HashMap;

use ryvus_protocol::{ApiAction, ApiQueryParam};
use serde_json::Value;

pub fn validate_request(
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
