use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};

use crate::{FlowError, FlowService, FlowStateStore, FlowStepExecutor};

pub struct FlowHttpState<S, E> {
    service: Arc<FlowService<S, E>>,
}

impl<S, E> Clone for FlowHttpState<S, E> {
    fn clone(&self) -> Self {
        Self {
            service: Arc::clone(&self.service),
        }
    }
}

pub fn flow_routes<S, E>(service: Arc<FlowService<S, E>>) -> Router
where
    S: FlowStateStore,
    E: FlowStepExecutor,
{
    Router::new()
        .route("/internal/flows", get(list_flows::<S, E>))
        .route("/internal/flows/{key}/runs", post(start_flow::<S, E>))
        .route("/internal/flows/runs/{id}", get(get_run::<S, E>))
        .route("/internal/flows/runs/{id}/cancel", post(cancel_run::<S, E>))
        .route(
            "/internal/flows/runs/{id}/steps/{step_key}/retry",
            post(retry_failed_step::<S, E>),
        )
        .with_state(FlowHttpState { service })
}

async fn list_flows<S, E>(State(state): State<FlowHttpState<S, E>>) -> Json<Value>
where
    S: FlowStateStore,
    E: FlowStepExecutor,
{
    Json(json!({ "flows": state.service.list_flows() }))
}

async fn start_flow<S, E>(
    State(state): State<FlowHttpState<S, E>>,
    Path(key): Path<String>,
    Json(input): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)>
where
    S: FlowStateStore,
    E: FlowStepExecutor,
{
    state
        .service
        .start_flow(&key, input)
        .map(|response| Json(json!(response)))
        .map_err(flow_error_response)
}

async fn get_run<S, E>(
    State(state): State<FlowHttpState<S, E>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)>
where
    S: FlowStateStore,
    E: FlowStepExecutor,
{
    state
        .service
        .get_run(&id)
        .map(|execution| Json(json!(execution)))
        .map_err(flow_error_response)
}

async fn cancel_run<S, E>(
    State(state): State<FlowHttpState<S, E>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)>
where
    S: FlowStateStore,
    E: FlowStepExecutor,
{
    state
        .service
        .cancel_run(&id)
        .map(|execution| Json(json!(execution)))
        .map_err(flow_error_response)
}

async fn retry_failed_step<S, E>(
    State(state): State<FlowHttpState<S, E>>,
    Path((id, step_key)): Path<(String, String)>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)>
where
    S: FlowStateStore,
    E: FlowStepExecutor,
{
    state
        .service
        .retry_failed_step(&id, &step_key)
        .map(|execution| Json(json!(execution)))
        .map_err(flow_error_response)
}

fn flow_error_response(error: FlowError) -> (StatusCode, Json<Value>) {
    let status = match error {
        FlowError::FlowNotFound { .. } | FlowError::RunNotFound { .. } => StatusCode::NOT_FOUND,
        FlowError::InvalidFlow { .. } | FlowError::InvalidStep { .. } => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };

    (
        status,
        Json(json!({
            "error": "flow_error",
            "message": error.to_string(),
        })),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant, SystemTime};

    use axum::{
        body::{to_bytes, Body},
        http::{Method, Request, StatusCode},
    };
    use ryvus_execution::{ExecutionRecord, ExecutionResult, ExecutionTarget};
    use ryvus_protocol::{
        ActionDefinition, ActionKind, ApiAction, InvocationRequest, InvocationResult,
        InvocationStatus, RuntimeKind, PROTOCOL_VERSION,
    };
    use serde_json::json;
    use tower::ServiceExt;

    use crate::{
        executor::FlowStepExecutor, FlowDefinition, FlowResult, FlowService, FlowSpec, FlowStep,
        InMemoryFlowStateStore,
    };

    use super::*;

    #[tokio::test]
    async fn starts_and_reads_flow_run() {
        let service = Arc::new(
            FlowService::new(
                FlowSpec {
                    flows: vec![FlowDefinition {
                        key: "restock".to_string(),
                        description: None,
                        version: None,
                        steps: vec![FlowStep {
                            key: "sync".to_string(),
                            action: "sync".to_string(),
                            policy: ryvus_protocol::ActionExecutionPolicy::default(),
                            params: json!({}),
                            config: json!({}),
                            next: None,
                            next_when: Vec::new(),
                            otherwise: None,
                            on_error: None,
                            end: None,
                        }],
                    }],
                },
                vec![api_action("sync")],
                Arc::new(InMemoryFlowStateStore::default()),
                Arc::new(RecordingFlowExecutor::default()),
            )
            .expect("service should build"),
        );
        let app = flow_routes(service);

        let started = request_json(
            app.clone(),
            Method::POST,
            "/internal/flows/restock/runs",
            Body::from(r#"{"sku":"abc"}"#),
        )
        .await;

        assert_eq!(started["flow_key"], "restock");
        let id = started["id"].as_str().expect("id should be string");
        let run = wait_for_run(app, id).await;
        assert_eq!(run["status"], "succeeded");
        assert_eq!(run["steps"][0]["key"], "sync");
    }

    async fn wait_for_run(app: Router, id: &str) -> serde_json::Value {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let run = request_json(
                app.clone(),
                Method::GET,
                &format!("/internal/flows/runs/{id}"),
                Body::empty(),
            )
            .await;
            if run["status"] == "succeeded" {
                return run;
            }
            assert!(Instant::now() < deadline, "flow did not finish");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn request_json(app: Router, method: Method, uri: &str, body: Body) -> serde_json::Value {
        let response = app
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(body)
                    .expect("request should build"),
            )
            .await
            .expect("request should be handled");

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should read");
        serde_json::from_slice(&bytes).expect("body should be json")
    }

    fn api_action(entrypoint: &str) -> ActionDefinition {
        ActionDefinition {
            runtime: RuntimeKind::Python,
            kind: ActionKind::Api(ApiAction {
                method: "POST".to_string(),
                path: format!("/{entrypoint}"),
                request_schema: None,
                response_schema: None,
                query_params: Vec::new(),
            }),
            source: format!("src/{entrypoint}.py").into(),
            entrypoint: entrypoint.to_string(),
            name: Some(entrypoint.to_string()),
            policy: ryvus_protocol::ActionExecutionPolicy::default(),
        }
    }

    #[derive(Default)]
    struct RecordingFlowExecutor {
        requests: Mutex<Vec<InvocationRequest>>,
    }

    impl FlowStepExecutor for RecordingFlowExecutor {
        fn execute_flow_step(
            &self,
            _action: &ActionDefinition,
            request: &InvocationRequest,
            _policy: &ryvus_execution::ExecutionPolicy,
        ) -> FlowResult<ExecutionRecord> {
            self.requests
                .lock()
                .expect("requests should lock")
                .push(request.clone());

            Ok(execution_record(
                request,
                InvocationResult {
                    protocol_version: PROTOCOL_VERSION.to_string(),
                    invocation_id: request.invocation_id.clone(),
                    status: InvocationStatus::Success,
                    output: Some(json!({ "ok": true })),
                    error: None,
                },
            ))
        }
    }

    fn execution_record(
        request: &InvocationRequest,
        invocation_result: InvocationResult,
    ) -> ExecutionRecord {
        let now = SystemTime::now();
        ExecutionRecord::new(
            request.clone(),
            ExecutionTarget::Process {
                command: "test".to_string(),
                args: Vec::new(),
                working_dir: None,
                env: Default::default(),
            },
            ExecutionResult {
                invocation_result,
                stdout: String::new(),
                stderr: String::new(),
                duration: Duration::from_millis(1),
                exit_code: Some(0),
            },
            now,
            now,
        )
    }
}
