mod support;

use axum::http::{Method, StatusCode};
use ryvus_gateway::{openapi::public::build_public_openapi_json_from_actions, server};
use ryvus_protocol::ActionKind;
use serde_json::json;

use support::*;

#[tokio::test]
async fn invokes_get_api_action() {
    let project = TestProject::new("get");
    project.add_action(
        "hello.py",
        r#"
@api_action(method="GET", path="/hello")
def hello(event, context):
    print("hello log")
    return {"message": "Hello from Ryvus"}
"#,
    );
    project.write_manifest(&[action("GET", "/hello", "src/hello.py", "hello")]);

    let response = request(&project, Method::GET, "/hello", None).await;

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.body, json!({"message": "Hello from Ryvus"}));
}

#[tokio::test]
async fn serves_scalar_docs_and_openapi_json() {
    let project = TestProject::new("docs");
    project.add_action(
        "hello.py",
        r#"
@api_action(method="GET", path="/hello")
def hello():
    return {"ok": True}
"#,
    );
    project.write_manifest(&[action("GET", "/hello", "src/hello.py", "hello")]);

    let docs = text_request(&project, Method::GET, "/docs").await;
    assert_eq!(docs.status, StatusCode::OK);
    assert!(docs.body.contains("Scalar.createApiReference"));
    assert!(docs.body.contains("/openapi.json"));
    assert!(docs.body.contains("/assets/scalar-api-reference.js"));
    assert!(!docs.body.contains("https://"));

    let scalar = text_request(&project, Method::GET, "/assets/scalar-api-reference.js").await;
    assert_eq!(scalar.status, StatusCode::OK);
    assert!(scalar.body.contains("createApiReference"));

    let openapi = request(&project, Method::GET, "/openapi.json", None).await;
    assert_eq!(openapi.status, StatusCode::OK);
    assert_eq!(
        openapi.body["paths"]["/hello"]["get"]["operationId"],
        json!("hello_get_hello")
    );
}

#[tokio::test]
async fn keeps_system_health_without_system_docs() {
    let project = TestProject::new("system-health");
    project.write_manifest(&[]);

    let health = request(&project, Method::GET, "/system/health", None).await;
    assert_eq!(health.status, StatusCode::OK);
    assert_eq!(health.body, json!({ "status": "ok" }));

    let system_docs = request(&project, Method::GET, "/system/docs", None).await;
    assert_eq!(system_docs.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn invokes_node_api_action() {
    let project = TestProject::new("node-get");
    project.add_node_action(
        "hello.js",
        r#"
export default apiAction({
  method: "GET",
  path: "/node/hello",
  handler(event, context) {
    return {
      message: "Hello from Node",
      invocation_id: context.invocationId,
      event,
    };
  },
});
"#,
    );
    project.write_manifest(&[node_action("GET", "/node/hello", "src/hello.js", "default")]);

    let response = request(&project, Method::GET, "/node/hello", None).await;

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.body["message"], json!("Hello from Node"));
    assert!(response.body["invocation_id"].is_string());
    assert_eq!(response.body["event"]["body"], json!(null));
}

#[tokio::test]
async fn invokes_node_api_action_with_object_binding_and_schemas() {
    let project = TestProject::new("node-binding");
    project.add_node_action(
        "store.js",
        r#"
const itemSchema = object({
  sku: string(),
  quantity: integer(),
});

export default apiAction({
  method: "POST",
  path: "/store/carts/{cart_id}",
  query: {
    confirm: boolean(),
  },
  body: object({
    sku: string(),
    quantity: integer(),
  }),
  response: object({
    cart: itemSchema,
    cart_id: string(),
    confirmed: boolean(),
  }),
  handler({ path, query, body }) {
    return {
      cart: body,
      cart_id: path.cart_id,
      confirmed: query.confirm,
    };
  },
});
"#,
    );
    project.write_manifest(&[json!({
        "runtime": "Node",
        "kind": {
            "Api": {
                "method": "POST",
                "path": "/store/carts/{cart_id}",
                "query_params": [
                    {
                        "name": "confirm",
                        "required": true,
                        "schema": { "type": "boolean" }
                    }
                ],
                "request_schema": {
                    "type": "object",
                    "required": ["sku", "quantity"],
                    "properties": {
                        "sku": { "type": "string" },
                        "quantity": { "type": "integer" }
                    }
                },
                "response_schema": {
                    "type": "object",
                    "required": ["cart", "cart_id", "confirmed"],
                    "properties": {
                        "cart": {
                            "type": "object",
                            "required": ["sku", "quantity"],
                            "properties": {
                                "sku": { "type": "string" },
                                "quantity": { "type": "integer" }
                            }
                        },
                        "cart_id": { "type": "string" },
                        "confirmed": { "type": "boolean" }
                    }
                }
            }
        },
        "source": "src/store.js",
        "entrypoint": "default"
    })]);

    let response = request(
        &project,
        Method::POST,
        "/store/carts/cart_123?confirm=true",
        Some(json!({ "sku": "food_salmon_2kg", "quantity": 2 })),
    )
    .await;

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.body["cart_id"], json!("cart_123"));
    assert_eq!(response.body["confirmed"], json!(true));
    assert_eq!(response.body["cart"]["quantity"], json!(2));

    let invalid_query = request(
        &project,
        Method::POST,
        "/store/carts/cart_123?confirm=maybe",
        Some(json!({ "sku": "food_salmon_2kg", "quantity": 2 })),
    )
    .await;
    assert_eq!(invalid_query.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        invalid_query.body["error"],
        json!("request_validation_failed")
    );
}

#[tokio::test]
async fn binds_path_params_query_params_and_json_body() {
    let project = TestProject::new("binding");
    project.add_action(
        "path.py",
        r#"
@api_action(method="GET", path="/hello/{name}")
def hello_name(name: str):
    return {"message": f"Hello {name}"}
"#,
    );
    project.add_action(
        "query.py",
        r#"
@api_action(method="GET", path="/search")
def search(limit: int, q: str | None = None):
    return {"limit": limit, "q": q}
"#,
    );
    project.add_action(
        "post.py",
        r#"
@api_action(method="POST", path="/users")
def create_user(name: str):
    return {"created": name}
"#,
    );
    project.write_manifest(&[
        action("GET", "/hello/{name}", "src/path.py", "hello_name"),
        action("GET", "/search", "src/query.py", "search"),
        action("POST", "/users", "src/post.py", "create_user"),
    ]);

    let path = request(&project, Method::GET, "/hello/Maikel", None).await;
    assert_eq!(path.status, StatusCode::OK);
    assert_eq!(path.body, json!({"message": "Hello Maikel"}));

    let query = request(&project, Method::GET, "/search?limit=5&q=ryvus", None).await;
    assert_eq!(query.status, StatusCode::OK);
    assert_eq!(query.body, json!({"limit": 5, "q": "ryvus"}));

    let post = request(
        &project,
        Method::POST,
        "/users",
        Some(json!({"name": "Ada"})),
    )
    .await;
    assert_eq!(post.status, StatusCode::OK);
    assert_eq!(post.body, json!({"created": "Ada"}));
}

#[tokio::test]
async fn returns_expected_http_errors() {
    let project = TestProject::new("errors");
    project.add_action(
        "hello.py",
        r#"
@api_action(method="GET", path="/hello")
def hello():
    return {"ok": True}
"#,
    );
    project.add_action(
        "post.py",
        r#"
@api_action(method="POST", path="/post")
def post(name: str):
    return {"name": name}
"#,
    );
    project.add_action(
        "fails.py",
        r#"
@api_action(method="GET", path="/fails")
def fails():
    raise RuntimeError("boom")
"#,
    );
    project.add_action(
        "needs.py",
        r#"
@api_action(method="GET", path="/needs")
def needs(required: str):
    return {"required": required}
"#,
    );
    project.write_manifest(&[
        action("GET", "/hello", "src/hello.py", "hello"),
        action("POST", "/post", "src/post.py", "post"),
        action("GET", "/fails", "src/fails.py", "fails"),
        action("GET", "/needs", "src/needs.py", "needs"),
    ]);

    assert_public_error(
        request(&project, Method::GET, "/missing", None).await,
        StatusCode::NOT_FOUND,
        "route_not_configured",
    );
    assert_public_error(
        request(&project, Method::POST, "/hello", None).await,
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
    );
    assert_public_error(
        raw_request(&project, Method::POST, "/post", "{").await,
        StatusCode::BAD_REQUEST,
        "invalid_json_body",
    );
    assert_public_error(
        request(&project, Method::GET, "/needs", None).await,
        StatusCode::BAD_REQUEST,
        "action_failed",
    );
    assert_public_error(
        request(&project, Method::GET, "/fails", None).await,
        StatusCode::INTERNAL_SERVER_ERROR,
        "action_failed",
    );
}

#[tokio::test]
async fn validates_query_params_and_request_body_before_invocation() {
    let project = TestProject::new("gateway-validation");
    project.add_action(
        "query.py",
        r#"
@api_action(method="GET", path="/search")
def search(limit: int):
    return {"limit": limit}
"#,
    );
    project.add_action(
        "post.py",
        r#"
@api_action(method="POST", path="/pets")
def create_pet(name: str, age: int):
    return {"name": name, "age": age}
"#,
    );
    project.write_manifest(&[
        json!({
            "runtime": "Python",
            "kind": {
                "Api": {
                    "method": "GET",
                    "path": "/search",
                    "query_params": [
                        {
                            "name": "limit",
                            "required": true,
                            "schema": { "type": "integer" }
                        }
                    ]
                }
            },
            "source": "src/query.py",
            "entrypoint": "search"
        }),
        json!({
            "runtime": "Python",
            "kind": {
                "Api": {
                    "method": "POST",
                    "path": "/pets",
                    "query_params": [],
                    "request_schema": {
                        "type": "object",
                        "required": ["name", "age"],
                        "properties": {
                            "name": { "type": "string" },
                            "age": { "type": "integer" }
                        }
                    }
                }
            },
            "source": "src/post.py",
            "entrypoint": "create_pet"
        }),
    ]);

    let missing_query = request(&project, Method::GET, "/search", None).await;
    assert_eq!(missing_query.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        missing_query.body["error"],
        json!("request_validation_failed")
    );
    assert!(missing_query.body["invocation_id"].is_string());

    let invalid_query = request(&project, Method::GET, "/search?limit=nope", None).await;
    assert_eq!(invalid_query.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        invalid_query.body["error"],
        json!("request_validation_failed")
    );

    let missing_body_field = request(
        &project,
        Method::POST,
        "/pets",
        Some(json!({ "name": "Nori" })),
    )
    .await;
    assert_eq!(missing_body_field.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        missing_body_field.body["error"],
        json!("request_validation_failed")
    );

    let invalid_body_type = request(
        &project,
        Method::POST,
        "/pets",
        Some(json!({ "name": "Nori", "age": "old" })),
    )
    .await;
    assert_eq!(invalid_body_type.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        invalid_body_type.body["error"],
        json!("request_validation_failed")
    );
}

#[tokio::test]
async fn validates_duplicate_routes_at_startup() {
    let project = TestProject::new("duplicate");
    project.write_manifest(&[
        action("GET", "/users/{id}", "src/a.py", "a"),
        action("GET", "/users/{name}", "src/b.py", "b"),
    ]);

    let error = server::build_app(&project.config()).expect_err("duplicate route should fail");

    assert!(error.to_string().contains("duplicate route"));
}

#[tokio::test]
async fn validates_unsupported_methods_and_invalid_paths_at_startup() {
    let bad_method = TestProject::new("bad-method");
    bad_method.write_manifest(&[action("TRACE", "/hello", "src/a.py", "a")]);

    let error = server::build_app(&bad_method.config()).expect_err("method should fail");
    assert!(error.to_string().contains("unsupported HTTP method"));

    let bad_path = TestProject::new("bad-path");
    bad_path.write_manifest(&[action("GET", "hello", "src/a.py", "a")]);

    let error = server::build_app(&bad_path.config()).expect_err("path should fail");
    assert!(error.to_string().contains("path must start"));
}

#[test]
fn openapi_uses_discovered_routes_and_stable_operation_ids() {
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

    assert_eq!(
        openapi["paths"]["/hello/{name}"]["get"]["operationId"],
        json!("hello_get_hello_name")
    );
    assert_eq!(
        openapi["paths"]["/hello"]["post"]["operationId"],
        json!("hello_post_hello")
    );
    assert!(openapi["paths"]["/hello"]["post"]["responses"]["400"].is_object());
    assert!(openapi["paths"]["/hello"]["post"]["responses"]["504"].is_object());
    assert_eq!(
        openapi["paths"]["/hello"]["post"]["responses"]["400"]["content"]["application/json"]
            ["schema"]["properties"]["error"]["type"],
        json!("string")
    );
    assert_eq!(
        openapi["paths"]["/hello"]["post"]["responses"]["400"]["content"]["application/json"]
            ["schema"]["properties"]["invocation_id"]["type"],
        json!("string")
    );
    assert_eq!(
        openapi["paths"]["/hello"]["post"]["responses"]["400"]["content"]["application/json"]
            ["schema"]["properties"]["message"]["type"],
        json!("string")
    );
    assert!(
        openapi["paths"]["/hello/{name}"]["get"]["responses"]["200"]["content"]["application/json"]
            ["schema"]["properties"]["pets"]["items"]["$ref"]
            .is_null()
    );
    assert_eq!(
        openapi["paths"]["/hello/{name}"]["get"]["responses"]["200"]["content"]["application/json"]
            ["schema"]["properties"]["pets"]["items"]["properties"]["id"]["type"],
        json!("string")
    );
}
