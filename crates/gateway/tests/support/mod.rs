use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    body::{to_bytes, Body},
    http::{Method, Request, StatusCode},
    Router,
};
use ryvus_gateway::{server, server::GatewayServerConfig};
use serde_json::{json, Value};
use tower::ServiceExt;

pub struct TestProject {
    root: PathBuf,
}

impl TestProject {
    pub fn new(name: &str) -> Self {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("ryvus-api-action-{name}-{id}"));

        fs::create_dir_all(root.join("src")).expect("test project should be created");
        fs::create_dir_all(root.join(".ryvus")).expect("ryvus dir should be created");

        Self { root }
    }

    pub fn add_action(&self, file: &str, body: &str) {
        let sdk_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../sdk/python")
            .canonicalize()
            .expect("python SDK path should resolve");
        let content = format!(
            r#"import sys
sys.path.insert(0, {sdk_path:?})
from ryvus import api_action, authorizer, scheduled_action
{body}
"#,
            sdk_path = sdk_path.to_string_lossy().to_string(),
            body = body,
        );

        fs::write(self.root.join("src").join(file), content).expect("action should be written");
    }

    pub fn add_node_action(&self, file: &str, body: &str) {
        let sdk_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../sdk/node/dist/index.js")
            .canonicalize()
            .expect("node SDK path should resolve");
        let content = format!(
            r#"import {{
  apiAction,
  array,
  authorizer,
  boolean,
  integer,
  number,
  object,
  string,
}} from {sdk_path:?};
{body}
"#,
            sdk_path = format!("file://{}", sdk_path.display()),
            body = body,
        );

        fs::write(self.root.join("src").join(file), content)
            .expect("node action should be written");
    }

    pub fn write_manifest(&self, actions: &[Value]) {
        fs::write(
            self.root.join(".ryvus/action-manifest.json"),
            serde_json::to_string_pretty(&json!({ "actions": actions }))
                .expect("manifest should serialize"),
        )
        .expect("manifest should be written");
    }

    pub fn config(&self) -> GatewayServerConfig {
        GatewayServerConfig {
            project_root: self.root.clone(),
            manifest_path: ".ryvus/action-manifest.json".into(),
            addr: ([127, 0, 0, 1], 0).into(),
        }
    }
}

pub struct TestResponse {
    pub status: StatusCode,
    pub body: Value,
    pub raw_body: String,
}

pub fn assert_public_error(response: TestResponse, status: StatusCode, error: &str) {
    assert_eq!(response.status, status);
    assert_eq!(response.body["error"], json!(error));
    assert!(response.body.get("execution_id").is_none());
    assert!(response.body.get("attempt_id").is_none());
    assert!(response.body["message"].is_string());
}

pub fn assert_invocation_error(response: TestResponse, status: StatusCode, error: &str) {
    assert_eq!(response.status, status);
    assert_eq!(response.body["error"], json!(error));
    assert!(response.body["execution_id"].is_string());
    assert!(response.body["attempt_id"].is_string());
    assert!(response.body["attempt_number"].is_u64());
    assert!(response.body["message"].is_string());
}

pub async fn request(
    project: &TestProject,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> TestResponse {
    let raw_body = body.map(|body| body.to_string()).unwrap_or_default();
    raw_request(project, method, uri, &raw_body).await
}

pub async fn raw_request(
    project: &TestProject,
    method: Method,
    uri: &str,
    body: &str,
) -> TestResponse {
    raw_request_with_content_type(project, method, uri, body, "application/json").await
}

pub async fn raw_request_with_content_type(
    project: &TestProject,
    method: Method,
    uri: &str,
    body: &str,
    content_type: &str,
) -> TestResponse {
    let app = server::build_app(&project.config()).expect("gateway app should build");
    let response = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", content_type)
                .body(Body::from(body.to_string()))
                .expect("request should build"),
        )
        .await
        .expect("request should be handled");

    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body should read");
    let raw_body = String::from_utf8_lossy(&bytes).to_string();
    let body = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);

    TestResponse {
        status,
        body,
        raw_body,
    }
}

pub async fn raw_request_with_headers(
    project: &TestProject,
    method: Method,
    uri: &str,
    body: &str,
    headers: &[(&str, &str)],
) -> TestResponse {
    let app = server::build_app(&project.config()).expect("gateway app should build");
    raw_request_with_headers_on_app(app, method, uri, body, headers).await
}

pub async fn raw_request_with_headers_on_app(
    app: Router,
    method: Method,
    uri: &str,
    body: &str,
    headers: &[(&str, &str)],
) -> TestResponse {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");

    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }

    let response = app
        .oneshot(
            builder
                .body(Body::from(body.to_string()))
                .expect("request should build"),
        )
        .await
        .expect("request should be handled");

    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body should read");
    let raw_body = String::from_utf8_lossy(&bytes).to_string();
    let body = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);

    TestResponse {
        status,
        body,
        raw_body,
    }
}

pub fn action(method: &str, path: &str, source: &str, entrypoint: &str) -> Value {
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

pub fn authorizer_action(source: &str, entrypoint: &str, name: &str) -> Value {
    json!({
        "runtime": "Python",
        "kind": {
            "Authorizer": {}
        },
        "source": source,
        "entrypoint": entrypoint,
        "name": name
    })
}

pub fn node_action(method: &str, path: &str, source: &str, entrypoint: &str) -> Value {
    json!({
        "runtime": "Node",
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

pub fn schedule_action(source: &str, entrypoint: &str, expression: &str) -> Value {
    json!({
        "runtime": "Python",
        "kind": {
            "Schedule": {
                "expression": expression
            }
        },
        "source": source,
        "entrypoint": entrypoint,
        "name": entrypoint
    })
}
