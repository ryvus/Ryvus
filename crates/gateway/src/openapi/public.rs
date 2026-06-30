use serde_json::{json, Value};
use utoipa::openapi::OpenApi;

use ryvus_protocol::{ActionDefinition, ActionKind, ApiQueryParam};

use crate::config::routes::{GatewayConfig, HttpMethod};

pub fn build_public_openapi_json_from_actions<'a>(
    actions: impl IntoIterator<Item = &'a ActionDefinition>,
) -> Value {
    let mut paths = serde_json::Map::new();

    for action in actions {
        let ActionKind::Api(api) = &action.kind else {
            continue;
        };

        let method = api.method.to_lowercase();
        let action_key = action_key(action);
        let operation_id = operation_id(action, &api.method, &api.path);

        let path_item = paths.entry(api.path.clone()).or_insert_with(|| json!({}));

        let path_object = path_item
            .as_object_mut()
            .expect("path item should be an object");

        path_object.insert(
            method,
            build_operation(
                &operation_id,
                &action.entrypoint,
                &format!("Routes to Ryvus action '{}'.", action_key),
                &api.path,
                &api.method,
                api.request_schema.as_ref(),
                api.response_schema.as_ref(),
                &api.query_params,
                "Successful response",
                false,
            ),
        );
    }

    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Ryvus Public API",
            "version": "1.0.0"
        },
        "paths": paths
    })
}

pub fn build_openapi_json_from_actions<'a>(
    actions: impl IntoIterator<Item = &'a ActionDefinition>,
    openapi: OpenApi,
) -> Value {
    let mut value = serde_json::to_value(openapi).expect("failed to serialize OpenAPI document");

    let paths = value
        .get_mut("paths")
        .and_then(Value::as_object_mut)
        .expect("OpenAPI document should contain paths object");

    for action in actions {
        let ActionKind::Api(api) = &action.kind else {
            continue;
        };

        let method = api.method.to_lowercase();
        let action_key = action_key(action);
        let operation_id = operation_id(action, &api.method, &api.path);

        let path_item = paths.entry(api.path.clone()).or_insert_with(|| json!({}));

        let path_object = path_item
            .as_object_mut()
            .expect("OpenAPI path item should be an object");

        path_object.insert(
            method,
            build_operation(
                &operation_id,
                &action.entrypoint,
                &format!("Routes to Ryvus action `{}`.", action_key),
                &api.path,
                &api.method,
                api.request_schema.as_ref(),
                api.response_schema.as_ref(),
                &api.query_params,
                "Route matched",
                true,
            ),
        );
    }

    value
}

pub fn build_public_openapi_json(config: &GatewayConfig) -> Value {
    let mut paths = serde_json::Map::new();

    for route in &config.routes {
        let method = method_to_openapi_key(route.method);

        let path_item = paths.entry(route.path.clone()).or_insert_with(|| json!({}));

        let path_object = path_item
            .as_object_mut()
            .expect("path item should be an object");

        path_object.insert(
            method.to_string(),
            build_operation(
                &route.name,
                &route.name,
                &format!("Routes to Ryvus action '{}'.", route.action),
                &route.path,
                method,
                None,
                None,
                &[],
                "Successful response",
                false,
            ),
        );
    }

    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Ryvus Public API",
            "version": "1.0.0"
        },
        "paths": paths
    })
}

pub fn build_openapi_json(config: &GatewayConfig, openapi: OpenApi) -> Value {
    let mut value = serde_json::to_value(openapi).expect("failed to serialize OpenAPI document");

    let paths = value
        .get_mut("paths")
        .and_then(Value::as_object_mut)
        .expect("OpenAPI document should contain paths object");

    for route in &config.routes {
        let method = method_to_openapi_key(route.method);

        let path_item = paths.entry(route.path.clone()).or_insert_with(|| json!({}));

        let path_object = path_item
            .as_object_mut()
            .expect("OpenAPI path item should be an object");

        path_object.insert(
            method.to_string(),
            build_operation(
                &route.name,
                &route.name,
                &format!("Routes to Ryvus action `{}`.", route.action),
                &route.path,
                method,
                None,
                None,
                &[],
                "Route matched",
                true,
            ),
        );
    }

    value
}

fn build_operation(
    operation_id: &str,
    summary: &str,
    description: &str,
    path: &str,
    method: &str,
    request_schema: Option<&Value>,
    response_schema: Option<&Value>,
    query_params: &[ApiQueryParam],
    success_description: &str,
    include_404: bool,
) -> Value {
    let mut parameters = path_parameters(path);
    parameters.extend(query_parameters(query_params));

    let mut responses = json!({
        "200": {
            "description": success_description,
            "content": {
                "application/json": {
                    "schema": response_schema
                        .map(resolve_local_schema_refs)
                        .unwrap_or_else(default_object_schema)
                }
            }
        },
        "400": error_response("Invalid request"),
        "405": error_response("Method not allowed"),
        "500": error_response("Action or runtime failed"),
        "504": error_response("Action timed out")
    });

    if include_404 {
        responses
            .as_object_mut()
            .expect("responses should be an object")
            .insert("404".to_string(), error_response("Route not configured"));
    }

    let mut operation = json!({
        "tags": ["public"],
        "operationId": operation_id,
        "summary": summary,
        "description": description,
        "parameters": parameters,
        "responses": responses
    });

    if method_allows_request_body(method) {
        operation
            .as_object_mut()
            .expect("operation should be an object")
            .insert(
                "requestBody".to_string(),
                request_body_schema(request_schema),
            );
    }

    operation
}

fn request_body_schema(schema: Option<&Value>) -> Value {
    json!({
        "required": false,
        "content": {
            "application/json": {
                "schema": schema
                    .map(resolve_local_schema_refs)
                    .unwrap_or_else(default_object_schema)
            }
        }
    })
}

fn error_response(description: &str) -> Value {
    json!({
        "description": description,
        "content": {
            "application/json": {
                "schema": {
                    "type": "object",
                    "required": ["error", "message"],
                    "properties": {
                        "error": { "type": "string" },
                        "message": { "type": "string" }
                    }
                }
            }
        }
    })
}

fn default_object_schema() -> Value {
    json!({})
}

fn resolve_local_schema_refs(schema: &Value) -> Value {
    let defs = schema
        .get("$defs")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    let mut resolved = schema.clone();
    inline_local_refs(&mut resolved, &defs);

    if let Some(object) = resolved.as_object_mut() {
        object.remove("$defs");
    }

    resolved
}

fn inline_local_refs(value: &mut Value, defs: &serde_json::Map<String, Value>) {
    match value {
        Value::Object(object) => {
            if let Some(ref_value) = object.get("$ref").and_then(Value::as_str) {
                if let Some(name) = ref_value.strip_prefix("#/$defs/") {
                    if let Some(definition) = defs.get(name) {
                        *value = definition.clone();
                        inline_local_refs(value, defs);
                        return;
                    }
                }
            }

            for child in object.values_mut() {
                inline_local_refs(child, defs);
            }
        }
        Value::Array(items) => {
            for item in items {
                inline_local_refs(item, defs);
            }
        }
        _ => {}
    }
}

fn method_allows_request_body(method: &str) -> bool {
    matches!(
        method.to_ascii_uppercase().as_str(),
        "POST" | "PUT" | "PATCH"
    )
}

fn method_to_openapi_key(method: HttpMethod) -> &'static str {
    match method {
        HttpMethod::Get => "get",
        HttpMethod::Post => "post",
        HttpMethod::Put => "put",
        HttpMethod::Patch => "patch",
        HttpMethod::Delete => "delete",
    }
}

fn action_key(action: &ActionDefinition) -> String {
    format!("{}::{}", action.source.display(), action.entrypoint)
}

fn operation_id(action: &ActionDefinition, method: &str, path: &str) -> String {
    format!(
        "{}_{}_{}",
        sanitize_identifier(&action.entrypoint),
        method.to_ascii_lowercase(),
        sanitize_identifier(path)
    )
}

fn sanitize_identifier(value: &str) -> String {
    let mut output = String::new();

    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            output.push(character.to_ascii_lowercase());
        } else if !output.ends_with('_') {
            output.push('_');
        }
    }

    output.trim_matches('_').to_string()
}

fn path_parameters(path: &str) -> Vec<Value> {
    path.split('/')
        .filter_map(|part| {
            if part.starts_with('{') && part.ends_with('}') {
                let name = part.trim_start_matches('{').trim_end_matches('}');

                Some(json!({
                    "name": name,
                    "in": "path",
                    "required": true,
                    "schema": {
                        "type": "string"
                    }
                }))
            } else {
                None
            }
        })
        .collect()
}

fn query_parameters(query_params: &[ApiQueryParam]) -> Vec<Value> {
    query_params
        .iter()
        .map(|param| {
            json!({
                "name": param.name,
                "in": "query",
                "required": param.required,
                "schema": param.schema,
            })
        })
        .collect()
}
