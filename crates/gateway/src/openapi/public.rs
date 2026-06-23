use serde_json::{json, Value};
use utoipa::openapi::OpenApi;

use ryvus_protocol::{ActionDefinition, ActionKind};

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
            json!({
                "tags": ["public"],
                "operationId": operation_name,
                "summary": operation_name,
                "description": format!("Routes to Ryvus action '{}'.", action_key),
                "requestBody": {
                    "required": false,
                    "content": {
                        "application/json": {
                            "schema": {
                                "type": "object",
                                "additionalProperties": true
                            }
                        }
                    }
                },
                "responses": {
                    "200": {
                        "description": "Successful response",
                        "content": {
                            "application/json": {
                                "schema": {
                                    "type": "object",
                                    "additionalProperties": true
                                }
                            }
                        }
                    }
                }
            }),
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
            json!({
                "tags": ["public"],
                "operationId": operation_name,
                "summary": operation_name,
                "description": format!("Routes to Ryvus action `{}`.", action_key),
                "requestBody": {
                    "required": false,
                    "content": {
                        "application/json": {
                            "schema": {
                                "type": "object",
                                "additionalProperties": true
                            }
                        }
                    }
                },
                "responses": {
                    "200": {
                        "description": "Route matched",
                        "content": {
                            "application/json": {
                                "schema": {
                                    "type": "object",
                                    "additionalProperties": true
                                }
                            }
                        }
                    },
                    "404": {
                        "description": "Route not configured"
                    }
                }
            }),
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
            json!({
                "tags": ["public"],
                "operationId": route.name,
                "summary": route.name,
                "description": format!("Routes to Ryvus action '{}'.", route.action),
                "requestBody": {
                    "required": false,
                    "content": {
                        "application/json": {
                            "schema": {
                                "type": "object",
                                "additionalProperties": true
                            }
                        }
                    }
                },
                "responses": {
                    "200": {
                        "description": "Successful response",
                        "content": {
                            "application/json": {
                                "schema": {
                                    "type": "object",
                                    "additionalProperties": true
                                }
                            }
                        }
                    }
                }
            }),
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
            json!({
                "tags": ["public"],
                "operationId": route.name,
                "summary": route.name,
                "description": format!("Routes to Ryvus action `{}`.", route.action),
                "requestBody": {
                    "required": false,
                    "content": {
                        "application/json": {
                            "schema": {
                                "type": "object",
                                "additionalProperties": true
                            }
                        }
                    }
                },
                "responses": {
                    "200": {
                        "description": "Route matched",
                        "content": {
                            "application/json": {
                                "schema": {
                                    "type": "object",
                                    "additionalProperties": true
                                }
                            }
                        }
                    },
                    "404": {
                        "description": "Route not configured"
                    }
                }
            }),
        );
    }

    value
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
