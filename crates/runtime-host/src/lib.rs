mod deadline;
mod error;
mod process;
mod worker;

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex as StdMutex,
    },
};

use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use ryvus_protocol::{
    ActiveAttemptOwnership, AttemptId, AttemptOutcome, ControlCommandOutcome, ControlMessageId,
    ExecutionAttempt, InvocationEvent, InvocationRequest, InvocationResult, RuntimeControlCommand,
    RuntimeControlEvent, RuntimeHostId, RuntimeSessionId, TerminationReason, PROTOCOL_VERSION,
    RUNTIME_CONTROL_PROTOCOL_VERSION,
};
use serde::Serialize;
use tokio::sync::{mpsc, Mutex, Semaphore};

pub use deadline::{DeadlineValidator, ValidatedDeadline, DEFAULT_CLOCK_SKEW_TOLERANCE};
pub use error::RuntimeHostError;
pub use process::{ProcessInvocationWorker, ProcessInvocationWorkerFactory, ProcessWorkerConfig};
pub use worker::{
    InvocationWorker, InvocationWorkerFactory, StartedWorker, WorkerError, WorkerInvocation,
};

#[derive(Clone)]
pub struct RuntimeHost {
    state: Arc<HostState>,
    control_tx: mpsc::UnboundedSender<ControlRequest>,
    control_rx: Arc<StdMutex<Option<mpsc::UnboundedReceiver<ControlRequest>>>>,
}

#[derive(Clone)]
pub struct RuntimeHostControlSender {
    tx: mpsc::UnboundedSender<ControlRequest>,
}

struct ControlRequest {
    command: RuntimeControlCommand,
    response: std::sync::mpsc::Sender<RuntimeControlEvent>,
}

struct HostState {
    factory: Arc<dyn InvocationWorkerFactory>,
    deadline_validator: DeadlineValidator,
    capacity: Arc<Semaphore>,
    active: Mutex<Option<ActiveWorker>>,
    completed_events: StdMutex<HashMap<AttemptId, Vec<InvocationEvent>>>,
    terminal_attempts: Mutex<HashMap<AttemptId, AttemptOutcome>>,
    command_results: Mutex<HashMap<ControlMessageId, RuntimeControlEvent>>,
    runtime_host_id: RuntimeHostId,
    runtime_session_id: RuntimeSessionId,
    expected_attempt: Option<ActiveAttemptOwnership>,
    accepting: AtomicBool,
    stopped: AtomicBool,
    stopped_notify: tokio::sync::Notify,
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
        Self::registered(factory, RuntimeHostId::new(), RuntimeSessionId::new(), None)
    }

    pub fn registered(
        factory: Arc<dyn InvocationWorkerFactory>,
        runtime_host_id: RuntimeHostId,
        runtime_session_id: RuntimeSessionId,
        expected_attempt: Option<ActiveAttemptOwnership>,
    ) -> Self {
        let (control_tx, control_rx) = mpsc::unbounded_channel();
        Self {
            state: Arc::new(HostState {
                factory,
                deadline_validator: DeadlineValidator::default(),
                capacity: Arc::new(Semaphore::new(1)),
                active: Mutex::new(None),
                completed_events: StdMutex::new(HashMap::new()),
                terminal_attempts: Mutex::new(HashMap::new()),
                command_results: Mutex::new(HashMap::new()),
                runtime_host_id,
                runtime_session_id,
                expected_attempt,
                accepting: AtomicBool::new(true),
                stopped: AtomicBool::new(false),
                stopped_notify: tokio::sync::Notify::new(),
                max_workers: 1,
            }),
            control_tx,
            control_rx: Arc::new(StdMutex::new(Some(control_rx))),
        }
    }

    pub fn control_sender(&self) -> RuntimeHostControlSender {
        RuntimeHostControlSender {
            tx: self.control_tx.clone(),
        }
    }

    pub async fn run_control_loop(&self) {
        let mut receiver = self
            .control_rx
            .lock()
            .expect("runtime control receiver should lock")
            .take()
            .expect("runtime control loop should start once");
        while let Some(request) = receiver.recv().await {
            let result = self.state.handle_command(request.command).await;
            let _ = request.response.send(result);
        }
    }

    pub async fn wait_stopped(&self) {
        if self.state.stopped.load(Ordering::Acquire) {
            return;
        }
        self.state.stopped_notify.notified().await;
    }

    pub fn identity(&self) -> (RuntimeHostId, RuntimeSessionId) {
        (
            self.state.runtime_host_id.clone(),
            self.state.runtime_session_id.clone(),
        )
    }

    pub async fn drain(&self) {
        self.state.accepting.store(false, Ordering::Release);
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

    pub fn take_events(&self, attempt: &ExecutionAttempt) -> Vec<InvocationEvent> {
        self.state
            .completed_events
            .lock()
            .expect("completed runtime events should lock")
            .remove(&attempt.attempt_id)
            .unwrap_or_default()
    }

    pub async fn shutdown(&self) -> Result<(), WorkerError> {
        self.drain().await;
        let active = self.state.active.lock().await.clone();
        if let Some(active) = active {
            let result = active.worker.terminate(TerminationReason::Shutdown).await;
            self.state.clear_active(&active.ownership).await;
            result?;
        }
        self.state.stopped.store(true, Ordering::Release);
        self.state.stopped_notify.notify_waiters();
        Ok(())
    }
}

impl RuntimeHostControlSender {
    pub fn send(&self, command: RuntimeControlCommand) -> Result<RuntimeControlEvent, String> {
        let (response_tx, response_rx) = std::sync::mpsc::channel();
        self.tx
            .send(ControlRequest {
                command,
                response: response_tx,
            })
            .map_err(|_| "runtime control loop is unavailable".to_string())?;
        response_rx
            .recv()
            .map_err(|_| "runtime control response was dropped".to_string())
    }
}

impl HostState {
    async fn handle_command(&self, command: RuntimeControlCommand) -> RuntimeControlEvent {
        let message_id = command_message_id(&command).clone();
        if let Some(result) = self.command_results.lock().await.get(&message_id).cloned() {
            return result;
        }
        let outcome = if command.validate().is_err() {
            (
                ControlCommandOutcome::Failed,
                Some("invalid command".to_string()),
            )
        } else if command_runtime_host_id(&command) != &self.runtime_host_id {
            (
                ControlCommandOutcome::OwnershipMismatch,
                Some("runtime host id does not match".to_string()),
            )
        } else if command_runtime_session_id(&command) != &self.runtime_session_id {
            (
                ControlCommandOutcome::StaleSession,
                Some("runtime session id is stale".to_string()),
            )
        } else {
            match &command {
                RuntimeControlCommand::TerminateAttempt {
                    execution_id,
                    attempt_id,
                    attempt_number,
                    reason,
                    ..
                } => {
                    self.terminate_attempt(execution_id, attempt_id, *attempt_number, *reason)
                        .await
                }
                RuntimeControlCommand::DrainRuntime { .. } => {
                    self.accepting.store(false, Ordering::Release);
                    (ControlCommandOutcome::Confirmed, None)
                }
                RuntimeControlCommand::ShutdownRuntime { .. } => match self.shutdown_now().await {
                    Ok(()) => (ControlCommandOutcome::Confirmed, None),
                    Err(error) => (ControlCommandOutcome::Failed, Some(error.to_string())),
                },
            }
        };
        let result = RuntimeControlEvent::CommandResult {
            protocol_version: RUNTIME_CONTROL_PROTOCOL_VERSION.to_string(),
            message_id: ControlMessageId::new(),
            runtime_host_id: self.runtime_host_id.clone(),
            runtime_session_id: self.runtime_session_id.clone(),
            command_message_id: message_id.clone(),
            outcome: outcome.0,
            message: outcome.1,
        };
        self.command_results
            .lock()
            .await
            .insert(message_id, result.clone());
        result
    }

    async fn terminate_attempt(
        &self,
        execution_id: &ryvus_protocol::ExecutionId,
        attempt_id: &AttemptId,
        attempt_number: u32,
        reason: TerminationReason,
    ) -> (ControlCommandOutcome, Option<String>) {
        if self.terminal_attempts.lock().await.contains_key(attempt_id) {
            return (ControlCommandOutcome::AlreadyTerminal, None);
        }
        let Some(expected) = &self.expected_attempt else {
            return (ControlCommandOutcome::AttemptNotFound, None);
        };
        if expected.attempt_id != *attempt_id {
            return (ControlCommandOutcome::AttemptNotFound, None);
        }
        if expected.execution_id != *execution_id || expected.attempt_number != attempt_number {
            return (ControlCommandOutcome::OwnershipMismatch, None);
        }

        let active = self.active.lock().await.clone();
        if let Some(active) = active {
            if active.ownership.worker_id != expected.worker_id
                || active.ownership.attempt_id != expected.attempt_id
            {
                return (ControlCommandOutcome::OwnershipMismatch, None);
            }
            if let Err(error) = active.worker.terminate(reason).await {
                return (ControlCommandOutcome::Failed, Some(error.to_string()));
            }
            self.clear_active(&active.ownership).await;
        }
        let outcome = match reason {
            TerminationReason::Timeout => AttemptOutcome::TimedOut,
            _ => AttemptOutcome::Cancelled,
        };
        let mut terminal = self.terminal_attempts.lock().await;
        if terminal.contains_key(attempt_id) {
            (ControlCommandOutcome::AlreadyTerminal, None)
        } else {
            terminal.insert(attempt_id.clone(), outcome);
            (ControlCommandOutcome::Confirmed, None)
        }
    }

    async fn shutdown_now(&self) -> Result<(), WorkerError> {
        self.accepting.store(false, Ordering::Release);
        let active = self.active.lock().await.clone();
        if let Some(active) = active {
            active.worker.terminate(TerminationReason::Shutdown).await?;
            self.clear_active(&active.ownership).await;
            self.terminal_attempts
                .lock()
                .await
                .entry(active.ownership.attempt_id)
                .or_insert(AttemptOutcome::Cancelled);
        }
        self.stopped.store(true, Ordering::Release);
        self.stopped_notify.notify_waiters();
        Ok(())
    }

    async fn transition_terminal(&self, attempt_id: &AttemptId, outcome: AttemptOutcome) -> bool {
        let mut terminal = self.terminal_attempts.lock().await;
        if terminal.contains_key(attempt_id) {
            false
        } else {
            terminal.insert(attempt_id.clone(), outcome);
            true
        }
    }

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

fn command_message_id(command: &RuntimeControlCommand) -> &ControlMessageId {
    match command {
        RuntimeControlCommand::TerminateAttempt { message_id, .. }
        | RuntimeControlCommand::DrainRuntime { message_id, .. }
        | RuntimeControlCommand::ShutdownRuntime { message_id, .. } => message_id,
    }
}

fn command_runtime_host_id(command: &RuntimeControlCommand) -> &RuntimeHostId {
    match command {
        RuntimeControlCommand::TerminateAttempt {
            runtime_host_id, ..
        }
        | RuntimeControlCommand::DrainRuntime {
            runtime_host_id, ..
        }
        | RuntimeControlCommand::ShutdownRuntime {
            runtime_host_id, ..
        } => runtime_host_id,
    }
}

fn command_runtime_session_id(command: &RuntimeControlCommand) -> &RuntimeSessionId {
    match command {
        RuntimeControlCommand::TerminateAttempt {
            runtime_session_id, ..
        }
        | RuntimeControlCommand::DrainRuntime {
            runtime_session_id, ..
        }
        | RuntimeControlCommand::ShutdownRuntime {
            runtime_session_id, ..
        } => runtime_session_id,
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
    if state
        .terminal_attempts
        .lock()
        .await
        .contains_key(&request.attempt_id)
    {
        return Err(RuntimeHostError::Cancelled);
    }
    let capacity = Arc::clone(&state.capacity)
        .try_acquire_owned()
        .map_err(|_| RuntimeHostError::Busy)?;

    let worker_id = state
        .expected_attempt
        .as_ref()
        .filter(|ownership| {
            ownership.execution_id == request.execution_id
                && ownership.attempt_id == request.attempt_id
                && ownership.attempt_number == request.attempt_number
        })
        .map(|ownership| ownership.worker_id.clone())
        .unwrap_or_default();
    let started = state.factory.start(&request, worker_id).await?;
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
        if timed_out {
            state
                .transition_terminal(&request.attempt_id, AttemptOutcome::TimedOut)
                .await;
        }
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
    if matches!(invocation, Err(RuntimeHostError::TimedOut)) {
        state
            .transition_terminal(&request.attempt_id, AttemptOutcome::TimedOut)
            .await;
    }
    let cleanup_reason = if matches!(invocation, Err(RuntimeHostError::TimedOut)) {
        TerminationReason::Timeout
    } else {
        TerminationReason::Shutdown
    };
    let cleanup = state.cleanup(&active, cleanup_reason).await;
    if let Err(error) = cleanup {
        return Err(RuntimeHostError::Worker(error));
    }
    let invocation = invocation?;
    let result = invocation.result;

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
    for event in &invocation.events {
        if event.attempt() != request.attempt() {
            return Err(RuntimeHostError::AttemptMismatch {
                expected: request.attempt(),
                actual: event.attempt(),
            });
        }
    }
    let outcome = if result.status == ryvus_protocol::InvocationStatus::Success {
        AttemptOutcome::Succeeded
    } else {
        AttemptOutcome::Failed
    };
    if !state
        .transition_terminal(&request.attempt_id, outcome)
        .await
    {
        return match state
            .terminal_attempts
            .lock()
            .await
            .get(&request.attempt_id)
            .copied()
        {
            Some(AttemptOutcome::TimedOut) => Err(RuntimeHostError::TimedOut),
            Some(AttemptOutcome::Cancelled) => Err(RuntimeHostError::Cancelled),
            _ => Err(RuntimeHostError::Unavailable),
        };
    }
    state
        .completed_events
        .lock()
        .expect("completed runtime events should lock")
        .insert(request.attempt_id.clone(), invocation.events);
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
    use ryvus_protocol::{
        ControlCommandOutcome, ControlMessageId, InvocationEvent, InvocationRequest,
        InvocationResult, LogEvent, LogLevel, RuntimeControlCommand, RuntimeHostId,
        RuntimeSessionId, WorkerId, RUNTIME_CONTROL_PROTOCOL_VERSION,
    };
    use serde_json::json;
    use tower::ServiceExt;

    use super::*;
    use crate::worker::{MockInvocationWorker, MockInvocationWorkerFactory};

    #[tokio::test]
    async fn terminate_command_targets_exact_worker_and_is_idempotent() {
        let request = request_with_budget(Duration::from_secs(5));
        let host_id = RuntimeHostId::from("host-1");
        let session_id = RuntimeSessionId::from("session-1");
        let worker_id = WorkerId::from("worker-1");
        let host = registered_test_host(&request, &host_id, &session_id, &worker_id);
        let mut worker = MockInvocationWorker::new();
        worker.expect_terminate().once().returning(|_| Ok(()));
        *host.state.active.lock().await = Some(ActiveWorker {
            ownership: ActiveAttemptOwnership {
                execution_id: request.execution_id.clone(),
                attempt_id: request.attempt_id.clone(),
                attempt_number: request.attempt_number,
                worker_id,
            },
            worker: Arc::new(worker),
        });
        let control = tokio::spawn({
            let host = host.clone();
            async move { host.run_control_loop().await }
        });
        let command = terminate_command(&request, host_id, session_id, "command-1");

        let first = send_command(host.control_sender(), command.clone()).await;
        let duplicate = send_command(host.control_sender(), command).await;

        assert_eq!(command_result(&first), ControlCommandOutcome::Confirmed);
        assert_eq!(duplicate, first);
        assert_eq!(host.active_attempt().await, None);
        control.abort();
    }

    #[tokio::test]
    async fn terminate_rejects_ownership_mismatch_stale_attempt_and_stale_session() {
        let request = request_with_budget(Duration::from_secs(5));
        let host_id = RuntimeHostId::from("host-1");
        let session_id = RuntimeSessionId::from("session-1");
        let host =
            registered_test_host(&request, &host_id, &session_id, &WorkerId::from("worker-1"));
        let control = tokio::spawn({
            let host = host.clone();
            async move { host.run_control_loop().await }
        });

        let mut mismatch =
            terminate_command(&request, host_id.clone(), session_id.clone(), "mismatch");
        if let RuntimeControlCommand::TerminateAttempt { execution_id, .. } = &mut mismatch {
            *execution_id = ryvus_protocol::ExecutionId::new();
        }
        let mut stale_attempt = terminate_command(
            &request,
            host_id.clone(),
            session_id.clone(),
            "stale-attempt",
        );
        if let RuntimeControlCommand::TerminateAttempt { attempt_id, .. } = &mut stale_attempt {
            *attempt_id = AttemptId::new();
        }
        let mut stale_number = terminate_command(
            &request,
            host_id.clone(),
            session_id.clone(),
            "stale-number",
        );
        if let RuntimeControlCommand::TerminateAttempt { attempt_number, .. } = &mut stale_number {
            *attempt_number += 1;
        }
        let stale_session = terminate_command(
            &request,
            host_id,
            RuntimeSessionId::from("old-session"),
            "stale-session",
        );

        assert_eq!(
            command_result(&send_command(host.control_sender(), mismatch).await),
            ControlCommandOutcome::OwnershipMismatch
        );
        assert_eq!(
            command_result(&send_command(host.control_sender(), stale_attempt).await),
            ControlCommandOutcome::AttemptNotFound
        );
        assert_eq!(
            command_result(&send_command(host.control_sender(), stale_number).await),
            ControlCommandOutcome::OwnershipMismatch
        );
        assert_eq!(
            command_result(&send_command(host.control_sender(), stale_session).await),
            ControlCommandOutcome::StaleSession
        );
        control.abort();
    }

    #[tokio::test]
    async fn terminate_never_reaches_a_worker_with_mismatched_ownership() {
        let request = request_with_budget(Duration::from_secs(5));
        let host_id = RuntimeHostId::from("host-1");
        let session_id = RuntimeSessionId::from("session-1");
        let host = registered_test_host(
            &request,
            &host_id,
            &session_id,
            &WorkerId::from("expected-worker"),
        );
        let mut worker = MockInvocationWorker::new();
        worker.expect_terminate().never();
        *host.state.active.lock().await = Some(ActiveWorker {
            ownership: ActiveAttemptOwnership {
                execution_id: request.execution_id.clone(),
                attempt_id: request.attempt_id.clone(),
                attempt_number: request.attempt_number,
                worker_id: WorkerId::from("different-worker"),
            },
            worker: Arc::new(worker),
        });
        let control = tokio::spawn({
            let host = host.clone();
            async move { host.run_control_loop().await }
        });

        let result = send_command(
            host.control_sender(),
            terminate_command(&request, host_id, session_id, "command-1"),
        )
        .await;

        assert_eq!(
            command_result(&result),
            ControlCommandOutcome::OwnershipMismatch
        );
        assert!(host.active_attempt().await.is_some());
        control.abort();
    }

    #[tokio::test]
    async fn drain_changes_readiness_and_rejects_new_work() {
        let request = request_with_budget(Duration::from_secs(5));
        let host_id = RuntimeHostId::from("host-1");
        let session_id = RuntimeSessionId::from("session-1");
        let host =
            registered_test_host(&request, &host_id, &session_id, &WorkerId::from("worker-1"));
        *host.state.active.lock().await = Some(ActiveWorker {
            ownership: ActiveAttemptOwnership {
                execution_id: request.execution_id.clone(),
                attempt_id: request.attempt_id.clone(),
                attempt_number: request.attempt_number,
                worker_id: WorkerId::from("worker-1"),
            },
            worker: Arc::new(MockInvocationWorker::new()),
        });
        let control = tokio::spawn({
            let host = host.clone();
            async move { host.run_control_loop().await }
        });
        let command = RuntimeControlCommand::DrainRuntime {
            protocol_version: RUNTIME_CONTROL_PROTOCOL_VERSION.to_string(),
            message_id: ControlMessageId::from("drain-1"),
            runtime_host_id: host_id,
            runtime_session_id: session_id,
        };

        assert_eq!(
            command_result(&send_command(host.control_sender(), command).await),
            ControlCommandOutcome::Confirmed
        );
        let ready = host
            .router()
            .oneshot(Request::get("/ready").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(ready.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            host.active_attempt().await.unwrap().attempt_id,
            request.attempt_id
        );
        let invocation = host
            .router()
            .oneshot(
                Request::post("/invoke")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&request).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invocation.status(), StatusCode::SERVICE_UNAVAILABLE);
        control.abort();
    }

    #[tokio::test]
    async fn shutdown_terminates_reaps_and_clears_active_worker() {
        let request = request_with_budget(Duration::from_secs(5));
        let host_id = RuntimeHostId::from("host-1");
        let session_id = RuntimeSessionId::from("session-1");
        let host =
            registered_test_host(&request, &host_id, &session_id, &WorkerId::from("worker-1"));
        let mut worker = MockInvocationWorker::new();
        worker
            .expect_terminate()
            .once()
            .withf(|reason| *reason == TerminationReason::Shutdown)
            .returning(|_| Ok(()));
        *host.state.active.lock().await = Some(ActiveWorker {
            ownership: ActiveAttemptOwnership {
                execution_id: request.execution_id,
                attempt_id: request.attempt_id,
                attempt_number: request.attempt_number,
                worker_id: WorkerId::from("worker-1"),
            },
            worker: Arc::new(worker),
        });
        let control = tokio::spawn({
            let host = host.clone();
            async move { host.run_control_loop().await }
        });
        let command = RuntimeControlCommand::ShutdownRuntime {
            protocol_version: RUNTIME_CONTROL_PROTOCOL_VERSION.to_string(),
            message_id: ControlMessageId::from("shutdown-1"),
            runtime_host_id: host_id,
            runtime_session_id: session_id,
        };

        assert_eq!(
            command_result(&send_command(host.control_sender(), command).await),
            ControlCommandOutcome::Confirmed
        );
        assert_eq!(host.active_attempt().await, None);
        assert!(host.state.stopped.load(Ordering::Acquire));
        control.abort();
    }

    fn registered_test_host(
        request: &InvocationRequest,
        host_id: &RuntimeHostId,
        session_id: &RuntimeSessionId,
        worker_id: &WorkerId,
    ) -> RuntimeHost {
        let factory = MockInvocationWorkerFactory::new();
        RuntimeHost::registered(
            Arc::new(factory),
            host_id.clone(),
            session_id.clone(),
            Some(ActiveAttemptOwnership {
                execution_id: request.execution_id.clone(),
                attempt_id: request.attempt_id.clone(),
                attempt_number: request.attempt_number,
                worker_id: worker_id.clone(),
            }),
        )
    }

    fn terminate_command(
        request: &InvocationRequest,
        host_id: RuntimeHostId,
        session_id: RuntimeSessionId,
        message_id: &str,
    ) -> RuntimeControlCommand {
        RuntimeControlCommand::TerminateAttempt {
            protocol_version: RUNTIME_CONTROL_PROTOCOL_VERSION.to_string(),
            message_id: ControlMessageId::from(message_id),
            runtime_host_id: host_id,
            runtime_session_id: session_id,
            execution_id: request.execution_id.clone(),
            attempt_id: request.attempt_id.clone(),
            attempt_number: request.attempt_number,
            reason: TerminationReason::Cancellation,
        }
    }

    async fn send_command(
        sender: RuntimeHostControlSender,
        command: RuntimeControlCommand,
    ) -> RuntimeControlEvent {
        tokio::task::spawn_blocking(move || sender.send(command).unwrap())
            .await
            .unwrap()
    }

    fn command_result(event: &RuntimeControlEvent) -> ControlCommandOutcome {
        match event {
            RuntimeControlEvent::CommandResult { outcome, .. } => *outcome,
            _ => panic!("expected command result"),
        }
    }

    #[tokio::test]
    async fn rejects_mismatched_worker_result_and_cleans_up_ownership() {
        let request = request_with_budget(Duration::from_secs(5));
        let expected_attempt = request.attempt();
        let mut result = InvocationResult::success(&request, json!({ "ok": true }));
        result.attempt_id = ryvus_protocol::AttemptId::new();

        let mut worker = MockInvocationWorker::new();
        worker.expect_wait_ready().once().returning(|_| Ok(()));
        worker.expect_invoke().once().return_once(move |_, _| {
            Ok(WorkerInvocation {
                result,
                events: Vec::new(),
            })
        });
        worker.expect_terminate().once().returning(|_| Ok(()));

        let mut factory = MockInvocationWorkerFactory::new();
        factory.expect_start().once().return_once(move |_, _| {
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
    async fn rejects_mismatched_worker_event_identity() {
        let request = request_with_budget(Duration::from_secs(5));
        let result = InvocationResult::success(&request, json!({ "ok": true }));
        let mut worker = MockInvocationWorker::new();
        worker.expect_wait_ready().once().returning(|_| Ok(()));
        worker
            .expect_invoke()
            .once()
            .return_once(move |request, _| {
                Ok(WorkerInvocation {
                    result,
                    events: vec![InvocationEvent::Log(LogEvent {
                        execution_id: request.execution_id,
                        attempt_id: ryvus_protocol::AttemptId::new(),
                        attempt_number: request.attempt_number,
                        level: LogLevel::Info,
                        message: "wrong attempt".to_string(),
                        fields: json!({}),
                    })],
                })
            });
        worker.expect_terminate().once().returning(|_| Ok(()));
        let mut factory = MockInvocationWorkerFactory::new();
        factory.expect_start().once().return_once(move |_, _| {
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
        assert!(host.take_events(&request.attempt()).is_empty());
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
        factory.expect_start().once().return_once(move |_, _| {
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
