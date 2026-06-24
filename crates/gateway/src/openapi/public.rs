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
        let operation_name = action.entrypoint.clone();

        let path_item = paths.entry(api.path.clone()).or_insert_with(|| json!({}));

        let path_object = path_item
            .as_object_mut()
            .expect("path item should be an object");

        path_object.insert(
            method,
            build_operation(
                &operation_name,
                &operation_name,
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
        let operation_name = action.entrypoint.clone();

        let path_item = paths.entry(api.path.clone()).or_insert_with(|| json!({}));

        let path_object = path_item
            .as_object_mut()
            .expect("OpenAPI path item should be an object");

        path_object.insert(
            method,
            build_operation(
                &operation_name,
                &operation_name,
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
                        .cloned()
                        .unwrap_or_else(default_object_schema)
                }
            }
        }
    });

    if include_404 {
        responses
            .as_object_mut()
            .expect("responses should be an object")
            .insert(
                "404".to_string(),
                json!({
                    "description": "Route not configured"
                }),
            );
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
                    .cloned()
                    .unwrap_or_else(default_object_schema)
            }
        }
    })
}

fn default_object_schema() -> Value {
    json!({})
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
