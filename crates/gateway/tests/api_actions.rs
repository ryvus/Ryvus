use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    body::{to_bytes, Body},
    http::{Method, Request, StatusCode},
};
use ryvus_gateway::{
    openapi::public::build_public_openapi_json_from_actions, server, server::GatewayServerConfig,
};
use ryvus_protocol::{ActionDefinition, ActionKind, ApiAction, RuntimeKind};
use serde_json::{json, Value};
use tower::ServiceExt;

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

    assert_eq!(
        request(&project, Method::GET, "/missing", None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(&project, Method::POST, "/hello", None).await.status,
        StatusCode::METHOD_NOT_ALLOWED
    );
    assert_eq!(
        raw_request(&project, Method::POST, "/post", "{")
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(&project, Method::GET, "/needs", None).await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(&project, Method::GET, "/fails", None).await.status,
        StatusCode::INTERNAL_SERVER_ERROR
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

struct TestProject {
    root: PathBuf,
}

impl TestProject {
    fn new(name: &str) -> Self {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("ryvus-api-action-{name}-{id}"));

        fs::create_dir_all(root.join("src")).expect("test project should be created");
        fs::create_dir_all(root.join(".ryvus")).expect("ryvus dir should be created");

        Self { root }
    }

    fn add_action(&self, file: &str, body: &str) {
        let sdk_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../sdk/python")
            .canonicalize()
            .expect("python SDK path should resolve");
        let content = format!(
            r#"import sys
sys.path.insert(0, {sdk_path:?})
from ryvus import api_action
{body}
"#,
            sdk_path = sdk_path.to_string_lossy().to_string(),
            body = body,
        );

        fs::write(self.root.join("src").join(file), content).expect("action should be written");
    }

    fn write_manifest(&self, actions: &[Value]) {
        fs::write(
            self.root.join(".ryvus/action-manifest.json"),
            serde_json::to_string_pretty(&json!({ "actions": actions }))
                .expect("manifest should serialize"),
        )
        .expect("manifest should be written");
    }

    fn config(&self) -> GatewayServerConfig {
        GatewayServerConfig {
            project_root: self.root.clone(),
            manifest_path: ".ryvus/action-manifest.json".into(),
            addr: ([127, 0, 0, 1], 0).into(),
        }
    }
}

struct TestResponse {
    status: StatusCode,
    body: Value,
}

async fn request(
    project: &TestProject,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> TestResponse {
    let raw_body = body.map(|body| body.to_string()).unwrap_or_default();
    raw_request(project, method, uri, &raw_body).await
}

async fn raw_request(project: &TestProject, method: Method, uri: &str, body: &str) -> TestResponse {
    let app = server::build_app(&project.config()).expect("gateway app should build");
    let response = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("request should build"),
        )
        .await
        .expect("request should be handled");

    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body should read");
    let body = serde_json::from_slice(&bytes).unwrap_or_else(|_| json!(null));

    TestResponse { status, body }
}

fn action(method: &str, path: &str, source: &str, entrypoint: &str) -> Value {
    json!({
        "runtime": "Python",
        "kind": {
            "Api": {
                "method": method,
                "path": path,
                "query_params": []
            }
        },
        "source": source,
        "entrypoint": entrypoint
    })
}

fn api_definition(method: &str, path: &str, source: &str, entrypoint: &str) -> ActionDefinition {
    ActionDefinition {
        runtime: RuntimeKind::Python,
        kind: ActionKind::Api(ApiAction {
            method: method.to_string(),
            path: path.to_string(),
            request_schema: None,
            response_schema: None,
            query_params: Vec::new(),
        }),
        source: source.into(),
        entrypoint: entrypoint.to_string(),
    }
}
