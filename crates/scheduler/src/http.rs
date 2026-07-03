use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use ryvus_protocol::{ActionDefinition, InvocationStatus};
use serde::Serialize;
use serde_json::{json, Value};

use crate::{run_schedule_once, schedule_infos, ScheduleExecutor, SchedulerError};

pub struct SchedulerService<E> {
    actions: Vec<ActionDefinition>,
    executor: Arc<E>,
}

impl<E> SchedulerService<E> {
    pub fn new(actions: Vec<ActionDefinition>, executor: Arc<E>) -> Self {
        Self { actions, executor }
    }
}

pub struct SchedulerState<E> {
    service: Arc<SchedulerService<E>>,
}

impl<E> Clone for SchedulerState<E> {
    fn clone(&self) -> Self {
        Self {
            service: Arc::clone(&self.service),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ScheduleRunResponse {
    pub invocation_id: String,
    pub status: String,
    pub output: Option<Value>,
}

pub fn scheduler_routes<E>(service: Arc<SchedulerService<E>>) -> Router
where
    E: ScheduleExecutor,
{
    Router::new()
        .route("/internal/scheduler/schedules", get(list_schedules::<E>))
        .route(
            "/internal/scheduler/schedules/{id}/run",
            post(run_schedule::<E>),
        )
        .with_state(SchedulerState { service })
}

async fn list_schedules<E>(
    State(state): State<SchedulerState<E>>,
) -> Result<Json<Value>, StatusCode>
where
    E: ScheduleExecutor,
{
    let schedules = schedule_infos(&state.service.actions)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .into_iter()
        .map(|schedule| {
            json!({
                "id": schedule.id,
                "name": schedule.name,
                "source": schedule.source,
                "entrypoint": schedule.entrypoint,
                "expression": schedule.expression,
                "action_key": schedule.action_key,
            })
        })
        .collect::<Vec<_>>();

    Ok(Json(json!(schedules)))
}

async fn run_schedule<E>(
    State(state): State<SchedulerState<E>>,
    Path(id): Path<String>,
) -> Result<Json<ScheduleRunResponse>, (StatusCode, Json<Value>)>
where
    E: ScheduleExecutor,
{
    let result = run_schedule_once(
        &state.service.actions,
        &id,
        Arc::clone(&state.service.executor),
    )
    .map_err(|error| {
        let status = match error {
            SchedulerError::ScheduleNotFound { .. } => StatusCode::NOT_FOUND,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };

        (
            status,
            Json(json!({
                "error": "schedule_execution_failed",
                "message": error.to_string(),
            })),
        )
    })?;

    Ok(Json(ScheduleRunResponse {
        invocation_id: result.invocation_id,
        status: status_label(&result.status).to_string(),
        output: result.output,
    }))
}

fn status_label(status: &InvocationStatus) -> &'static str {
    match status {
        InvocationStatus::Success => "success",
        InvocationStatus::Failed => "failed",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use axum::{
        body::{to_bytes, Body},
        http::{Method, Request, StatusCode},
    };
    use ryvus_protocol::{
        ActionDefinition, ActionKind, InvocationRequest, InvocationResult, InvocationStatus,
        RuntimeKind, ScheduleAction, PROTOCOL_VERSION,
    };
    use serde_json::json;
    use tower::ServiceExt;

    use crate::{ScheduleExecutor, SchedulerResult};

    use super::*;

    #[tokio::test]
    async fn serves_scheduler_routes() {
        let service = Arc::new(SchedulerService::new(
            vec![schedule_action()],
            Arc::new(RecordingScheduleExecutor::default()),
        ));
        let app = scheduler_routes(service);

        let schedules =
            request_json(app.clone(), Method::GET, "/internal/scheduler/schedules").await;
        assert_eq!(schedules[0]["id"], json!("restock_report"));
        assert_eq!(schedules[0]["expression"], json!("every 10s"));

        let run = request_json(
            app,
            Method::POST,
            "/internal/scheduler/schedules/restock_report/run",
        )
        .await;
        assert_eq!(run["status"], json!("success"));
        assert_eq!(run["output"]["expression"], json!("every 10s"));
    }

    async fn request_json(app: Router, method: Method, uri: &str) -> Value {
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

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should read");
        serde_json::from_slice(&bytes).expect("body should be json")
    }

    fn schedule_action() -> ActionDefinition {
        ActionDefinition {
            runtime: RuntimeKind::Python,
            kind: ActionKind::Schedule(ScheduleAction {
                expression: "every 10s".to_string(),
            }),
            source: "src/restock.py".into(),
            entrypoint: "restock_report".to_string(),
            name: Some("restock_report".to_string()),
        }
    }

    #[derive(Default)]
    struct RecordingScheduleExecutor {
        requests: Mutex<Vec<InvocationRequest>>,
    }

    impl ScheduleExecutor for RecordingScheduleExecutor {
        fn execute_scheduled(
            &self,
            _action: &ActionDefinition,
            request: &InvocationRequest,
        ) -> SchedulerResult<InvocationResult> {
            self.requests
                .lock()
                .expect("requests should lock")
                .push(request.clone());

            Ok(InvocationResult {
                protocol_version: PROTOCOL_VERSION.to_string(),
                invocation_id: request.invocation_id.clone(),
                status: InvocationStatus::Success,
                output: Some(json!({
                    "expression": request.event["expression"],
                })),
                error: None,
            })
        }
    }
}
