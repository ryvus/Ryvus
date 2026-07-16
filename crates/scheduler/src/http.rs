use std::{sync::Arc, time::SystemTime};

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use ryvus_execution::ScheduleId;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{DurableSchedulerService, ScheduleExecutor, ScheduleTriggerKind, SchedulerError};

pub struct SchedulerHttpState<E> {
    service: Arc<DurableSchedulerService<E>>,
}

impl<E> Clone for SchedulerHttpState<E> {
    fn clone(&self) -> Self {
        Self {
            service: Arc::clone(&self.service),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    limit: Option<usize>,
    kind: Option<ScheduleTriggerKind>,
}

#[derive(Debug, Serialize)]
pub struct ScheduleRunResponse {
    pub execution_id: ryvus_protocol::ExecutionId,
    pub attempt_id: Option<ryvus_protocol::AttemptId>,
    pub attempt_number: Option<u32>,
    pub status: String,
    pub output: Option<Value>,
}

pub fn scheduler_routes<E>(service: Arc<DurableSchedulerService<E>>) -> Router
where
    E: ScheduleExecutor,
{
    Router::new()
        .route("/internal/scheduler/schedules", get(list_schedules::<E>))
        .route("/internal/scheduler/schedules/{id}", get(get_schedule::<E>))
        .route(
            "/internal/scheduler/schedules/{id}/revisions",
            get(list_revisions::<E>),
        )
        .route(
            "/internal/scheduler/schedules/{id}/triggers",
            get(list_triggers::<E>),
        )
        .route(
            "/internal/scheduler/schedules/{id}/events",
            get(list_events::<E>),
        )
        .route(
            "/internal/scheduler/schedules/{id}/enable",
            post(enable::<E>),
        )
        .route(
            "/internal/scheduler/schedules/{id}/disable",
            post(disable::<E>),
        )
        .route(
            "/internal/scheduler/schedules/{id}/run",
            post(run_schedule::<E>),
        )
        .with_state(SchedulerHttpState { service })
}

async fn list_schedules<E>(
    State(state): State<SchedulerHttpState<E>>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Value>, ApiError>
where
    E: ScheduleExecutor,
{
    Ok(Json(json!(state
        .service
        .list_schedules(query.limit.unwrap_or(100))?)))
}

async fn get_schedule<E>(
    State(state): State<SchedulerHttpState<E>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError>
where
    E: ScheduleExecutor,
{
    let id = schedule_id(id)?;
    let schedule = state.service.get_schedule(&id)?.ok_or(ApiError::NotFound)?;
    Ok(Json(json!(schedule)))
}

async fn list_revisions<E>(
    State(state): State<SchedulerHttpState<E>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError>
where
    E: ScheduleExecutor,
{
    Ok(Json(json!(state
        .service
        .list_revisions(&schedule_id(id)?)?)))
}

async fn list_triggers<E>(
    State(state): State<SchedulerHttpState<E>>,
    Path(id): Path<String>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Value>, ApiError>
where
    E: ScheduleExecutor,
{
    Ok(Json(json!(state.service.list_triggers(
        &schedule_id(id)?,
        query.kind,
        query.limit.unwrap_or(100),
    )?)))
}

async fn list_events<E>(
    State(state): State<SchedulerHttpState<E>>,
    Path(id): Path<String>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Value>, ApiError>
where
    E: ScheduleExecutor,
{
    Ok(Json(json!(state.service.list_operational_events(
        &schedule_id(id)?,
        query.limit.unwrap_or(100),
    )?)))
}

async fn enable<E>(
    State(state): State<SchedulerHttpState<E>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError>
where
    E: ScheduleExecutor,
{
    Ok(Json(json!(state
        .service
        .enable(&schedule_id(id)?, SystemTime::now())?)))
}

async fn disable<E>(
    State(state): State<SchedulerHttpState<E>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError>
where
    E: ScheduleExecutor,
{
    Ok(Json(json!(state
        .service
        .disable(&schedule_id(id)?, SystemTime::now())?)))
}

async fn run_schedule<E>(
    State(state): State<SchedulerHttpState<E>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> Result<Json<ScheduleRunResponse>, ApiError>
where
    E: ScheduleExecutor,
{
    let idempotency_key = headers
        .get("idempotency-key")
        .map(|value| value.to_str())
        .transpose()
        .map_err(|_| ApiError::BadRequest("invalid Idempotency-Key header".into()))?;
    let execution = state.service.run_now(
        &schedule_id(id)?,
        body.map(|Json(value)| value).unwrap_or_else(|| json!({})),
        idempotency_key,
        SystemTime::now(),
    )?;
    let (attempt_id, attempt_number, status, output) = execution.result.map_or_else(
        || (None, None, "existing".to_string(), None),
        |result| {
            (
                Some(result.attempt_id),
                Some(result.attempt_number),
                match result.status {
                    ryvus_protocol::InvocationStatus::Success => "success",
                    ryvus_protocol::InvocationStatus::Failed => "failed",
                }
                .to_string(),
                result.output,
            )
        },
    );
    Ok(Json(ScheduleRunResponse {
        execution_id: execution.execution_id,
        attempt_id,
        attempt_number,
        status,
        output,
    }))
}

fn schedule_id(value: String) -> Result<ScheduleId, ApiError> {
    ScheduleId::new(value).map_err(|error| ApiError::BadRequest(error.to_string()))
}

enum ApiError {
    NotFound,
    BadRequest(String),
    Scheduler(SchedulerError),
}

impl From<SchedulerError> for ApiError {
    fn from(error: SchedulerError) -> Self {
        match error {
            SchedulerError::DurableScheduleNotFound { .. } => Self::NotFound,
            other => Self::Scheduler(other),
        }
    }
}

impl axum::response::IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let (status, code, message) = match self {
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                "schedule_not_found",
                "schedule not found".into(),
            ),
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, "invalid_request", message),
            Self::Scheduler(SchedulerError::Conflict(message)) => {
                (StatusCode::CONFLICT, "schedule_conflict", message)
            }
            Self::Scheduler(error) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "scheduler_error",
                error.to_string(),
            ),
        };
        (status, Json(json!({ "error": code, "message": message }))).into_response()
    }
}
