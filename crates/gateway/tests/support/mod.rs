use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    body::{to_bytes, Body},
    http::{Method, Request, StatusCode},
};
use ryvus_gateway::{server, server::GatewayServerConfig};
use ryvus_protocol::{ActionDefinition, ActionKind, ApiAction, RuntimeKind};
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
from ryvus import api_action
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
}

pub struct TextResponse {
    pub status: StatusCode,
    pub body: String,
}

pub fn assert_public_error(response: TestResponse, status: StatusCode, error: &str) {
    assert_eq!(response.status, status);
    assert_eq!(response.body["error"], json!(error));
    assert!(response.body["invocation_id"].is_string());
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
    let body = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);

    TestResponse { status, body }
}

pub async fn text_request(project: &TestProject, method: Method, uri: &str) -> TextResponse {
    let app = server::build_app(&project.config()).expect("gateway app should build");
    let response = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("request should be handled");

    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body should read");
    let body = String::from_utf8(bytes.to_vec()).expect("body should be UTF-8");

    TextResponse { status, body }
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

pub fn api_definition(
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
            request_schema: None,
            response_schema: None,
            query_params: Vec::new(),
        }),
        source: source.into(),
        entrypoint: entrypoint.to_string(),
    }
}
