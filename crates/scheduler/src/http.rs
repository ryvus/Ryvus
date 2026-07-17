use std::{sync::Arc, time::SystemTime};

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use ryvus_execution::{ScheduleId, ScheduleTriggerId};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    DurableSchedulerService, ScheduleExecutor, SchedulePage, ScheduleQuery, ScheduleTriggerKind,
    SchedulerError, TriggerPage, TriggerQuery,
};

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
struct ScheduleListQuery {
    cursor: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct TriggerListQuery {
    cursor: Option<String>,
    kind: Option<ScheduleTriggerKind>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct EventListQuery {
    limit: Option<usize>,
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
    Query(query): Query<ScheduleListQuery>,
) -> Result<Json<SchedulePage>, ApiError>
where
    E: ScheduleExecutor,
{
    Ok(Json(state.service.list_schedules(ScheduleQuery {
        execution_scope_id: None,
        cursor: query.cursor.map(schedule_cursor).transpose()?,
        limit: query.limit.unwrap_or(100),
    })?))
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
    Query(query): Query<TriggerListQuery>,
) -> Result<Json<TriggerPage>, ApiError>
where
    E: ScheduleExecutor,
{
    Ok(Json(state.service.list_triggers(TriggerQuery {
        schedule_id: schedule_id(id)?,
        kind: query.kind,
        cursor: query.cursor.map(trigger_cursor).transpose()?,
        limit: query.limit.unwrap_or(100),
    })?))
}

async fn list_events<E>(
    State(state): State<SchedulerHttpState<E>>,
    Path(id): Path<String>,
    Query(query): Query<EventListQuery>,
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

fn schedule_cursor(value: String) -> Result<ScheduleId, ApiError> {
    ScheduleId::new(value).map_err(|_| ApiError::InvalidCursor)
}

fn trigger_cursor(value: String) -> Result<ScheduleTriggerId, ApiError> {
    ScheduleTriggerId::new(value).map_err(|_| ApiError::InvalidCursor)
}

enum ApiError {
    NotFound,
    InvalidCursor,
    BadRequest(String),
    Scheduler(SchedulerError),
}

impl From<SchedulerError> for ApiError {
    fn from(error: SchedulerError) -> Self {
        match error {
            SchedulerError::DurableScheduleNotFound { .. } => Self::NotFound,
            SchedulerError::InvalidCursor(_) => Self::InvalidCursor,
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
            Self::InvalidCursor => (
                StatusCode::BAD_REQUEST,
                "schedule_invalid_cursor",
                "invalid schedule cursor".into(),
            ),
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, "invalid_request", message),
            Self::Scheduler(SchedulerError::Conflict(message)) => {
                (StatusCode::CONFLICT, "schedule_conflict", message)
            }
            Self::Scheduler(
                SchedulerError::StoreBackend(_) | SchedulerError::StoreLockPoisoned,
            ) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "schedule_store_unavailable",
                "schedule store unavailable".into(),
            ),
            Self::Scheduler(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "scheduler_error",
                "scheduler request failed".into(),
            ),
        };
        (status, Json(json!({ "error": code, "message": message }))).into_response()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };

    use axum::{
        body::{to_bytes, Body},
        http::{Method, Request},
    };
    use ryvus_execution::{
        ActorRef, ExecutionScopeId, ExecutionSubmission, ExecutionTrigger, ScheduleTriggerId,
    };
    use ryvus_protocol::{ActionDefinition, ActionKind, RuntimeKind, ScheduleAction};
    use tower::ServiceExt;

    use crate::{
        MemoryScheduleStore, MisfirePolicy, MockScheduleStore, ScheduleAvailability,
        ScheduleEnablement, ScheduleExecution, SchedulePage, ScheduleRecord, ScheduleStore,
        TriggerPage,
    };

    use super::*;

    #[tokio::test]
    async fn collection_routes_return_pages_and_forward_queries() {
        let schedule_id = ScheduleId::new("schedule-1").expect("schedule id should be valid");
        let trigger_cursor =
            ScheduleTriggerId::new("trigger-1").expect("schedule trigger id should be valid");
        let mut store = MockScheduleStore::new();
        let first_cursor = schedule_id.clone();
        store
            .expect_list_schedules()
            .times(2)
            .returning(move |query| {
                assert_eq!(query.limit, 1);
                assert_eq!(
                    query.execution_scope_id.as_ref().map(AsRef::as_ref),
                    Some("trusted-scope")
                );
                if let Some(cursor) = &query.cursor {
                    assert_eq!(cursor, &first_cursor);
                }
                Ok(SchedulePage {
                    items: Vec::new(),
                    next_cursor: query.cursor.is_none().then(|| first_cursor.clone()),
                })
            });
        let expected_schedule_id = schedule_id.clone();
        let schedule_for_lookup = schedule_id.clone();
        store
            .expect_get_schedule()
            .withf(move |id| id == &schedule_for_lookup)
            .return_once(move |id| Ok(Some(schedule_record(id.clone(), "trusted-scope"))));
        let next_trigger = trigger_cursor.clone();
        store
            .expect_list_triggers()
            .withf(move |query| {
                query.schedule_id == expected_schedule_id
                    && query.kind == Some(ScheduleTriggerKind::Manual)
                    && query.cursor.is_none()
                    && query.limit == 1
            })
            .return_once(move |_| {
                Ok(TriggerPage {
                    items: Vec::new(),
                    next_cursor: Some(next_trigger),
                })
            });
        let app = app(Arc::new(store), Arc::new(RecordingExecutor::default()));

        let (status, first) = request(
            app.clone(),
            Method::GET,
            "/internal/scheduler/schedules?limit=1",
            Body::empty(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(first, json!({ "items": [], "next_cursor": "schedule-1" }));

        let (status, second) = request(
            app.clone(),
            Method::GET,
            "/internal/scheduler/schedules?limit=1&cursor=schedule-1",
            Body::empty(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(second, json!({ "items": [], "next_cursor": null }));

        let (status, triggers) = request(
            app,
            Method::GET,
            "/internal/scheduler/schedules/schedule-1/triggers?kind=manual&limit=1",
            Body::empty(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(triggers, json!({ "items": [], "next_cursor": "trigger-1" }));
    }

    #[tokio::test]
    async fn schedule_errors_are_stable_and_safe() {
        let mut store = MockScheduleStore::new();
        store.expect_get_schedule().return_once(|_| Ok(None));
        store.expect_list_schedules().return_once(|_| {
            Err(SchedulerError::StoreBackend(
                "postgres://admin:secret@database/schedules".into(),
            ))
        });
        let app = app(Arc::new(store), Arc::new(RecordingExecutor::default()));

        let (status, invalid) = request(
            app.clone(),
            Method::GET,
            "/internal/scheduler/schedules?cursor=",
            Body::empty(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(invalid["error"], "schedule_invalid_cursor");

        let (status, missing) = request(
            app.clone(),
            Method::GET,
            "/internal/scheduler/schedules/missing",
            Body::empty(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(missing["error"], "schedule_not_found");

        let (status, unavailable) = request(
            app,
            Method::GET,
            "/internal/scheduler/schedules",
            Body::empty(),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(unavailable["error"], "schedule_store_unavailable");
        assert!(!unavailable.to_string().contains("admin:secret"));
    }

    #[tokio::test]
    async fn unknown_schedule_trigger_collection_returns_not_found() {
        let app = app(
            Arc::new(MemoryScheduleStore::default()),
            Arc::new(RecordingExecutor::default()),
        );

        let (status, body) = request(
            app,
            Method::GET,
            "/internal/scheduler/schedules/missing/triggers",
            Body::empty(),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "schedule_not_found");
    }

    #[tokio::test]
    async fn cross_scope_schedule_routes_return_not_found_without_mutation() {
        let store = Arc::new(MemoryScheduleStore::default());
        let executor = Arc::new(RecordingExecutor::default());
        let other_service = service_in_scope(store.clone(), executor.clone(), "other-scope");
        other_service
            .reconcile(&[schedule_action()], SystemTime::UNIX_EPOCH)
            .expect("other scope schedule should reconcile");
        let schedule_id = other_service
            .list_schedules(ScheduleQuery {
                execution_scope_id: None,
                cursor: None,
                limit: 1,
            })
            .expect("other scope schedule should list")
            .items
            .pop()
            .expect("other scope schedule should exist")
            .schedule_id;
        other_service
            .run_now(&schedule_id, json!({}), None, SystemTime::UNIX_EPOCH)
            .expect("other scope trigger should run");
        let schedule_before = other_service
            .get_schedule(&schedule_id)
            .expect("other scope schedule should load")
            .expect("other scope schedule should exist");
        let revisions_before = other_service
            .list_revisions(&schedule_id)
            .expect("other scope revisions should list");
        let events_before = other_service
            .list_operational_events(&schedule_id, 100)
            .expect("other scope events should list");
        let triggers_before = other_service
            .list_triggers(TriggerQuery {
                schedule_id: schedule_id.clone(),
                kind: None,
                cursor: None,
                limit: 100,
            })
            .expect("other scope triggers should list");
        assert_eq!(triggers_before.items.len(), 1);

        let app = app(store, executor);
        for (method, suffix) in [
            (Method::GET, ""),
            (Method::GET, "/revisions"),
            (Method::GET, "/events"),
            (Method::GET, "/triggers"),
            (Method::POST, "/enable"),
            (Method::POST, "/disable"),
            (Method::POST, "/run"),
        ] {
            let body = if method == Method::POST {
                Body::from("{}")
            } else {
                Body::empty()
            };
            let (status, body) = request(
                app.clone(),
                method,
                &format!("/internal/scheduler/schedules/{schedule_id}{suffix}"),
                body,
            )
            .await;
            assert_eq!(status, StatusCode::NOT_FOUND, "route suffix {suffix}");
            assert_eq!(body["error"], "schedule_not_found", "route suffix {suffix}");
        }

        assert_eq!(
            other_service
                .get_schedule(&schedule_id)
                .expect("other scope schedule should load"),
            Some(schedule_before)
        );
        assert_eq!(
            other_service
                .list_revisions(&schedule_id)
                .expect("other scope revisions should list"),
            revisions_before
        );
        assert_eq!(
            other_service
                .list_operational_events(&schedule_id, 100)
                .expect("other scope events should list"),
            events_before
        );
        assert_eq!(
            other_service
                .list_triggers(TriggerQuery {
                    schedule_id,
                    kind: None,
                    cursor: None,
                    limit: 100,
                })
                .expect("other scope triggers should list"),
            triggers_before
        );
    }

    #[tokio::test]
    async fn nonempty_invalid_trigger_cursor_returns_stable_bad_request() {
        let store = Arc::new(MemoryScheduleStore::default());
        let executor = Arc::new(RecordingExecutor::default());
        let service = Arc::new(service(store, executor));
        service
            .reconcile(&[schedule_action()], SystemTime::UNIX_EPOCH)
            .expect("schedule should reconcile");
        let schedule_id = service
            .list_schedules(ScheduleQuery {
                execution_scope_id: None,
                cursor: None,
                limit: 1,
            })
            .expect("schedule should list")
            .items
            .pop()
            .expect("schedule should exist")
            .schedule_id;

        let (status, body) = request(
            scheduler_routes(service),
            Method::GET,
            &format!("/internal/scheduler/schedules/{schedule_id}/triggers?cursor=missing-trigger"),
            Body::empty(),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "schedule_invalid_cursor");
    }

    #[tokio::test]
    async fn run_now_uses_trusted_identity_instead_of_body_fields() {
        let store = Arc::new(MemoryScheduleStore::default());
        let executor = Arc::new(RecordingExecutor::default());
        let service = Arc::new(service(store.clone(), executor.clone()));
        service
            .reconcile(&[schedule_action()], SystemTime::UNIX_EPOCH)
            .expect("schedule should reconcile");
        let schedule_id = service
            .list_schedules(ScheduleQuery {
                execution_scope_id: None,
                cursor: None,
                limit: 1,
            })
            .expect("schedule should list")
            .items
            .pop()
            .expect("schedule should exist")
            .schedule_id;
        let app = scheduler_routes(service);

        let (status, _) = request(
            app,
            Method::POST,
            &format!("/internal/scheduler/schedules/{schedule_id}/run"),
            Body::from(r#"{"actor":"attacker","scope":"other","execution_id":"chosen"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let submission = executor
            .submission
            .lock()
            .expect("submission should lock")
            .clone()
            .expect("submission should be recorded");
        assert_eq!(submission.scope.as_ref(), "trusted-scope");
        assert_ne!(submission.request.execution_id.as_ref(), "chosen");
        assert!(matches!(
            submission.trigger,
            ExecutionTrigger::Manual { actor, .. } if actor.as_ref() == "trusted-actor"
        ));
    }

    fn app(store: Arc<dyn ScheduleStore>, executor: Arc<RecordingExecutor>) -> Router {
        scheduler_routes(Arc::new(service(store, executor)))
    }

    fn service(
        store: Arc<dyn ScheduleStore>,
        executor: Arc<RecordingExecutor>,
    ) -> DurableSchedulerService<RecordingExecutor> {
        service_in_scope(store, executor, "trusted-scope")
    }

    fn service_in_scope(
        store: Arc<dyn ScheduleStore>,
        executor: Arc<RecordingExecutor>,
        scope: &str,
    ) -> DurableSchedulerService<RecordingExecutor> {
        DurableSchedulerService::new(
            store,
            executor,
            ExecutionScopeId::new(scope).expect("scope should be valid"),
            ActorRef::new("trusted-actor").expect("actor should be valid"),
            "test-owner",
            Duration::from_secs(30),
        )
    }

    fn schedule_record(schedule_id: ScheduleId, scope: &str) -> ScheduleRecord {
        ScheduleRecord {
            execution_scope_id: ExecutionScopeId::new(scope).expect("scope should be valid"),
            schedule_id,
            stable_schedule_key: "daily".into(),
            display_name: "daily".into(),
            current_revision: 1,
            availability: ScheduleAvailability::Available,
            enablement: ScheduleEnablement::Enabled,
            next_trigger_at: None,
            last_scheduled_trigger_at: None,
            last_discovered_at: SystemTime::UNIX_EPOCH,
            unavailable_since: None,
            misfire_policy: MisfirePolicy::SkipMissed,
            created_at: SystemTime::UNIX_EPOCH,
            updated_at: SystemTime::UNIX_EPOCH,
            version: 0,
        }
    }

    async fn request(app: Router, method: Method, uri: &str, body: Body) -> (StatusCode, Value) {
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
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response should read");
        let body = serde_json::from_slice(&bytes).expect("response should be json");
        (status, body)
    }

    #[derive(Default)]
    struct RecordingExecutor {
        submission: Mutex<Option<ExecutionSubmission>>,
    }

    impl ScheduleExecutor for RecordingExecutor {
        fn submit(
            &self,
            _action: &ActionDefinition,
            submission: ExecutionSubmission,
        ) -> crate::SchedulerResult<ScheduleExecution> {
            let execution_id = submission.request.execution_id.clone();
            *self
                .submission
                .lock()
                .map_err(|_| SchedulerError::StoreLockPoisoned)? = Some(submission);
            Ok(ScheduleExecution {
                execution_id,
                result: None,
            })
        }
    }

    fn schedule_action() -> ActionDefinition {
        ActionDefinition {
            runtime: RuntimeKind::Python,
            kind: ActionKind::Schedule(ScheduleAction {
                key: "daily".into(),
                expression: "every 1h".into(),
            }),
            source: "src/daily.py".into(),
            entrypoint: "daily".into(),
            name: Some("daily".into()),
            policy: Default::default(),
        }
    }
}
