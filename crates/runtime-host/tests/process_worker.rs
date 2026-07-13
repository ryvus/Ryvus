use std::{
    fs,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use ryvus_protocol::{InvocationRequest, InvocationResult, InvocationStatus};
use ryvus_runtime_host::{ProcessInvocationWorkerFactory, ProcessWorkerConfig, RuntimeHost};
use serde_json::{json, Value};
use tower::ServiceExt;

const ACTION_SOURCE: &str = r#"
import os
import time
from ryvus import api_action

@api_action
def handler(event, context):
    body = event.body or {}
    print("structured worker log")
    if body.get("crash"):
        os._exit(17)
    if body.get("fail"):
        raise ValueError("handler failed")
    if body.get("sleep_ms"):
        time.sleep(body["sleep_ms"] / 1000)
    return {
        "execution_id": context.execution_id,
        "attempt_id": context.attempt_id,
        "attempt_number": context.attempt_number,
    }
"#;

static PROCESS_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn invokes_python_worker_and_preserves_attempt_identity() {
    let _guard = PROCESS_TEST_LOCK.lock().await;
    let fixture = PythonActionFixture::new();
    let host = fixture.host();
    let request = invocation(json!({}), Duration::from_secs(5));
    let response = call(&host, &request).await;

    assert_eq!(response.0, StatusCode::OK, "{}", response_text(&response));
    let result: InvocationResult = serde_json::from_slice(&response.1).unwrap();
    assert_eq!(result.attempt(), request.attempt());
    assert_eq!(
        result.output,
        Some(json!({
            "execution_id": request.execution_id.as_ref(),
            "attempt_id": request.attempt_id.as_ref(),
            "attempt_number": request.attempt_number,
        }))
    );
    assert_eq!(host.active_attempt().await, None);
}

#[tokio::test]
async fn handler_failure_is_a_valid_terminal_result() {
    let _guard = PROCESS_TEST_LOCK.lock().await;
    let fixture = PythonActionFixture::new();
    let host = fixture.host();
    let response = call(
        &host,
        &invocation(json!({ "fail": true }), Duration::from_secs(5)),
    )
    .await;

    assert_eq!(response.0, StatusCode::OK, "{}", response_text(&response));
    let result: InvocationResult = serde_json::from_slice(&response.1).unwrap();
    assert_eq!(result.status, InvocationStatus::Failed);
    assert_eq!(result.error.unwrap().message, "handler failed");
}

#[tokio::test]
async fn timeout_terminates_and_reaps_worker_before_next_invocation() {
    let _guard = PROCESS_TEST_LOCK.lock().await;
    let fixture = PythonActionFixture::new();
    let host = fixture.host();
    let slow = invocation(json!({ "sleep_ms": 5_000 }), Duration::from_millis(750));

    let response = call(&host, &slow).await;
    assert_eq!(
        response.0,
        StatusCode::GATEWAY_TIMEOUT,
        "{}",
        response_text(&response)
    );
    assert_eq!(host.active_attempt().await, None);

    let next = invocation(json!({}), Duration::from_secs(5));
    assert_eq!(call(&host, &next).await.0, StatusCode::OK);
}

#[tokio::test]
async fn worker_crash_does_not_make_host_unhealthy() {
    let _guard = PROCESS_TEST_LOCK.lock().await;
    let fixture = PythonActionFixture::new();
    let host = fixture.host();
    let request = invocation(json!({ "crash": true }), Duration::from_secs(5));

    assert_eq!(call(&host, &request).await.0, StatusCode::BAD_GATEWAY);
    assert_eq!(host.active_attempt().await, None);

    let response = host
        .router()
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let health: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(health["max_workers"], 1);
    assert_eq!(health["active_workers"], 0);
    assert_eq!(health["available_capacity"], 1);
}

#[tokio::test]
async fn capacity_one_and_shutdown_terminate_the_active_worker() {
    let _guard = PROCESS_TEST_LOCK.lock().await;
    let fixture = PythonActionFixture::new();
    let host = fixture.host();
    let slow = invocation(json!({ "sleep_ms": 5_000 }), Duration::from_secs(10));
    let task_host = host.clone();
    let active_call = tokio::spawn(async move { call(&task_host, &slow).await });

    wait_for_active_attempt(&host).await;
    let health = host
        .router()
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let health = to_bytes(health.into_body(), usize::MAX).await.unwrap();
    let health: Value = serde_json::from_slice(&health).unwrap();
    assert_eq!(health["active_workers"], 1);
    assert_eq!(health["available_capacity"], 0);

    let second = invocation(json!({}), Duration::from_secs(5));
    assert_eq!(call(&host, &second).await.0, StatusCode::CONFLICT);

    host.shutdown().await.unwrap();
    assert_eq!(active_call.await.unwrap().0, StatusCode::BAD_GATEWAY);
    assert_eq!(host.active_attempt().await, None);
}

#[tokio::test]
async fn rejects_malformed_duplicate_and_missing_terminal_frames() {
    let _guard = PROCESS_TEST_LOCK.lock().await;
    for (script, expected) in [
        (MALFORMED_FRAME, "deserialization"),
        (DUPLICATE_RESULT, "more than one terminal result"),
        (EOF_BEFORE_RESULT, "before a terminal result"),
        (OUTPUT_AFTER_RESULT, "after its terminal result"),
    ] {
        let host = script_host(script);
        let request = invocation(json!({}), Duration::from_secs(5));
        let response = call(&host, &request).await;
        let body = response_text(&response);
        assert_eq!(response.0, StatusCode::BAD_GATEWAY, "{body}");
        assert!(body.contains(expected), "{body}");
        assert_eq!(host.active_attempt().await, None);
    }
}

const MALFORMED_FRAME: &str = r#"
import json, sys
print(json.dumps({"type":"ready"}), flush=True)
sys.stdin.readline()
print("not-json", flush=True)
"#;

const EOF_BEFORE_RESULT: &str = r#"
import json, sys
print(json.dumps({"type":"ready"}), flush=True)
sys.stdin.readline()
"#;

const DUPLICATE_RESULT: &str = r#"
import json, sys
print(json.dumps({"type":"ready"}), flush=True)
r = json.loads(sys.stdin.readline())
result = {"protocol_version":r["protocol_version"],"execution_id":r["execution_id"],"attempt_id":r["attempt_id"],"attempt_number":r["attempt_number"],"status":"success","output":{},"error":None}
frame = json.dumps({"type":"result","result":result})
print(frame, flush=True)
print(frame, flush=True)
"#;

const OUTPUT_AFTER_RESULT: &str = r#"
import json, sys
print(json.dumps({"type":"ready"}), flush=True)
r = json.loads(sys.stdin.readline())
result = {"protocol_version":r["protocol_version"],"execution_id":r["execution_id"],"attempt_id":r["attempt_id"],"attempt_number":r["attempt_number"],"status":"success","output":{},"error":None}
print(json.dumps({"type":"result","result":result}), flush=True)
print(json.dumps({"type":"event","event":{"type":"log","execution_id":r["execution_id"],"attempt_id":r["attempt_id"],"attempt_number":r["attempt_number"],"level":"info","message":"late","fields":{}}}), flush=True)
"#;

fn script_host(script: &str) -> RuntimeHost {
    RuntimeHost::new(Arc::new(ProcessInvocationWorkerFactory::new(
        ProcessWorkerConfig::new("python3")
            .arg("-u")
            .arg("-c")
            .arg(script),
    )))
}

async fn call(host: &RuntimeHost, request: &InvocationRequest) -> (StatusCode, Vec<u8>) {
    let response = host
        .router()
        .oneshot(
            Request::post("/invoke")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    (status, body)
}

fn response_text(response: &(StatusCode, Vec<u8>)) -> String {
    String::from_utf8_lossy(&response.1).into_owned()
}

async fn wait_for_active_attempt(host: &RuntimeHost) {
    let timeout = tokio::time::Instant::now() + Duration::from_secs(5);
    while host.active_attempt().await.is_none() {
        assert!(
            tokio::time::Instant::now() < timeout,
            "worker did not start"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn invocation(body: Value, budget: Duration) -> InvocationRequest {
    let mut request = InvocationRequest::new(json!({ "body": body }));
    let budget_ms = u64::try_from(budget.as_millis()).unwrap();
    request.set_deadline(now_unix_ms() + i64::try_from(budget_ms).unwrap(), budget_ms);
    request
}

fn now_unix_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

struct PythonActionFixture {
    directory: PathBuf,
    source: PathBuf,
}

impl PythonActionFixture {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!("ryvus-worker-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let source = directory.join("action.py");
        fs::write(&source, ACTION_SOURCE).unwrap();
        Self { directory, source }
    }

    fn host(&self) -> RuntimeHost {
        let sdk = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../sdk/python");
        let config = ProcessWorkerConfig::new("python3")
            .arg(self.source.to_string_lossy())
            .working_dir(&self.directory)
            .env("PYTHONPATH", sdk.to_string_lossy())
            .env("RYVUS_ENTRYPOINT", "handler");
        RuntimeHost::new(Arc::new(ProcessInvocationWorkerFactory::new(config)))
    }
}

impl Drop for PythonActionFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}
