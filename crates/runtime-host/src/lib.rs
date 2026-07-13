mod deadline;
mod error;
mod process;
mod worker;

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use ryvus_protocol::{
    ActiveAttemptOwnership, InvocationRequest, InvocationResult, TerminationReason,
    PROTOCOL_VERSION,
};
use serde::Serialize;
use tokio::sync::{Mutex, Semaphore};

pub use deadline::{DeadlineValidator, ValidatedDeadline, DEFAULT_CLOCK_SKEW_TOLERANCE};
pub use error::RuntimeHostError;
pub use process::{ProcessInvocationWorker, ProcessInvocationWorkerFactory, ProcessWorkerConfig};
pub use worker::{InvocationWorker, InvocationWorkerFactory, StartedWorker, WorkerError};

#[derive(Clone)]
pub struct RuntimeHost {
    state: Arc<HostState>,
}

struct HostState {
    factory: Arc<dyn InvocationWorkerFactory>,
    deadline_validator: DeadlineValidator,
    capacity: Arc<Semaphore>,
    active: Mutex<Option<ActiveWorker>>,
    accepting: AtomicBool,
    max_workers: usize,
}

#[derive(Clone)]
struct ActiveWorker {
    ownership: ActiveAttemptOwnership,
    worker: Arc<dyn InvocationWorker>,
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    busy: bool,
    max_workers: usize,
    active_workers: usize,
    available_capacity: usize,
}

impl RuntimeHost {
    pub fn new(factory: Arc<dyn InvocationWorkerFactory>) -> Self {
        Self {
            state: Arc::new(HostState {
                factory,
                deadline_validator: DeadlineValidator::default(),
                capacity: Arc::new(Semaphore::new(1)),
                active: Mutex::new(None),
                accepting: AtomicBool::new(true),
                max_workers: 1,
            }),
        }
    }

    pub fn router(&self) -> Router {
        Router::new()
            .route("/health", get(health))
            .route("/ready", get(ready))
            .route("/invoke", post(invoke))
            .with_state(Arc::clone(&self.state))
    }

    pub async fn active_attempt(&self) -> Option<ActiveAttemptOwnership> {
        self.state
            .active
            .lock()
            .await
            .as_ref()
            .map(|active| active.ownership.clone())
    }

    pub async fn shutdown(&self) -> Result<(), WorkerError> {
        self.state.accepting.store(false, Ordering::Release);
        let active = self.state.active.lock().await.clone();
        if let Some(active) = active {
            let result = active.worker.terminate(TerminationReason::Shutdown).await;
            self.state.clear_active(&active.ownership).await;
            result?;
        }
        Ok(())
    }
}

impl HostState {
    async fn clear_active(&self, ownership: &ActiveAttemptOwnership) {
        let mut active = self.active.lock().await;
        if active.as_ref().is_some_and(|current| {
            current.ownership.attempt_id == ownership.attempt_id
                && current.ownership.worker_id == ownership.worker_id
        }) {
            *active = None;
        }
    }

    async fn cleanup(
        &self,
        active: &ActiveWorker,
        reason: TerminationReason,
    ) -> Result<(), WorkerError> {
        let result = active.worker.terminate(reason).await;
        self.clear_active(&active.ownership).await;
        result
    }
}

async fn health(State(state): State<Arc<HostState>>) -> Json<HealthResponse> {
    let active_workers = usize::from(state.active.lock().await.is_some());
    let accepting = state.accepting.load(Ordering::Acquire);
    Json(HealthResponse {
        status: "healthy",
        busy: active_workers != 0,
        max_workers: state.max_workers,
        active_workers,
        available_capacity: usize::from(accepting) * state.capacity.available_permits(),
    })
}

async fn ready(State(state): State<Arc<HostState>>) -> impl IntoResponse {
    let accepting = state.accepting.load(Ordering::Acquire);
    let busy = state.active.lock().await.is_some();
    let status = if accepting && !busy {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(HealthResponse {
            status: if status == StatusCode::OK {
                "ready"
            } else {
                "not_ready"
            },
            busy,
            max_workers: state.max_workers,
            active_workers: usize::from(busy),
            available_capacity: usize::from(accepting) * state.capacity.available_permits(),
        }),
    )
}

async fn invoke(
    State(state): State<Arc<HostState>>,
    Json(request): Json<InvocationRequest>,
) -> Result<Json<InvocationResult>, RuntimeHostError> {
    validate_request(&request)?;
    let deadline = state.deadline_validator.validate(&request)?;
    if !state.accepting.load(Ordering::Acquire) {
        return Err(RuntimeHostError::Unavailable);
    }
    let capacity = Arc::clone(&state.capacity)
        .try_acquire_owned()
        .map_err(|_| RuntimeHostError::Busy)?;

    let started = state.factory.start(&request).await?;
    let ownership = ActiveAttemptOwnership {
        execution_id: request.execution_id.clone(),
        attempt_id: request.attempt_id.clone(),
        attempt_number: request.attempt_number,
        worker_id: started.worker_id,
    };
    let active = ActiveWorker {
        ownership: ownership.clone(),
        worker: started.worker,
    };
    *state.active.lock().await = Some(active.clone());

    if !state.accepting.load(Ordering::Acquire) {
        state.cleanup(&active, TerminationReason::Shutdown).await?;
        return Err(RuntimeHostError::Unavailable);
    }

    let task_state = Arc::clone(&state);
    let recovery = active.clone();
    let task = tokio::spawn(async move {
        let _capacity = capacity;
        supervise_attempt(task_state, active, request, deadline.monotonic).await
    });
    match task.await {
        Ok(result) => result,
        Err(error) => {
            state
                .cleanup(&recovery, TerminationReason::Shutdown)
                .await?;
            Err(RuntimeHostError::Supervision(error))
        }
    }
}

async fn supervise_attempt(
    state: Arc<HostState>,
    active: ActiveWorker,
    request: InvocationRequest,
    deadline: tokio::time::Instant,
) -> Result<Json<InvocationResult>, RuntimeHostError> {
    let readiness = tokio::select! {
        biased;
        _ = tokio::time::sleep_until(deadline) => Err(WorkerError::DeadlineExpired),
        result = active.worker.wait_ready(deadline) => result,
    };
    if let Err(error) = readiness {
        let timed_out = worker_timed_out(&error);
        state
            .cleanup(
                &active,
                if timed_out {
                    TerminationReason::Timeout
                } else {
                    TerminationReason::Shutdown
                },
            )
            .await?;
        return Err(if timed_out {
            RuntimeHostError::TimedOut
        } else {
            RuntimeHostError::Worker(error)
        });
    }

    let invocation = tokio::select! {
        biased;
        _ = tokio::time::sleep_until(deadline) => Err(RuntimeHostError::TimedOut),
        result = active.worker.invoke(request.clone(), deadline) => {
            result.map_err(|error| {
                if worker_timed_out(&error) {
                    RuntimeHostError::TimedOut
                } else {
                    RuntimeHostError::Worker(error)
                }
            })
        }
    };
    let cleanup_reason = if matches!(invocation, Err(RuntimeHostError::TimedOut)) {
        TerminationReason::Timeout
    } else {
        TerminationReason::Shutdown
    };
    let cleanup = state.cleanup(&active, cleanup_reason).await;
    if let Err(error) = cleanup {
        return Err(RuntimeHostError::Worker(error));
    }
    let result = invocation?;

    if result.protocol_version != PROTOCOL_VERSION {
        return Err(RuntimeHostError::WorkerProtocolMismatch {
            actual: result.protocol_version,
        });
    }
    if result.attempt() != request.attempt() {
        return Err(RuntimeHostError::AttemptMismatch {
            expected: request.attempt(),
            actual: result.attempt(),
        });
    }
    Ok(Json(result))
}

fn validate_request(request: &InvocationRequest) -> Result<(), RuntimeHostError> {
    if request.protocol_version != PROTOCOL_VERSION {
        return Err(RuntimeHostError::InvalidProtocolVersion {
            actual: request.protocol_version.clone(),
        });
    }
    if request.execution_id.as_ref().trim().is_empty() {
        return Err(RuntimeHostError::InvalidIdentity(
            "execution_id is empty".to_string(),
        ));
    }
    if request.attempt_id.as_ref().trim().is_empty() {
        return Err(RuntimeHostError::InvalidIdentity(
            "attempt_id is empty".to_string(),
        ));
    }
    if request.attempt_number == 0 {
        return Err(RuntimeHostError::InvalidIdentity(
            "attempt_number must be greater than zero".to_string(),
        ));
    }
    Ok(())
}

fn worker_timed_out(error: &WorkerError) -> bool {
    matches!(error, WorkerError::DeadlineExpired)
}

#[cfg(test)]
mod tests {
    use std::{
        sync::Arc,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use ryvus_protocol::{InvocationRequest, InvocationResult, WorkerId};
    use serde_json::json;
    use tower::ServiceExt;

    use super::*;
    use crate::worker::{MockInvocationWorker, MockInvocationWorkerFactory};

    #[tokio::test]
    async fn rejects_mismatched_worker_result_and_cleans_up_ownership() {
        let request = request_with_budget(Duration::from_secs(5));
        let expected_attempt = request.attempt();
        let mut result = InvocationResult::success(&request, json!({ "ok": true }));
        result.attempt_id = ryvus_protocol::AttemptId::new();

        let mut worker = MockInvocationWorker::new();
        worker.expect_wait_ready().once().returning(|_| Ok(()));
        worker
            .expect_invoke()
            .once()
            .return_once(move |_, _| Ok(result));
        worker.expect_terminate().once().returning(|_| Ok(()));

        let mut factory = MockInvocationWorkerFactory::new();
        factory.expect_start().once().return_once(move |_| {
            Ok(StartedWorker {
                worker_id: WorkerId::new(),
                worker: Arc::new(worker),
            })
        });
        let host = RuntimeHost::new(Arc::new(factory));
        let response = host
            .router()
            .oneshot(
                Request::post("/invoke")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&request).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(host.active_attempt().await, None);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains("attempt mismatch"));
        assert_eq!(request.attempt(), expected_attempt);
    }

    #[tokio::test]
    async fn expired_request_never_starts_a_worker() {
        let mut factory = MockInvocationWorkerFactory::new();
        factory.expect_start().never();
        let host = RuntimeHost::new(Arc::new(factory));
        let mut request = InvocationRequest::new(json!({}));
        request.set_deadline(now_unix_ms() - 1, 1_000);

        let response = host
            .router()
            .oneshot(
                Request::post("/invoke")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&request).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
        assert_eq!(host.active_attempt().await, None);
    }

    #[tokio::test]
    async fn v1_and_v2_requests_fail_closed_before_worker_startup() {
        for version in ["ryvus.invoke.v1", "ryvus.invoke.v2"] {
            let mut factory = MockInvocationWorkerFactory::new();
            factory.expect_start().never();
            let host = RuntimeHost::new(Arc::new(factory));
            let mut request = request_with_budget(Duration::from_secs(5));
            request.protocol_version = version.to_string();

            let response = host
                .router()
                .oneshot(
                    Request::post("/invoke")
                        .header("content-type", "application/json")
                        .body(Body::from(serde_json::to_vec(&request).unwrap()))
                        .unwrap(),
                )
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
    }

    #[tokio::test]
    async fn supervision_failure_still_terminates_worker_and_clears_ownership() {
        let request = request_with_budget(Duration::from_secs(5));
        let mut worker = MockInvocationWorker::new();
        worker.expect_wait_ready().once().returning(|_| Ok(()));
        worker
            .expect_invoke()
            .once()
            .return_once(|_, _| panic!("worker task panic"));
        worker.expect_terminate().once().returning(|_| Ok(()));

        let mut factory = MockInvocationWorkerFactory::new();
        factory.expect_start().once().return_once(move |_| {
            Ok(StartedWorker {
                worker_id: WorkerId::new(),
                worker: Arc::new(worker),
            })
        });
        let host = RuntimeHost::new(Arc::new(factory));
        let response = host
            .router()
            .oneshot(
                Request::post("/invoke")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&request).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(host.active_attempt().await, None);
    }

    fn request_with_budget(budget: Duration) -> InvocationRequest {
        let mut request = InvocationRequest::new(json!({}));
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
}
