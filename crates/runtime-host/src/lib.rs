mod deadline;
mod error;
mod logging;
mod process;
mod websocket_control;
mod worker;

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::SyncSender,
        Arc, Mutex as StdMutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
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
    ExecutionAttempt, InvocationEvent, InvocationRequest, InvocationResult, LogEvent, LogLevel,
    RuntimeControlCommand, RuntimeControlEvent, RuntimeHostId, RuntimeRegistration,
    RuntimeSessionId, TerminationReason, PROTOCOL_VERSION, RUNTIME_CONTROL_PROTOCOL_VERSION,
};
use serde::Serialize;
use serde_json::json;
use tokio::sync::{broadcast, mpsc, oneshot, Mutex, Semaphore};

pub use deadline::{DeadlineValidator, ValidatedDeadline, DEFAULT_CLOCK_SKEW_TOLERANCE};
pub use error::RuntimeHostError;
pub use logging::{
    normalize_log_event, LogNormalizationLimits, LogOverflowPolicy, RuntimeLogWriter,
    RuntimeLogWriterConfig, RuntimeLogWriterError,
};
pub use process::{ProcessInvocationWorker, ProcessInvocationWorkerFactory, ProcessWorkerConfig};
pub use ryvus_logging::{ExecutionLogRecord, ExecutionLogStore, RuntimeLogContext};
pub use websocket_control::{
    WebSocketHeaderProvider, WebSocketRuntimeHostClient, WebSocketRuntimeHostError,
};
pub use worker::{
    InvocationWorker, InvocationWorkerFactory, StartedWorker, WorkerError, WorkerEventConsumer,
};

const MAX_METRICS_PER_INVOCATION: usize = 1024;
const PRODUCER_SHUTDOWN_GRACE: Duration = Duration::from_secs(1);

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
    response: ControlResponse,
}

enum ControlResponse {
    Blocking(std::sync::mpsc::Sender<RuntimeControlEvent>),
    Async(oneshot::Sender<RuntimeControlEvent>),
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
    runtime_session_id: StdMutex<Option<RuntimeSessionId>>,
    log_context: Option<RuntimeLogContext>,
    log_writer: Option<Arc<RuntimeLogWriter>>,
    producers: Arc<ProducerTracker>,
    startups: Arc<LifecycleBarrier>,
    startup_cancel: tokio::sync::watch::Sender<bool>,
    startup_admission: StdMutex<bool>,
    shutdown: Mutex<()>,
    shutdown_result: StdMutex<Option<Result<(), String>>>,
    control_events: broadcast::Sender<RuntimeControlEvent>,
    expected_attempt: Option<ActiveAttemptOwnership>,
    accepting: AtomicBool,
    draining: AtomicBool,
    stopped: AtomicBool,
    stopped_notify: tokio::sync::Notify,
    max_workers: usize,
}

#[derive(Clone)]
struct ActiveWorker {
    ownership: ActiveAttemptOwnership,
    worker: Arc<dyn InvocationWorker>,
}

struct ProducerTracker {
    accepting: AtomicBool,
    active: AtomicUsize,
    changed: tokio::sync::Notify,
}

struct ProducerGuard {
    producers: Arc<ProducerTracker>,
}

struct HostEventConsumer {
    state: Arc<HostState>,
    attempt: ExecutionAttempt,
    buffered: StdMutex<BufferedWorkerEvents>,
    _producer: ProducerGuard,
}

#[derive(Default)]
struct BufferedWorkerEvents {
    events: Vec<InvocationEvent>,
    mismatch: Option<ExecutionAttempt>,
    protocol_error: Option<String>,
}

struct LifecycleBarrier {
    active: AtomicUsize,
    changed: tokio::sync::Notify,
}

struct LifecycleGuard {
    barrier: Arc<LifecycleBarrier>,
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
        Self::build(
            factory,
            runtime_host_id,
            Some(runtime_session_id),
            expected_attempt,
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn logged(
        factory: Arc<dyn InvocationWorkerFactory>,
        runtime_host_id: RuntimeHostId,
        runtime_session_id: Option<RuntimeSessionId>,
        expected_attempt: Option<ActiveAttemptOwnership>,
        log_context: RuntimeLogContext,
        store: Arc<dyn ExecutionLogStore>,
        config: RuntimeLogWriterConfig,
        console: Option<SyncSender<ExecutionLogRecord>>,
    ) -> Result<Self, RuntimeHostError> {
        let writer = RuntimeLogWriter::new(
            store,
            log_context.clone(),
            runtime_host_id.clone(),
            now_unix_nanos(),
            config,
            console,
        )?;
        let host = Self::build(
            factory,
            runtime_host_id,
            runtime_session_id,
            expected_attempt,
            Some(log_context),
            Some(Arc::new(writer)),
        );
        host.state.log_lifecycle(
            LogLevel::Info,
            "runtime.startup",
            json!({"ryvus.lifecycle": "startup"}),
        );
        host.state.log_lifecycle(
            LogLevel::Info,
            "runtime.ready",
            json!({"ryvus.lifecycle": "readiness"}),
        );
        Ok(host)
    }

    fn build(
        factory: Arc<dyn InvocationWorkerFactory>,
        runtime_host_id: RuntimeHostId,
        runtime_session_id: Option<RuntimeSessionId>,
        expected_attempt: Option<ActiveAttemptOwnership>,
        log_context: Option<RuntimeLogContext>,
        log_writer: Option<Arc<RuntimeLogWriter>>,
    ) -> Self {
        let (control_tx, control_rx) = mpsc::unbounded_channel();
        let (control_events, _) = broadcast::channel(64);
        let (startup_cancel, _) = tokio::sync::watch::channel(false);
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
                runtime_session_id: StdMutex::new(runtime_session_id),
                log_context,
                log_writer,
                producers: Arc::new(ProducerTracker {
                    accepting: AtomicBool::new(true),
                    active: AtomicUsize::new(0),
                    changed: tokio::sync::Notify::new(),
                }),
                startups: Arc::new(LifecycleBarrier {
                    active: AtomicUsize::new(0),
                    changed: tokio::sync::Notify::new(),
                }),
                startup_cancel,
                startup_admission: StdMutex::new(true),
                shutdown: Mutex::new(()),
                shutdown_result: StdMutex::new(None),
                control_events,
                expected_attempt,
                accepting: AtomicBool::new(true),
                draining: AtomicBool::new(false),
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
        let Some(mut receiver) = self
            .control_rx
            .lock()
            .ok()
            .and_then(|mut receiver| receiver.take())
        else {
            return;
        };
        while let Some(request) = receiver.recv().await {
            let result = self.state.handle_command(request.command).await;
            match request.response {
                ControlResponse::Blocking(response) => {
                    let _ = response.send(result);
                }
                ControlResponse::Async(response) => {
                    let _ = response.send(result);
                }
            }
        }
    }

    pub async fn wait_stopped(&self) {
        if self.state.stopped.load(Ordering::Acquire) {
            return;
        }
        self.state.stopped_notify.notified().await;
    }

    pub fn identity(&self) -> (RuntimeHostId, Option<RuntimeSessionId>) {
        (self.state.runtime_host_id.clone(), self.state.session_id())
    }

    pub fn ensure_log_context(&self, context: &RuntimeLogContext) -> Result<(), RuntimeHostError> {
        if self.state.log_context.as_ref() == Some(context) {
            Ok(())
        } else {
            Err(RuntimeHostError::IncompatibleLogContext)
        }
    }

    pub fn begin_control_session(&self) -> RuntimeSessionId {
        let session_id = RuntimeSessionId::new();
        if let Ok(mut current) = self.state.runtime_session_id.lock() {
            *current = Some(session_id.clone());
        }
        self.state.log_lifecycle(
            LogLevel::Info,
            "runtime.session_changed",
            json!({"ryvus.lifecycle": "session_change"}),
        );
        session_id
    }

    pub fn end_control_session(&self, session_id: &RuntimeSessionId) {
        let cleared = self
            .state
            .runtime_session_id
            .lock()
            .map(|mut current| {
                if current.as_ref() == Some(session_id) {
                    *current = None;
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false);
        if cleared {
            self.state.log_lifecycle(
                LogLevel::Warn,
                "runtime.session_lost",
                json!({"ryvus.lifecycle": "session_loss"}),
            );
        }
    }

    pub async fn registration(&self, revision: impl Into<String>) -> RuntimeRegistration {
        let runtime_session_id = self
            .state
            .session_id()
            .unwrap_or_else(|| self.begin_control_session());
        RuntimeRegistration {
            protocol_version: RUNTIME_CONTROL_PROTOCOL_VERSION.to_string(),
            message_id: ControlMessageId::new(),
            runtime_host_id: self.state.runtime_host_id.clone(),
            runtime_session_id,
            revision: revision.into(),
            max_concurrency: u32::try_from(self.state.max_workers).unwrap_or(u32::MAX),
            capabilities: ryvus_protocol::RuntimeCapabilities {
                terminate_attempt: true,
                drain: true,
                shutdown: true,
            },
            active_attempts: self.active_attempt().await.into_iter().collect(),
        }
    }

    pub fn subscribe_control_events(&self) -> broadcast::Receiver<RuntimeControlEvent> {
        self.state.control_events.subscribe()
    }

    pub async fn drain(&self) {
        self.state.begin_drain();
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
            .ok()
            .and_then(|mut events| events.remove(&attempt.attempt_id))
            .unwrap_or_default()
    }

    pub async fn shutdown(&self) -> Result<(), RuntimeHostError> {
        self.state.shutdown_now().await
    }
}

impl RuntimeHostControlSender {
    pub fn send(&self, command: RuntimeControlCommand) -> Result<RuntimeControlEvent, String> {
        let (response_tx, response_rx) = std::sync::mpsc::channel();
        self.tx
            .send(ControlRequest {
                command,
                response: ControlResponse::Blocking(response_tx),
            })
            .map_err(|_| "runtime control loop is unavailable".to_string())?;
        response_rx
            .recv()
            .map_err(|_| "runtime control response was dropped".to_string())
    }

    pub async fn send_async(
        &self,
        command: RuntimeControlCommand,
    ) -> Result<RuntimeControlEvent, String> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(ControlRequest {
                command,
                response: ControlResponse::Async(response_tx),
            })
            .map_err(|_| "runtime control loop is unavailable".to_string())?;
        response_rx
            .await
            .map_err(|_| "runtime control response was dropped".to_string())
    }
}

impl ProducerTracker {
    fn guard(self: &Arc<Self>) -> Option<ProducerGuard> {
        self.acquire().then(|| ProducerGuard {
            producers: Arc::clone(self),
        })
    }

    fn acquire(&self) -> bool {
        if !self.accepting.load(Ordering::Acquire) {
            return false;
        }
        self.active.fetch_add(1, Ordering::AcqRel);
        if self.accepting.load(Ordering::Acquire) {
            true
        } else {
            self.release();
            false
        }
    }

    fn release(&self) {
        if self.active.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.changed.notify_waiters();
        }
    }

    async fn close_and_wait(&self, deadline: tokio::time::Instant) -> bool {
        self.accepting.store(false, Ordering::Release);
        loop {
            let changed = self.changed.notified();
            if self.active.load(Ordering::Acquire) == 0 {
                return true;
            }
            if tokio::time::timeout_at(deadline, changed).await.is_err() {
                return self.active.load(Ordering::Acquire) == 0;
            }
        }
    }
}

impl Drop for ProducerGuard {
    fn drop(&mut self) {
        self.producers.release();
    }
}

impl LifecycleBarrier {
    fn acquire(self: &Arc<Self>) -> LifecycleGuard {
        self.active.fetch_add(1, Ordering::AcqRel);
        LifecycleGuard {
            barrier: Arc::clone(self),
        }
    }

    async fn wait_idle(&self) {
        loop {
            let changed = self.changed.notified();
            if self.active.load(Ordering::Acquire) == 0 {
                return;
            }
            changed.await;
        }
    }
}

impl Drop for LifecycleGuard {
    fn drop(&mut self) {
        if self.barrier.active.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.barrier.changed.notify_waiters();
        }
    }
}

impl WorkerEventConsumer for HostEventConsumer {
    fn record(&self, event: InvocationEvent) {
        if !self.state.producers.accepting.load(Ordering::Acquire) {
            return;
        }
        match event {
            InvocationEvent::Log(log) => {
                if log.attempt_id != self.attempt.attempt_id
                    || log.execution_id != self.attempt.execution_id
                    || log.attempt_number != self.attempt.attempt_number
                {
                    self.state.log_lifecycle(
                        LogLevel::Warn,
                        "runtime.worker_event_rejected",
                        json!({"ryvus.diagnostic": "log_attempt_mismatch"}),
                    );
                    return;
                }
                if let Some(writer) = &self.state.log_writer {
                    if let Err(error) =
                        writer.enqueue(log, now_unix_nanos(), self.state.session_id())
                    {
                        tracing::warn!(%error, "runtime application log was rejected");
                    }
                } else {
                    self.buffer(InvocationEvent::Log(log));
                }
            }
            event => {
                if event.attempt() != self.attempt {
                    if let Ok(mut buffered) = self.buffered.lock() {
                        buffered.mismatch.get_or_insert_with(|| event.attempt());
                    }
                } else {
                    self.buffer(event);
                }
            }
        }
    }
}

impl HostEventConsumer {
    fn buffer(&self, event: InvocationEvent) {
        let Ok(mut buffered) = self.buffered.lock() else {
            return;
        };
        if buffered.events.len() < MAX_METRICS_PER_INVOCATION {
            buffered.events.push(event);
        } else {
            buffered.protocol_error.get_or_insert_with(|| {
                "worker emitted more non-log events than the invocation limit".to_string()
            });
        }
    }

    fn finish(&self) -> Result<Vec<InvocationEvent>, RuntimeHostError> {
        let mut buffered = self.buffered.lock().map_err(|_| {
            RuntimeHostError::Worker(WorkerError::Protocol(
                "worker event buffer is unavailable".to_string(),
            ))
        })?;
        if let Some(actual) = buffered.mismatch.take() {
            return Err(RuntimeHostError::AttemptMismatch {
                expected: self.attempt.clone(),
                actual,
            });
        }
        if let Some(error) = buffered.protocol_error.take() {
            return Err(RuntimeHostError::Worker(WorkerError::Protocol(error)));
        }
        Ok(std::mem::take(&mut buffered.events))
    }
}

impl HostState {
    fn admit_startup(&self) -> Option<LifecycleGuard> {
        self.startup_admission
            .lock()
            .ok()
            .filter(|admission| **admission)
            .map(|_| self.startups.acquire())
    }

    fn begin_drain(&self) {
        if let Ok(mut admission) = self.startup_admission.lock() {
            *admission = false;
        }
        self.accepting.store(false, Ordering::Release);
        if !self.draining.swap(true, Ordering::AcqRel) {
            self.log_lifecycle(
                LogLevel::Info,
                "runtime.drain",
                json!({"ryvus.lifecycle": "drain"}),
            );
        }
    }

    fn event_consumer(
        self: &Arc<Self>,
        attempt: ExecutionAttempt,
    ) -> Option<Arc<HostEventConsumer>> {
        self.producers.guard().map(|producer| {
            Arc::new(HostEventConsumer {
                state: Arc::clone(self),
                attempt,
                buffered: StdMutex::new(BufferedWorkerEvents::default()),
                _producer: producer,
            })
        })
    }

    fn commit_events(&self, attempt_id: &AttemptId, events: Vec<InvocationEvent>) {
        if events.is_empty() {
            return;
        }
        let Ok(mut completed) = self.completed_events.lock() else {
            return;
        };
        completed.insert(attempt_id.clone(), events);
    }

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
        } else if self.session_id().as_ref() != Some(command_runtime_session_id(&command)) {
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
                    self.begin_drain();
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
            runtime_session_id: command_runtime_session_id(&command).clone(),
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
        if let Some(expected) = &self.expected_attempt {
            if expected.attempt_id != *attempt_id {
                return (ControlCommandOutcome::AttemptNotFound, None);
            }
            if expected.execution_id != *execution_id || expected.attempt_number != attempt_number {
                return (ControlCommandOutcome::OwnershipMismatch, None);
            }
        }
        let active = self.active.lock().await.clone();
        if let Some(active) = active {
            if active.ownership.attempt_id != *attempt_id {
                return (ControlCommandOutcome::AttemptNotFound, None);
            }
            if active.ownership.execution_id != *execution_id
                || active.ownership.attempt_number != attempt_number
                || self
                    .expected_attempt
                    .as_ref()
                    .is_some_and(|expected| expected.worker_id != active.ownership.worker_id)
            {
                return (ControlCommandOutcome::OwnershipMismatch, None);
            }
            if let Err(error) = active.worker.terminate(reason).await {
                return (ControlCommandOutcome::Failed, Some(error.to_string()));
            }
            self.clear_active(&active.ownership).await;
            let outcome = match reason {
                TerminationReason::Timeout => AttemptOutcome::TimedOut,
                _ => AttemptOutcome::Cancelled,
            };
            if self.transition_terminal(&active.ownership, outcome).await {
                (ControlCommandOutcome::Confirmed, None)
            } else {
                (ControlCommandOutcome::AlreadyTerminal, None)
            }
        } else {
            let Some(expected) = &self.expected_attempt else {
                return (ControlCommandOutcome::AttemptNotFound, None);
            };
            let outcome = match reason {
                TerminationReason::Timeout => AttemptOutcome::TimedOut,
                _ => AttemptOutcome::Cancelled,
            };
            if self.transition_terminal(expected, outcome).await {
                (ControlCommandOutcome::Confirmed, None)
            } else {
                (ControlCommandOutcome::AlreadyTerminal, None)
            }
        }
    }

    async fn shutdown_now(&self) -> Result<(), RuntimeHostError> {
        let _shutdown = self.shutdown.lock().await;
        if self.stopped.load(Ordering::Acquire) {
            return Ok(());
        }
        if let Some(result) = self
            .shutdown_result
            .lock()
            .ok()
            .and_then(|result| result.clone())
        {
            return result.map_err(RuntimeHostError::ShutdownFailed);
        }
        self.begin_drain();
        let _ = self.startup_cancel.send(true);
        self.startups.wait_idle().await;
        let active = self.active.lock().await.clone();
        if let Some(active) = active {
            active.worker.terminate(TerminationReason::Shutdown).await?;
            self.clear_active(&active.ownership).await;
            self.transition_terminal(&active.ownership, AttemptOutcome::Cancelled)
                .await;
        }
        let producer_deadline = tokio::time::Instant::now() + PRODUCER_SHUTDOWN_GRACE;
        if !self.producers.close_and_wait(producer_deadline).await {
            return Err(RuntimeHostError::LoggingProducersActive);
        }
        self.log_lifecycle_final(
            LogLevel::Info,
            "runtime.shutdown",
            json!({"ryvus.lifecycle": "shutdown"}),
        );
        let terminal_result = if let Some(writer) = &self.log_writer {
            let writer = Arc::clone(writer);
            let deadline = std::time::Instant::now() + writer.configured_shutdown_duration();
            match tokio::task::spawn_blocking(move || writer.shutdown(deadline)).await {
                Ok(result) => result.map_err(RuntimeHostError::from),
                Err(error) => Err(RuntimeHostError::Supervision(error)),
            }
        } else {
            Ok(())
        };
        if let Err(error) = terminal_result {
            if let Ok(mut result) = self.shutdown_result.lock() {
                *result = Some(Err(error.to_string()));
            }
            return Err(error);
        }
        self.stopped.store(true, Ordering::Release);
        if let Ok(mut result) = self.shutdown_result.lock() {
            *result = Some(Ok(()));
        }
        self.stopped_notify.notify_waiters();
        Ok(())
    }

    async fn transition_terminal(
        &self,
        ownership: &ActiveAttemptOwnership,
        outcome: AttemptOutcome,
    ) -> bool {
        let mut terminal = self.terminal_attempts.lock().await;
        if terminal.contains_key(&ownership.attempt_id) {
            false
        } else {
            terminal.insert(ownership.attempt_id.clone(), outcome);
            drop(terminal);
            if let Some(runtime_session_id) = self.session_id() {
                let _ = self
                    .control_events
                    .send(RuntimeControlEvent::AttemptFinished {
                        protocol_version: RUNTIME_CONTROL_PROTOCOL_VERSION.to_string(),
                        message_id: ControlMessageId::new(),
                        runtime_host_id: self.runtime_host_id.clone(),
                        runtime_session_id,
                        execution_id: ownership.execution_id.clone(),
                        attempt_id: ownership.attempt_id.clone(),
                        attempt_number: ownership.attempt_number,
                        worker_id: ownership.worker_id.clone(),
                        outcome,
                    });
            }
            true
        }
    }

    fn session_id(&self) -> Option<RuntimeSessionId> {
        self.runtime_session_id
            .lock()
            .map(|session| session.clone())
            .unwrap_or(None)
    }

    fn with_producer<R>(&self, produce: impl FnOnce(&ProducerGuard) -> R) -> Option<R> {
        let producer = self.producers.guard()?;
        Some(produce(&producer))
    }

    fn log_lifecycle(&self, level: LogLevel, message: &str, fields: serde_json::Value) {
        self.with_producer(|_| self.log_lifecycle_final(level, message, fields));
    }

    fn log_lifecycle_final(&self, level: LogLevel, message: &str, fields: serde_json::Value) {
        let Some(writer) = &self.log_writer else {
            return;
        };
        if let Err(error) =
            writer.enqueue_lifecycle(level, message, fields, now_unix_nanos(), self.session_id())
        {
            tracing::warn!(%error, message, "runtime lifecycle log was rejected");
        }
    }

    fn log_invocation(&self, request: &InvocationRequest, level: LogLevel, message: &str) {
        self.with_producer(|producer| {
            self.log_invocation_guarded(producer, request, level, message)
        });
    }

    fn log_invocation_guarded(
        &self,
        _producer: &ProducerGuard,
        request: &InvocationRequest,
        level: LogLevel,
        message: &str,
    ) {
        let Some(writer) = &self.log_writer else {
            return;
        };
        let event = LogEvent {
            execution_id: request.execution_id.clone(),
            attempt_id: request.attempt_id.clone(),
            attempt_number: request.attempt_number,
            timestamp_unix_nanos: Some(now_unix_nanos()),
            trace_id: None,
            span_id: None,
            level,
            message: message.to_string(),
            fields: json!({"ryvus.lifecycle": message}),
        };
        if let Err(error) = writer.enqueue(event, now_unix_nanos(), self.session_id()) {
            tracing::warn!(%error, message, "runtime invocation log was rejected");
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

    async fn cleanup_and_finish(
        &self,
        active: &ActiveWorker,
        request: &InvocationRequest,
        reason: TerminationReason,
        level: LogLevel,
    ) -> Result<(), WorkerError> {
        let result = self.cleanup(active, reason).await;
        self.log_invocation(request, level, "invocation.finish");
        result
    }

    async fn cleanup_and_finish_guarded(
        &self,
        active: &ActiveWorker,
        request: &InvocationRequest,
        reason: TerminationReason,
        level: LogLevel,
        producer: &ProducerGuard,
    ) -> Result<(), WorkerError> {
        let result = self.cleanup(active, reason).await;
        self.log_invocation_guarded(producer, request, level, "invocation.finish");
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

    let mut startup_cancel = state.startup_cancel.subscribe();
    let Some(startup) = state.admit_startup() else {
        return Err(RuntimeHostError::Unavailable);
    };
    state.log_invocation(&request, LogLevel::Info, "invocation.start");

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
    let started = tokio::select! {
        biased;
        changed = startup_cancel.changed() => {
            let _ = changed;
            state.log_invocation(&request, LogLevel::Warn, "invocation.finish");
            return Err(RuntimeHostError::Unavailable);
        }
        result = state.factory.start(&request, worker_id) => match result {
            Ok(started) => started,
            Err(error) => {
            state.log_invocation(&request, LogLevel::Error, "invocation.finish");
            return Err(error.into());
            }
        }
    };
    state.log_invocation(&request, LogLevel::Info, "worker.initialized");
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
        state
            .cleanup_and_finish(
                &active,
                &request,
                TerminationReason::Shutdown,
                LogLevel::Warn,
            )
            .await?;
        return Err(RuntimeHostError::Unavailable);
    }
    if let Some(runtime_session_id) = state.session_id() {
        let _ = state
            .control_events
            .send(RuntimeControlEvent::AttemptStarted {
                protocol_version: RUNTIME_CONTROL_PROTOCOL_VERSION.to_string(),
                message_id: ControlMessageId::new(),
                runtime_host_id: state.runtime_host_id.clone(),
                runtime_session_id,
                execution_id: ownership.execution_id.clone(),
                attempt_id: ownership.attempt_id.clone(),
                attempt_number: ownership.attempt_number,
                worker_id: ownership.worker_id.clone(),
            });
    }

    let Some(events) = state.event_consumer(request.attempt()) else {
        state
            .cleanup_and_finish(
                &active,
                &request,
                TerminationReason::Shutdown,
                LogLevel::Warn,
            )
            .await?;
        return Err(RuntimeHostError::Unavailable);
    };
    let Some(recovery_producer) = state.producers.guard() else {
        state
            .cleanup_and_finish(
                &active,
                &request,
                TerminationReason::Shutdown,
                LogLevel::Warn,
            )
            .await?;
        return Err(RuntimeHostError::Unavailable);
    };
    drop(startup);

    let task_state = Arc::clone(&state);
    let recovery = active.clone();
    let recovery_request = request.clone();
    let task = tokio::spawn(async move {
        let _capacity = capacity;
        supervise_attempt(task_state, active, request, deadline.monotonic, events).await
    });
    let result = match task.await {
        Ok(result) => result,
        Err(error) => {
            state
                .cleanup_and_finish_guarded(
                    &recovery,
                    &recovery_request,
                    TerminationReason::Shutdown,
                    LogLevel::Error,
                    &recovery_producer,
                )
                .await?;
            Err(RuntimeHostError::Supervision(error))
        }
    };
    drop(recovery_producer);
    result
}

async fn supervise_attempt(
    state: Arc<HostState>,
    active: ActiveWorker,
    request: InvocationRequest,
    deadline: tokio::time::Instant,
    events: Arc<HostEventConsumer>,
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
                .transition_terminal(&active.ownership, AttemptOutcome::TimedOut)
                .await;
        }
        state
            .cleanup_and_finish(
                &active,
                &request,
                if timed_out {
                    TerminationReason::Timeout
                } else {
                    TerminationReason::Shutdown
                },
                LogLevel::Error,
            )
            .await?;
        return Err(if timed_out {
            RuntimeHostError::TimedOut
        } else {
            RuntimeHostError::Worker(error)
        });
    }
    state.log_invocation(&request, LogLevel::Info, "worker.ready");

    let invocation = tokio::select! {
        biased;
        _ = tokio::time::sleep_until(deadline) => Err(RuntimeHostError::TimedOut),
        result = active.worker.invoke(request.clone(), deadline, events.clone()) => {
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
            .transition_terminal(&active.ownership, AttemptOutcome::TimedOut)
            .await;
    }
    let cleanup_reason = if matches!(invocation, Err(RuntimeHostError::TimedOut)) {
        TerminationReason::Timeout
    } else {
        TerminationReason::Shutdown
    };
    let cleanup = state.cleanup(&active, cleanup_reason).await;
    if let Err(error) = cleanup {
        state.log_invocation(&request, LogLevel::Error, "invocation.finish");
        return Err(RuntimeHostError::Worker(error));
    }
    let result = match invocation {
        Ok(result) => result,
        Err(error) => {
            state.log_invocation(&request, LogLevel::Error, "invocation.finish");
            return Err(error);
        }
    };

    if result.protocol_version != PROTOCOL_VERSION {
        state.log_invocation(&request, LogLevel::Error, "invocation.finish");
        return Err(RuntimeHostError::WorkerProtocolMismatch {
            actual: result.protocol_version,
        });
    }
    if result.attempt() != request.attempt() {
        state.log_invocation(&request, LogLevel::Error, "invocation.finish");
        return Err(RuntimeHostError::AttemptMismatch {
            expected: request.attempt(),
            actual: result.attempt(),
        });
    }
    let retained_events = match events.finish() {
        Ok(events) => events,
        Err(error) => {
            state.log_invocation(&request, LogLevel::Error, "invocation.finish");
            return Err(error);
        }
    };
    let outcome = if result.status == ryvus_protocol::InvocationStatus::Success {
        AttemptOutcome::Succeeded
    } else {
        AttemptOutcome::Failed
    };
    if !state.transition_terminal(&active.ownership, outcome).await {
        state.log_invocation(&request, LogLevel::Warn, "invocation.finish");
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
    state.commit_events(&request.attempt_id, retained_events);
    state.log_invocation(&request, LogLevel::Info, "invocation.finish");
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

fn now_unix_nanos() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_nanos()).ok())
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use ryvus_protocol::{
        ControlCommandOutcome, ControlMessageId, InvocationEvent, InvocationRequest,
        InvocationResult, LogEvent, LogLevel, MetricEvent, RuntimeControlCommand, RuntimeHostId,
        RuntimeSessionId, WorkerId, RUNTIME_CONTROL_PROTOCOL_VERSION,
    };
    use serde_json::json;
    use tower::ServiceExt;

    use super::*;
    use crate::worker::{MockInvocationWorker, MockInvocationWorkerFactory};

    struct BlockingFactory {
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        started: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl InvocationWorkerFactory for BlockingFactory {
        async fn start(
            &self,
            _request: &InvocationRequest,
            worker_id: WorkerId,
        ) -> Result<StartedWorker, WorkerError> {
            self.entered.notify_waiters();
            self.release.notified().await;
            self.started.store(true, Ordering::Release);
            Ok(StartedWorker {
                worker_id,
                worker: Arc::new(NeverUsedWorker),
            })
        }
    }

    struct NeverUsedWorker;

    #[async_trait::async_trait]
    impl InvocationWorker for NeverUsedWorker {
        async fn wait_ready(&self, _deadline: tokio::time::Instant) -> Result<(), WorkerError> {
            Ok(())
        }

        async fn invoke(
            &self,
            request: InvocationRequest,
            _deadline: tokio::time::Instant,
            _events: Arc<dyn WorkerEventConsumer>,
        ) -> Result<InvocationResult, WorkerError> {
            Ok(InvocationResult::success(&request, json!({})))
        }

        async fn terminate(&self, _reason: TerminationReason) -> Result<(), WorkerError> {
            Ok(())
        }
    }

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
    async fn shutdown_cancels_admitted_worker_start_before_terminalizing() {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let started = Arc::new(AtomicBool::new(false));
        let host = RuntimeHost::new(Arc::new(BlockingFactory {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
            started: Arc::clone(&started),
        }));
        let request = request_with_budget(Duration::from_secs(5));
        let entered_start = entered.notified();
        let invoke_host = host.clone();
        let invoke = tokio::spawn(async move {
            invoke_host
                .router()
                .oneshot(
                    Request::post("/invoke")
                        .header("content-type", "application/json")
                        .body(Body::from(serde_json::to_vec(&request).unwrap()))
                        .unwrap(),
                )
                .await
                .unwrap()
        });
        entered_start.await;

        host.shutdown().await.unwrap();

        assert!(!started.load(Ordering::Acquire));
        release.notify_waiters();
        assert_eq!(
            invoke.await.unwrap().status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        tokio::task::yield_now().await;
        assert!(!started.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn producer_close_rejects_new_guards_before_waiting_for_existing_guard() {
        let producers = Arc::new(ProducerTracker {
            accepting: AtomicBool::new(true),
            active: AtomicUsize::new(0),
            changed: tokio::sync::Notify::new(),
        });
        assert!(producers.acquire());
        let close = tokio::spawn({
            let producers = Arc::clone(&producers);
            async move {
                producers
                    .close_and_wait(tokio::time::Instant::now() + Duration::from_secs(1))
                    .await
            }
        });
        while producers.accepting.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }

        assert!(!producers.acquire());
        assert!(!close.is_finished());
        producers.release();
        assert!(close.await.unwrap());
    }

    #[tokio::test]
    async fn shutdown_waits_for_a_lifecycle_enqueue_that_already_holds_a_producer() {
        let host = RuntimeHost::new(Arc::new(MockInvocationWorkerFactory::new()));
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let producer = tokio::task::spawn_blocking({
            let state = Arc::clone(&host.state);
            move || {
                state.with_producer(|_| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    state.log_lifecycle_final(
                        LogLevel::Info,
                        "runtime.paused",
                        json!({"ryvus.lifecycle": "paused"}),
                    );
                });
            }
        });
        tokio::task::spawn_blocking(move || entered_rx.recv().unwrap())
            .await
            .unwrap();
        let shutdown = tokio::spawn({
            let host = host.clone();
            async move { host.shutdown().await }
        });
        while host.state.producers.accepting.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }

        assert!(!shutdown.is_finished());
        release_tx.send(()).unwrap();
        producer.await.unwrap();
        shutdown.await.unwrap().unwrap();
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
        worker
            .expect_invoke()
            .once()
            .return_once(move |request, _, events| {
                events.record(InvocationEvent::Metric(MetricEvent {
                    execution_id: request.execution_id,
                    attempt_id: request.attempt_id,
                    attempt_number: request.attempt_number,
                    name: "discarded".to_string(),
                    value: 1.0,
                    unit: "count".to_string(),
                }));
                Ok(result)
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
        assert!(host.take_events(&request.attempt()).is_empty());
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
            .return_once(move |request, _, events| {
                events.record(InvocationEvent::Log(LogEvent {
                    execution_id: request.execution_id,
                    attempt_id: ryvus_protocol::AttemptId::new(),
                    attempt_number: request.attempt_number,
                    timestamp_unix_nanos: None,
                    trace_id: None,
                    span_id: None,
                    level: LogLevel::Info,
                    message: "wrong attempt".to_string(),
                    fields: json!({}),
                }));
                Ok(result)
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

        assert_eq!(response.status(), StatusCode::OK);
        assert!(host.take_events(&request.attempt()).is_empty());
    }

    #[tokio::test]
    async fn rejects_mismatched_metric_identity_and_commits_nothing() {
        let request = request_with_budget(Duration::from_secs(5));
        let result = InvocationResult::success(&request, json!({ "ok": true }));
        let mut worker = MockInvocationWorker::new();
        worker.expect_wait_ready().once().returning(|_| Ok(()));
        worker
            .expect_invoke()
            .once()
            .return_once(move |request, _, events| {
                events.record(InvocationEvent::Metric(MetricEvent {
                    execution_id: request.execution_id,
                    attempt_id: ryvus_protocol::AttemptId::new(),
                    attempt_number: request.attempt_number,
                    name: "wrong".to_string(),
                    value: 1.0,
                    unit: "count".to_string(),
                }));
                Ok(result)
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
            .return_once(|_, _, _| panic!("worker task panic"));
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
