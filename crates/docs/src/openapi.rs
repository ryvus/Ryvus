use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use ryvus_protocol::{
    ActionDefinition, ActionKind, ApiQueryParam, AuthorizerAction, AuthorizerParameter,
    AuthorizerParameterLocation, AuthorizerSecurity,
};

pub fn build_public_openapi_json_from_actions<'a>(
    actions: impl IntoIterator<Item = &'a ActionDefinition>,
) -> Value {
    let actions = actions.into_iter().collect::<Vec<_>>();
    let authorizers = authorizers_by_name(&actions);
    let mut paths = serde_json::Map::new();
    let mut tags = BTreeSet::new();
    let mut security_schemes = serde_json::Map::new();

    for action in actions {
        if !matches!(&action.kind, ActionKind::Api(_)) {
            continue;
        }

        let ActionKind::Api(api) = &action.kind else {
            continue;
        };

        let method = api.method.to_lowercase();
        let tag = module_tag(action);
        tags.insert(tag.clone());
        let action_key = action_key(action);
        let operation_id = operation_id(action, &api.method, &api.path);
        let authorizer = api
            .authorizer
            .as_deref()
            .and_then(|name| authorizers.get(name).copied());
        let security = api.authorizer.as_deref().and_then(|name| {
            authorizer.map(|authorizer| {
                register_security_schemes(name, authorizer, &mut security_schemes)
            })
        });

        let path_item = paths.entry(api.path.clone()).or_insert_with(|| json!({}));

        let path_object = path_item
            .as_object_mut()
            .expect("path item should be an object");

        path_object.insert(
            method,
            build_operation(
                &operation_id,
                action_name(action),
                &format!("Routes to Ryvus action '{}'.", action_key),
                &tag,
                &api.path,
                &api.method,
                &api.consumes,
                &api.produces,
                api.request_schema.as_ref(),
                api.response_schema.as_ref(),
                &api.query_params,
                api.authorizer.as_deref(),
                authorizer.map(|authorizer| authorizer.parameters.as_slice()),
                security.as_deref(),
            ),
        );
    }

    let mut openapi = json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Ryvus Public API",
            "version": "1.0.0"
        },
        "tags": tags.into_iter().map(|name| json!({ "name": name })).collect::<Vec<_>>(),
        "paths": paths
    });

    if !security_schemes.is_empty() {
        openapi
            .as_object_mut()
            .expect("openapi should be an object")
            .insert(
                "components".to_string(),
                json!({ "securitySchemes": security_schemes }),
            );
    }

    openapi
}

fn build_operation(
    operation_id: &str,
    summary: &str,
    description: &str,
    tag: &str,
    path: &str,
    method: &str,
    consumes: &[String],
    produces: &[String],
    request_schema: Option<&Value>,
    response_schema: Option<&Value>,
    query_params: &[ApiQueryParam],
    authorizer: Option<&str>,
    authorizer_parameters: Option<&[AuthorizerParameter]>,
    security: Option<&[String]>,
) -> Value {
    let mut parameters = path_parameters(path);
    parameters.extend(query_parameters(query_params));
    if let Some(authorizer_parameters) = authorizer_parameters {
        parameters.extend(authorizer_parameters.iter().map(authorizer_parameter));
    }

    let responses = json!({
        "200": {
            "description": "Successful response",
            "content": media_content(produces, response_schema, true)
        },
        "400": error_response("Invalid request"),
        "405": error_response("Method not allowed"),
        "500": error_response("Action or runtime failed"),
        "504": error_response("Action timed out")
    });

    let mut operation = json!({
        "tags": [tag],
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
                request_body_schema(consumes, request_schema),
            );
    }

    if let Some(authorizer) = authorizer {
        operation
            .as_object_mut()
            .expect("operation should be an object")
            .insert("x-ryvus-authorizer".to_string(), json!(authorizer));
    }

    if let Some(security) = security {
        if !security.is_empty() {
            operation
                .as_object_mut()
                .expect("operation should be an object")
                .insert(
                    "security".to_string(),
                    Value::Array(
                        security
                            .iter()
                            .map(|name| json!({ name: [] }))
                            .collect::<Vec<_>>(),
                    ),
                );
        }
    }

    operation
}

fn authorizers_by_name<'a>(
    actions: &[&'a ActionDefinition],
) -> BTreeMap<String, &'a AuthorizerAction> {
    actions
        .iter()
        .filter_map(|action| {
            if let ActionKind::Authorizer(authorizer) = &action.kind {
                action.name.as_ref().map(|name| (name.clone(), authorizer))
            } else {
                None
            }
        })
        .collect()
}

fn register_security_schemes(
    authorizer_name: &str,
    authorizer: &AuthorizerAction,
    security_schemes: &mut serde_json::Map<String, Value>,
) -> Vec<String> {
    authorizer
        .security
        .iter()
        .map(|security| {
            let name = security_scheme_name(authorizer_name, security);
            security_schemes.insert(name.clone(), security_scheme(security));
            name
        })
        .collect()
}

fn security_scheme_name(authorizer_name: &str, security: &AuthorizerSecurity) -> String {
    let suffix = match security.security_type.as_str() {
        "http" => security.scheme.as_deref().unwrap_or("http").to_string(),
        "apiKey" => format!(
            "{}_{}",
            security
                .location
                .as_ref()
                .map(parameter_location_name)
                .unwrap_or_else(|| "api_key".to_string()),
            security.name.as_deref().unwrap_or("api_key")
        ),
        other => other.to_string(),
    };

    format!(
        "{}_{}",
        sanitize_identifier(authorizer_name),
        sanitize_identifier(&suffix)
    )
}

fn security_scheme(security: &AuthorizerSecurity) -> Value {
    let mut object = serde_json::Map::new();
    object.insert("type".to_string(), json!(security.security_type));
    if let Some(scheme) = &security.scheme {
        object.insert("scheme".to_string(), json!(scheme));
    }
    if let Some(location) = &security.location {
        object.insert("in".to_string(), serde_json::to_value(location).unwrap());
    }
    if let Some(name) = &security.name {
        object.insert("name".to_string(), json!(name));
    }

    Value::Object(object)
}

fn parameter_location_name(location: &AuthorizerParameterLocation) -> String {
    match location {
        AuthorizerParameterLocation::Header => "header".to_string(),
        AuthorizerParameterLocation::Query => "query".to_string(),
        AuthorizerParameterLocation::Cookie => "cookie".to_string(),
    }
}

fn authorizer_parameter(parameter: &AuthorizerParameter) -> Value {
    json!({
        "name": parameter.name,
        "in": parameter.location,
        "required": parameter.required,
        "schema": {
            "type": parameter.parameter_type,
        },
    })
}

fn request_body_schema(consumes: &[String], schema: Option<&Value>) -> Value {
    json!({
        "required": false,
        "content": media_content(consumes, schema, false)
    })
}

fn media_content(media_types: &[String], schema: Option<&Value>, response: bool) -> Value {
    let mut content = serde_json::Map::new();
    let media_types = if media_types.is_empty() {
        vec!["application/json".to_string()]
    } else {
        media_types.to_vec()
    };

    for media_type in media_types {
        content.insert(
            media_type.clone(),
            json!({
                "schema": schema_for_media_type(&media_type, schema, response)
            }),
        );
    }

    Value::Object(content)
}

fn schema_for_media_type(media_type: &str, schema: Option<&Value>, response: bool) -> Value {
    if media_type.starts_with("text/") {
        return schema
            .map(resolve_local_schema_refs)
            .unwrap_or_else(|| json!({ "type": "string" }));
    }

    if media_type == "application/x-www-form-urlencoded" && !response {
        return schema
            .map(resolve_local_schema_refs)
            .unwrap_or_else(default_object_schema);
    }

    schema
        .map(resolve_local_schema_refs)
        .unwrap_or_else(default_object_schema)
}

fn error_response(description: &str) -> Value {
    json!({
        "description": description,
        "content": {
            "application/json": {
                "schema": {
                    "type": "object",
                    "required": ["invocation_id", "error", "message"],
                    "properties": {
                        "invocation_id": { "type": "string" },
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

fn action_key(action: &ActionDefinition) -> String {
    format!("{}::{}", action.source.display(), action.entrypoint)
}

fn operation_id(action: &ActionDefinition, method: &str, path: &str) -> String {
    format!(
        "{}_{}_{}",
        sanitize_identifier(action_name(action)),
        method.to_ascii_lowercase(),
        sanitize_identifier(path)
    )
}

fn action_name(action: &ActionDefinition) -> &str {
    action.name.as_deref().unwrap_or(&action.entrypoint)
}

fn module_tag(action: &ActionDefinition) -> String {
    module_tag_from_source(&action.source.display().to_string())
}

fn module_tag_from_source(source: &str) -> String {
    let mut components = std::path::Path::new(source).components();

    while let Some(component) = components.next() {
        if component.as_os_str() == "modules" {
            if let Some(module) = components.next() {
                return module.as_os_str().to_string_lossy().to_string();
            }
        }
    }

    "public".to_string()
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ryvus_protocol::{
        ActionDefinition, ActionKind, ApiAction, AuthorizerAction, AuthorizerParameter,
        AuthorizerParameterLocation, AuthorizerSecurity, RuntimeKind, ScheduleAction,
    };
    use serde_json::json;

    use super::build_public_openapi_json_from_actions;

    fn api_definition(
        method: &str,
        path: &str,
        source: &str,
        entrypoint: &str,
    ) -> ActionDefinition {
        ActionDefinition {
            runtime: RuntimeKind::Python,
            kind: ActionKind::Api(ApiAction {
                method: method.to_string(),
                path: path.to_string(),
                consumes: vec!["application/json".to_string()],
                produces: vec!["application/json".to_string()],
                request_schema: None,
                response_schema: None,
                query_params: Vec::new(),
                authorizer: None,
            }),
            source: PathBuf::from(source),
            entrypoint: entrypoint.to_string(),
            name: None,
            policy: ryvus_protocol::ActionExecutionPolicy::default(),
        }
    }

    #[test]
    fn generated_openapi_uses_paths_methods_and_stable_operation_ids() {
        let mut actions = vec![
            api_definition("GET", "/hello/{name}", "src/hello.py", "hello"),
            api_definition("POST", "/hello", "src/post.py", "hello"),
        ];

        if let ActionKind::Api(api) = &mut actions[0].kind {
            api.response_schema = Some(json!({
                "$defs": {
                    "PetResponse": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string" }
                        }
                    }
                },
                "type": "object",
                "properties": {
                    "pets": {
                        "type": "array",
                        "items": {
                            "$ref": "#/$defs/PetResponse"
                        }
                    }
                }
            }));
        }

        let openapi = build_public_openapi_json_from_actions(&actions);

        assert_eq!(openapi["openapi"], json!("3.1.0"));
        assert_eq!(openapi["info"]["title"], json!("Ryvus Public API"));
        assert_eq!(openapi["tags"], json!([{ "name": "public" }]));
        assert_eq!(
            openapi["paths"]["/hello/{name}"]["get"]["operationId"],
            json!("hello_get_hello_name")
        );
        assert_eq!(
            openapi["paths"]["/hello"]["post"]["operationId"],
            json!("hello_post_hello")
        );
        assert!(openapi["paths"]["/hello"]["post"]["responses"]["400"].is_object());
        assert!(openapi["paths"]["/hello"]["post"]["responses"]["405"].is_object());
        assert!(openapi["paths"]["/hello"]["post"]["responses"]["500"].is_object());
        assert!(openapi["paths"]["/hello"]["post"]["responses"]["504"].is_object());
        assert!(
            openapi["paths"]["/hello/{name}"]["get"]["responses"]["200"]["content"]
                ["application/json"]["schema"]["properties"]["pets"]["items"]["$ref"]
                .is_null()
        );
        assert_eq!(
            openapi["paths"]["/hello/{name}"]["get"]["responses"]["200"]["content"]
                ["application/json"]["schema"]["properties"]["pets"]["items"]["properties"]["id"]
                ["type"],
            json!("string")
        );
    }

    #[test]
    fn generated_openapi_groups_modules_and_prefers_action_names() {
        let mut action = api_definition(
            "GET",
            "/store/products",
            "dist/modules/store/api/list_products.js",
            "default",
        );
        action.name = Some("listProducts".to_string());

        let openapi = build_public_openapi_json_from_actions([&action]);

        assert_eq!(openapi["tags"], json!([{ "name": "store" }]));
        assert_eq!(
            openapi["paths"]["/store/products"]["get"]["tags"],
            json!(["store"])
        );
        assert_eq!(
            openapi["paths"]["/store/products"]["get"]["summary"],
            json!("listProducts")
        );
        assert_eq!(
            openapi["paths"]["/store/products"]["get"]["operationId"],
            json!("listproducts_get_store_products")
        );
    }

    #[test]
    fn protected_routes_include_authorizer_extension() {
        let mut action = api_definition("GET", "/pets", "src/pets.py", "list_pets");

        if let ActionKind::Api(api) = &mut action.kind {
            api.authorizer = Some("petstore".to_string());
        }

        let authorizer = ActionDefinition {
            runtime: RuntimeKind::Python,
            kind: ActionKind::Authorizer(AuthorizerAction {
                security: vec![
                    AuthorizerSecurity {
                        security_type: "http".to_string(),
                        scheme: Some("bearer".to_string()),
                        location: None,
                        name: None,
                    },
                    AuthorizerSecurity {
                        security_type: "apiKey".to_string(),
                        scheme: None,
                        location: Some(AuthorizerParameterLocation::Header),
                        name: Some("X-API-Key".to_string()),
                    },
                ],
                parameters: vec![AuthorizerParameter {
                    name: "X-Tenant-ID".to_string(),
                    location: AuthorizerParameterLocation::Header,
                    required: true,
                    parameter_type: "string".to_string(),
                }],
                cache: None,
            }),
            source: PathBuf::from("src/auth.py"),
            entrypoint: "auth".to_string(),
            name: Some("petstore".to_string()),
            policy: ryvus_protocol::ActionExecutionPolicy::default(),
        };

        let openapi = build_public_openapi_json_from_actions([&action, &authorizer]);

        assert_eq!(
            openapi["paths"]["/pets"]["get"]["x-ryvus-authorizer"],
            json!("petstore")
        );
        assert_eq!(
            openapi["components"]["securitySchemes"]["petstore_bearer"],
            json!({ "type": "http", "scheme": "bearer" })
        );
        assert_eq!(
            openapi["components"]["securitySchemes"]["petstore_header_x_api_key"],
            json!({ "type": "apiKey", "in": "header", "name": "X-API-Key" })
        );
        assert_eq!(
            openapi["paths"]["/pets"]["get"]["security"],
            json!([
                { "petstore_bearer": [] },
                { "petstore_header_x_api_key": [] }
            ])
        );
        assert!(openapi["paths"]["/pets"]["get"]["parameters"]
            .as_array()
            .expect("parameters should be an array")
            .contains(&json!({
                "name": "X-Tenant-ID",
                "in": "header",
                "required": true,
                "schema": { "type": "string" }
            })));
    }

    #[test]
    fn openapi_excludes_schedules() {
        let api_action = ActionDefinition {
            runtime: RuntimeKind::Python,
            kind: ActionKind::Api(ApiAction {
                method: "GET".to_string(),
                path: "/hello".to_string(),
                consumes: vec!["application/json".to_string()],
                produces: vec!["application/json".to_string()],
                request_schema: None,
                response_schema: None,
                query_params: Vec::new(),
                authorizer: None,
            }),
            source: PathBuf::from("src/hello.py"),
            entrypoint: "hello".to_string(),
            name: None,
            policy: ryvus_protocol::ActionExecutionPolicy::default(),
        };

        let schedule_action = ActionDefinition {
            runtime: RuntimeKind::Python,
            kind: ActionKind::Schedule(ScheduleAction {
                expression: "every 10s".to_string(),
            }),
            source: PathBuf::from("src/modules/petstore/schedules/restock.py"),
            entrypoint: "restock_report".to_string(),
            name: Some("restock_report".to_string()),
            policy: ryvus_protocol::ActionExecutionPolicy::default(),
        };

        let openapi = build_public_openapi_json_from_actions([&api_action, &schedule_action]);

        assert_eq!(
            openapi["paths"]["/hello"]["get"]["operationId"],
            json!("hello_get_hello")
        );
        assert!(openapi["paths"]["/system/schedules"].is_null());
        assert!(openapi["paths"]["/system/schedules/restock_report/run"].is_null());
        assert!(openapi["paths"]["/system/schedules/{id}/run"].is_null());
        assert_eq!(openapi["tags"], json!([{ "name": "public" }]));
    }
}
