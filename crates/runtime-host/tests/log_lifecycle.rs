use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use axum::{body::Body, http::Request};
use ryvus_logging::{
    ExecutionLogStore, InMemoryExecutionLogStore, LogBatch, LogProjectedRecordPage,
    LogProjectedRecordQuery, LogRecordPage, LogRecordQuery, LogStoreError, LogStreamCompleteness,
    LogStreamId, LogStreamPage, LogStreamQuery, RuntimeLogContext,
};
use ryvus_protocol::{
    ControlCommandOutcome, ControlMessageId, ExecutionScopeId, InvocationEvent, InvocationRequest,
    InvocationResult, LogEvent, LogLevel, MetricEvent, RuntimeControlCommand, RuntimeControlEvent,
    RuntimeHostId, RuntimeKind, WorkerId, RUNTIME_CONTROL_PROTOCOL_VERSION,
};
use ryvus_runtime_host::{
    InvocationWorker, InvocationWorkerFactory, RuntimeHost, RuntimeHostError,
    RuntimeLogWriterConfig, StartedWorker, WorkerError, WorkerEventConsumer,
};
use serde_json::json;
use tokio::time::Instant;
use tower::ServiceExt;

struct TestFactory {
    worker: Arc<dyn InvocationWorker>,
}

struct BlockingStartFactory {
    entered: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl InvocationWorkerFactory for BlockingStartFactory {
    async fn start(
        &self,
        _request: &InvocationRequest,
        _worker_id: WorkerId,
    ) -> Result<StartedWorker, WorkerError> {
        self.entered.notify_waiters();
        std::future::pending().await
    }
}

type RetainedConsumer = Arc<Mutex<Option<Arc<dyn WorkerEventConsumer>>>>;

#[async_trait]
impl InvocationWorkerFactory for TestFactory {
    async fn start(
        &self,
        _request: &InvocationRequest,
        worker_id: WorkerId,
    ) -> Result<StartedWorker, WorkerError> {
        Ok(StartedWorker {
            worker_id,
            worker: Arc::clone(&self.worker),
        })
    }
}

struct EventWorker {
    retained: Option<RetainedConsumer>,
    wait_for_persisted_log: Option<Arc<AtomicBool>>,
}

struct PanicWorker;

#[async_trait]
impl InvocationWorker for PanicWorker {
    async fn wait_ready(&self, _deadline: Instant) -> Result<(), WorkerError> {
        Ok(())
    }

    async fn invoke(
        &self,
        _request: InvocationRequest,
        _deadline: Instant,
        _events: Arc<dyn WorkerEventConsumer>,
    ) -> Result<InvocationResult, WorkerError> {
        panic!("worker task panic")
    }

    async fn terminate(
        &self,
        _reason: ryvus_protocol::TerminationReason,
    ) -> Result<(), WorkerError> {
        Ok(())
    }
}

struct FailingTerminateWorker;

#[async_trait]
impl InvocationWorker for FailingTerminateWorker {
    async fn wait_ready(&self, _deadline: Instant) -> Result<(), WorkerError> {
        Err(WorkerError::Protocol("readiness failed".to_string()))
    }

    async fn invoke(
        &self,
        _request: InvocationRequest,
        _deadline: Instant,
        _events: Arc<dyn WorkerEventConsumer>,
    ) -> Result<InvocationResult, WorkerError> {
        unreachable!("readiness failure must prevent invocation")
    }

    async fn terminate(
        &self,
        _reason: ryvus_protocol::TerminationReason,
    ) -> Result<(), WorkerError> {
        Err(WorkerError::Process(std::io::Error::other(
            "termination failed",
        )))
    }
}

struct PausedPanicRecoveryWorker {
    terminate_calls: AtomicUsize,
    recovery_entered: Arc<tokio::sync::Notify>,
    release_recovery: Arc<tokio::sync::Notify>,
    shutdown_terminate_entered: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl InvocationWorker for PausedPanicRecoveryWorker {
    async fn wait_ready(&self, _deadline: Instant) -> Result<(), WorkerError> {
        Ok(())
    }

    async fn invoke(
        &self,
        _request: InvocationRequest,
        _deadline: Instant,
        _events: Arc<dyn WorkerEventConsumer>,
    ) -> Result<InvocationResult, WorkerError> {
        panic!("worker task panic")
    }

    async fn terminate(
        &self,
        _reason: ryvus_protocol::TerminationReason,
    ) -> Result<(), WorkerError> {
        if self.terminate_calls.fetch_add(1, Ordering::AcqRel) == 0 {
            self.recovery_entered.notify_waiters();
            self.release_recovery.notified().await;
        } else {
            self.shutdown_terminate_entered.notify_waiters();
        }
        Ok(())
    }
}

#[async_trait]
impl InvocationWorker for EventWorker {
    async fn wait_ready(&self, _deadline: Instant) -> Result<(), WorkerError> {
        Ok(())
    }

    async fn invoke(
        &self,
        request: InvocationRequest,
        _deadline: Instant,
        events: Arc<dyn WorkerEventConsumer>,
    ) -> Result<InvocationResult, WorkerError> {
        events.record(InvocationEvent::Log(LogEvent {
            execution_id: request.execution_id.clone(),
            attempt_id: request.attempt_id.clone(),
            attempt_number: request.attempt_number,
            timestamp_unix_nanos: Some(now_unix_nanos()),
            trace_id: Some("11".repeat(16)),
            span_id: Some("22".repeat(8)),
            level: LogLevel::Info,
            message: "application.log".to_string(),
            fields: json!({"source": "test"}),
        }));
        events.record(InvocationEvent::Metric(MetricEvent {
            execution_id: request.execution_id.clone(),
            attempt_id: request.attempt_id.clone(),
            attempt_number: request.attempt_number,
            name: "items".to_string(),
            value: 1.0,
            unit: "count".to_string(),
        }));
        if let Some(retained) = &self.retained {
            if let Ok(mut slot) = retained.lock() {
                *slot = Some(events);
            }
        }
        if let Some(persisted) = &self.wait_for_persisted_log {
            let deadline = Instant::now() + Duration::from_secs(1);
            while !persisted.load(Ordering::Acquire) {
                if Instant::now() >= deadline {
                    return Err(WorkerError::Protocol(
                        "streamed log was not persisted before result".to_string(),
                    ));
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }
        Ok(InvocationResult::success(&request, json!({"ok": true})))
    }

    async fn terminate(
        &self,
        _reason: ryvus_protocol::TerminationReason,
    ) -> Result<(), WorkerError> {
        Ok(())
    }
}

struct ObservingStore {
    inner: Arc<InMemoryExecutionLogStore>,
    application_log_persisted: Arc<AtomicBool>,
}

impl ExecutionLogStore for ObservingStore {
    fn append_batch(&self, batch: LogBatch) -> Result<(), LogStoreError> {
        let contains_application_log = batch
            .records
            .iter()
            .any(|record| record.message == "application.log");
        self.inner.append_batch(batch)?;
        if contains_application_log {
            self.application_log_persisted
                .store(true, Ordering::Release);
        }
        Ok(())
    }

    fn list_streams(&self, query: LogStreamQuery) -> Result<LogStreamPage, LogStoreError> {
        self.inner.list_streams(query)
    }

    fn list_records(&self, query: LogRecordQuery) -> Result<LogRecordPage, LogStoreError> {
        self.inner.list_records(query)
    }

    fn list_projected_records(
        &self,
        query: LogProjectedRecordQuery,
    ) -> Result<LogProjectedRecordPage, LogStoreError> {
        self.inner.list_projected_records(query)
    }
}

#[tokio::test]
async fn streams_logs_keeps_metrics_and_preserves_one_sessionless_stream() {
    let store = Arc::new(InMemoryExecutionLogStore::default());
    let application_log_persisted = Arc::new(AtomicBool::new(false));
    let writer_store: Arc<dyn ExecutionLogStore> = Arc::new(ObservingStore {
        inner: Arc::clone(&store),
        application_log_persisted: Arc::clone(&application_log_persisted),
    });
    let context = log_context("revision-1");
    let host_id = RuntimeHostId::from("host-1");
    let host = logged_host(
        Arc::new(EventWorker {
            retained: None,
            wait_for_persisted_log: Some(application_log_persisted),
        }),
        writer_store,
        host_id.clone(),
        context.clone(),
    );
    assert_eq!(host.identity(), (host_id.clone(), None));
    assert!(host.ensure_log_context(&context).is_ok());
    assert!(matches!(
        host.ensure_log_context(&log_context("revision-2")),
        Err(RuntimeHostError::IncompatibleLogContext)
    ));

    let session = host.begin_control_session();
    let request = invocation_request();
    let response = host
        .router()
        .oneshot(
            Request::post("/invoke")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&request).expect("request")))
                .expect("http request"),
        )
        .await
        .expect("invoke response");
    assert!(response.status().is_success());
    assert!(matches!(
        host.take_events(&request.attempt()).as_slice(),
        [InvocationEvent::Metric(metric)] if metric.name == "items"
    ));
    host.end_control_session(&session);
    let next_session = host.begin_control_session();
    let next_request = invocation_request();
    let next_response = host
        .router()
        .oneshot(
            Request::post("/invoke")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&next_request).expect("request"),
                ))
                .expect("http request"),
        )
        .await
        .expect("invoke response");
    assert!(next_response.status().is_success());
    host.end_control_session(&next_session);
    host.shutdown().await.expect("shutdown");

    let records = store
        .list_records(LogRecordQuery {
            stream_id: LogStreamId::new(context.execution_scope, host_id),
            execution_id: None,
            attempt_id: None,
            severity: None,
            message_contains: None,
            cursor: None,
            limit: 100,
        })
        .expect("records")
        .records;
    assert!(records
        .windows(2)
        .all(|pair| { pair[0].stream_sequence.checked_add(1) == Some(pair[1].stream_sequence) }));
    assert_eq!(
        records.first().map(|record| record.message.as_str()),
        Some("runtime.startup")
    );
    for message in [
        "runtime.startup",
        "runtime.ready",
        "worker.initialized",
        "worker.ready",
    ] {
        let record = records
            .iter()
            .find(|record| record.message == message)
            .unwrap_or_else(|| panic!("missing {message}"));
        assert_eq!(record.severity, LogLevel::Trace, "{message}");
    }
    assert_eq!(
        records
            .first()
            .and_then(|record| record.runtime_session_id.as_ref()),
        None
    );
    assert!(records.iter().any(|record| {
        record.message == "application.log"
            && record.runtime_session_id.as_ref() == Some(&session)
            && record.correlation.as_ref().is_some_and(|correlation| {
                correlation.execution_id == request.execution_id
                    && correlation.attempt_id == request.attempt_id
            })
    }));
    assert!(records.iter().any(|record| {
        record.message == "runtime.session_lost" && record.runtime_session_id.is_none()
    }));
    assert!(records.iter().any(|record| {
        record.message == "application.log"
            && record.runtime_session_id.as_ref() == Some(&next_session)
            && record.correlation.as_ref().is_some_and(|correlation| {
                correlation.execution_id == next_request.execution_id
                    && correlation.attempt_id == next_request.attempt_id
            })
    }));
    let streams = store
        .list_streams(ryvus_logging::LogStreamQuery {
            execution_scope: ExecutionScopeId::new("scope").expect("scope"),
            action_key_id: None,
            action_revision: None,
            runtime_host_id: None,
            execution_id: None,
            attempt_id: None,
            severity: None,
            message_contains: None,
            cursor: None,
            limit: 10,
        })
        .expect("streams");
    assert_eq!(streams.streams.len(), 1);
    assert_eq!(
        streams.streams[0].completeness,
        LogStreamCompleteness::Complete
    );
}

#[tokio::test]
async fn retained_event_consumer_prevents_terminal_construction() {
    let retained = Arc::new(Mutex::new(None));
    let store = Arc::new(InMemoryExecutionLogStore::default());
    let context = log_context("revision-1");
    let host_id = RuntimeHostId::from("host-retained");
    let host = logged_host(
        Arc::new(EventWorker {
            retained: Some(Arc::clone(&retained)),
            wait_for_persisted_log: None,
        }),
        Arc::clone(&store) as Arc<dyn ExecutionLogStore>,
        host_id.clone(),
        context.clone(),
    );
    let request = invocation_request();
    let response = host
        .router()
        .oneshot(
            Request::post("/invoke")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&request).expect("request")))
                .expect("http request"),
        )
        .await
        .expect("invoke response");
    assert!(response.status().is_success());

    assert!(matches!(
        host.shutdown().await,
        Err(RuntimeHostError::LoggingProducersActive)
    ));
    let stream = store
        .list_streams(ryvus_logging::LogStreamQuery {
            execution_scope: ExecutionScopeId::new("scope").expect("scope"),
            action_key_id: None,
            action_revision: None,
            runtime_host_id: Some(host_id),
            execution_id: None,
            attempt_id: None,
            severity: None,
            message_contains: None,
            cursor: None,
            limit: 10,
        })
        .expect("streams")
        .streams
        .pop()
        .expect("stream");
    assert_eq!(stream.completeness, LogStreamCompleteness::Active);

    retained.lock().expect("retained consumer").take();
    host.shutdown()
        .await
        .expect("shutdown after producer closure");
}

#[tokio::test]
async fn control_and_direct_drain_emit_one_lifecycle_record_before_shutdown() {
    let store = Arc::new(InMemoryExecutionLogStore::default());
    let context = log_context("revision-1");
    let host_id = RuntimeHostId::from("host-drain");
    let host = logged_host(
        Arc::new(EventWorker {
            retained: None,
            wait_for_persisted_log: None,
        }),
        Arc::clone(&store) as Arc<dyn ExecutionLogStore>,
        host_id.clone(),
        context.clone(),
    );
    let session_id = host.begin_control_session();
    let control_loop = tokio::spawn({
        let host = host.clone();
        async move { host.run_control_loop().await }
    });

    host.control_sender()
        .send_async(RuntimeControlCommand::DrainRuntime {
            protocol_version: RUNTIME_CONTROL_PROTOCOL_VERSION.to_string(),
            message_id: ControlMessageId::new(),
            runtime_host_id: host_id.clone(),
            runtime_session_id: session_id,
        })
        .await
        .expect("control drain");
    host.drain().await;
    host.shutdown().await.expect("shutdown");
    control_loop.abort();

    let records = store
        .list_records(LogRecordQuery {
            stream_id: LogStreamId::new(context.execution_scope, host_id),
            execution_id: None,
            attempt_id: None,
            severity: None,
            message_contains: None,
            cursor: None,
            limit: 100,
        })
        .expect("records")
        .records;
    let drain: Vec<_> = records
        .iter()
        .filter(|record| record.message == "runtime.drain")
        .collect();
    assert_eq!(drain.len(), 1);
    let shutdown = records
        .iter()
        .find(|record| record.message == "runtime.shutdown")
        .expect("shutdown record");
    assert_eq!(drain[0].severity, LogLevel::Trace);
    assert_eq!(shutdown.severity, LogLevel::Trace);
    assert!(drain[0].stream_sequence < shutdown.stream_sequence);
}

#[tokio::test]
async fn concurrent_direct_and_control_shutdown_share_one_terminal_owner() {
    let store = Arc::new(InMemoryExecutionLogStore::default());
    let context = log_context("revision-1");
    let host_id = RuntimeHostId::from("host-concurrent-shutdown");
    let host = logged_host(
        Arc::new(EventWorker {
            retained: None,
            wait_for_persisted_log: None,
        }),
        Arc::clone(&store) as Arc<dyn ExecutionLogStore>,
        host_id.clone(),
        context.clone(),
    );
    let session_id = host.begin_control_session();
    let control_loop = tokio::spawn({
        let host = host.clone();
        async move { host.run_control_loop().await }
    });
    let direct = tokio::spawn({
        let host = host.clone();
        async move { host.shutdown().await }
    });
    let control_sender = host.control_sender();
    let control = control_sender.send_async(RuntimeControlCommand::ShutdownRuntime {
        protocol_version: RUNTIME_CONTROL_PROTOCOL_VERSION.to_string(),
        message_id: ControlMessageId::new(),
        runtime_host_id: host_id.clone(),
        runtime_session_id: session_id,
    });

    let (direct, control) = tokio::join!(direct, control);
    direct.expect("direct task").expect("direct shutdown");
    assert!(matches!(
        control.expect("control shutdown"),
        RuntimeControlEvent::CommandResult {
            outcome: ControlCommandOutcome::Confirmed,
            ..
        }
    ));
    control_loop.abort();

    let records = store
        .list_records(LogRecordQuery {
            stream_id: LogStreamId::new(context.execution_scope, host_id),
            execution_id: None,
            attempt_id: None,
            severity: None,
            message_contains: None,
            cursor: None,
            limit: 100,
        })
        .expect("records")
        .records;
    assert_eq!(
        records
            .iter()
            .filter(|record| record.message == "runtime.shutdown")
            .count(),
        1
    );
}

#[tokio::test]
async fn supervision_panic_emits_correlated_error_finish_after_start() {
    let store = Arc::new(InMemoryExecutionLogStore::default());
    let context = log_context("revision-1");
    let host_id = RuntimeHostId::from("host-panic");
    let host = logged_host(
        Arc::new(PanicWorker),
        Arc::clone(&store) as Arc<dyn ExecutionLogStore>,
        host_id.clone(),
        context.clone(),
    );
    let request = invocation_request();

    let response = host
        .router()
        .oneshot(
            Request::post("/invoke")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&request).expect("request")))
                .expect("http request"),
        )
        .await
        .expect("invoke response");
    assert_eq!(response.status(), axum::http::StatusCode::BAD_GATEWAY);
    host.shutdown().await.expect("shutdown");

    let records = store
        .list_records(LogRecordQuery {
            stream_id: LogStreamId::new(context.execution_scope, host_id),
            execution_id: Some(request.execution_id.clone()),
            attempt_id: Some(request.attempt_id.clone()),
            severity: None,
            message_contains: None,
            cursor: None,
            limit: 100,
        })
        .expect("records")
        .records;
    let start = records
        .iter()
        .find(|record| record.message == "invocation.start")
        .expect("start record");
    let finish = records
        .iter()
        .find(|record| record.message == "invocation.finish")
        .expect("finish record");
    assert!(start.stream_sequence < finish.stream_sequence);
    assert_eq!(finish.severity, LogLevel::Error);
    assert!(finish.correlation.as_ref().is_some_and(|correlation| {
        correlation.execution_id == request.execution_id
            && correlation.attempt_id == request.attempt_id
    }));
}

#[tokio::test]
async fn failing_terminate_still_emits_exactly_one_correlated_finish() {
    let store = Arc::new(InMemoryExecutionLogStore::default());
    let context = log_context("revision-1");
    let host_id = RuntimeHostId::from("host-terminate-failure");
    let host = logged_host(
        Arc::new(FailingTerminateWorker),
        Arc::clone(&store) as Arc<dyn ExecutionLogStore>,
        host_id.clone(),
        context.clone(),
    );
    let request = invocation_request();

    let response = host
        .router()
        .oneshot(
            Request::post("/invoke")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&request).expect("request")))
                .expect("http request"),
        )
        .await
        .expect("invoke response");
    assert_eq!(response.status(), axum::http::StatusCode::BAD_GATEWAY);
    host.shutdown().await.expect("shutdown");

    let records = store
        .list_records(LogRecordQuery {
            stream_id: LogStreamId::new(context.execution_scope, host_id),
            execution_id: Some(request.execution_id.clone()),
            attempt_id: Some(request.attempt_id.clone()),
            severity: None,
            message_contains: None,
            cursor: None,
            limit: 100,
        })
        .expect("records")
        .records;
    let finishes: Vec<_> = records
        .iter()
        .filter(|record| record.message == "invocation.finish")
        .collect();
    assert_eq!(finishes.len(), 1);
    assert_eq!(finishes[0].severity, LogLevel::Error);
    assert!(finishes[0].correlation.as_ref().is_some_and(|correlation| {
        correlation.execution_id == request.execution_id
            && correlation.attempt_id == request.attempt_id
    }));
}

#[tokio::test]
async fn logged_startup_enrollment_finishes_before_shutdown_terminalizes() {
    let store = Arc::new(InMemoryExecutionLogStore::default());
    let context = log_context("revision-1");
    let host_id = RuntimeHostId::from("host-startup-race");
    let entered = Arc::new(tokio::sync::Notify::new());
    let config = RuntimeLogWriterConfig {
        minimum_level: LogLevel::Trace,
        batch_size: 1,
        ..RuntimeLogWriterConfig::default()
    };
    let host = RuntimeHost::logged(
        Arc::new(BlockingStartFactory {
            entered: Arc::clone(&entered),
        }),
        host_id.clone(),
        None,
        None,
        context.clone(),
        Arc::clone(&store) as Arc<dyn ExecutionLogStore>,
        config,
        None,
    )
    .expect("logged host");
    let request = invocation_request();
    let entered_start = entered.notified();
    let invoke = tokio::spawn({
        let host = host.clone();
        let request = request.clone();
        async move {
            host.router()
                .oneshot(
                    Request::post("/invoke")
                        .header("content-type", "application/json")
                        .body(Body::from(serde_json::to_vec(&request).expect("request")))
                        .expect("http request"),
                )
                .await
                .expect("invoke response")
        }
    });
    entered_start.await;

    host.shutdown().await.expect("shutdown");
    assert_eq!(
        invoke.await.expect("invoke task").status(),
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    );

    let records = store
        .list_records(LogRecordQuery {
            stream_id: LogStreamId::new(context.execution_scope, host_id),
            execution_id: Some(request.execution_id.clone()),
            attempt_id: Some(request.attempt_id.clone()),
            severity: None,
            message_contains: None,
            cursor: None,
            limit: 100,
        })
        .expect("records")
        .records;
    assert_eq!(
        records
            .iter()
            .filter(|record| record.message == "invocation.start")
            .count(),
        1
    );
    assert_eq!(
        records
            .iter()
            .filter(|record| record.message == "invocation.finish")
            .count(),
        1
    );
    let finish = records
        .iter()
        .find(|record| record.message == "invocation.finish")
        .expect("finish");
    let shutdown = store
        .list_records(LogRecordQuery {
            stream_id: finish.stream_id.clone(),
            execution_id: None,
            attempt_id: None,
            severity: None,
            message_contains: None,
            cursor: None,
            limit: 100,
        })
        .expect("all records")
        .records
        .into_iter()
        .find(|record| record.message == "runtime.shutdown")
        .expect("shutdown");
    assert!(finish.stream_sequence < shutdown.stream_sequence);
}

#[tokio::test]
async fn panic_recovery_guard_keeps_finish_before_concurrent_shutdown() {
    let store = Arc::new(InMemoryExecutionLogStore::default());
    let context = log_context("revision-1");
    let host_id = RuntimeHostId::from("host-panic-shutdown-race");
    let recovery_entered = Arc::new(tokio::sync::Notify::new());
    let release_recovery = Arc::new(tokio::sync::Notify::new());
    let shutdown_terminate_entered = Arc::new(tokio::sync::Notify::new());
    let host = logged_host(
        Arc::new(PausedPanicRecoveryWorker {
            terminate_calls: AtomicUsize::new(0),
            recovery_entered: Arc::clone(&recovery_entered),
            release_recovery: Arc::clone(&release_recovery),
            shutdown_terminate_entered: Arc::clone(&shutdown_terminate_entered),
        }),
        Arc::clone(&store) as Arc<dyn ExecutionLogStore>,
        host_id.clone(),
        context.clone(),
    );
    let request = invocation_request();
    let recovery_wait = recovery_entered.notified();
    let invoke = tokio::spawn({
        let host = host.clone();
        let request = request.clone();
        async move {
            host.router()
                .oneshot(
                    Request::post("/invoke")
                        .header("content-type", "application/json")
                        .body(Body::from(serde_json::to_vec(&request).expect("request")))
                        .expect("http request"),
                )
                .await
                .expect("invoke response")
        }
    });
    recovery_wait.await;
    let shutdown_terminate_wait = shutdown_terminate_entered.notified();
    let shutdown = tokio::spawn({
        let host = host.clone();
        async move { host.shutdown().await }
    });
    shutdown_terminate_wait.await;
    tokio::task::yield_now().await;

    assert!(!shutdown.is_finished());
    release_recovery.notify_waiters();
    assert_eq!(
        invoke.await.expect("invoke task").status(),
        axum::http::StatusCode::BAD_GATEWAY
    );
    shutdown.await.expect("shutdown task").expect("shutdown");

    let records = store
        .list_records(LogRecordQuery {
            stream_id: LogStreamId::new(context.execution_scope, host_id),
            execution_id: None,
            attempt_id: None,
            severity: None,
            message_contains: None,
            cursor: None,
            limit: 100,
        })
        .expect("records")
        .records;
    let finishes: Vec<_> = records
        .iter()
        .filter(|record| {
            record.message == "invocation.finish"
                && record
                    .correlation
                    .as_ref()
                    .is_some_and(|correlation| correlation.attempt_id == request.attempt_id)
        })
        .collect();
    assert_eq!(finishes.len(), 1);
    let shutdown = records
        .iter()
        .find(|record| record.message == "runtime.shutdown")
        .expect("shutdown record");
    assert!(finishes[0].stream_sequence < shutdown.stream_sequence);
}

fn logged_host(
    worker: Arc<dyn InvocationWorker>,
    store: Arc<dyn ExecutionLogStore>,
    host_id: RuntimeHostId,
    context: RuntimeLogContext,
) -> RuntimeHost {
    let config = RuntimeLogWriterConfig {
        minimum_level: LogLevel::Trace,
        batch_size: 1,
        flush_interval: Duration::from_millis(5),
        ..RuntimeLogWriterConfig::default()
    };
    RuntimeHost::logged(
        Arc::new(TestFactory { worker }),
        host_id,
        None,
        None,
        context,
        store,
        config,
        None,
    )
    .expect("logged host")
}

fn log_context(revision: &str) -> RuntimeLogContext {
    RuntimeLogContext::new(
        ExecutionScopeId::new("scope").expect("scope"),
        "action",
        revision,
        RuntimeKind::Python,
    )
    .expect("context")
}

fn invocation_request() -> InvocationRequest {
    let mut request = InvocationRequest::new(json!({}));
    request.set_deadline(now_unix_ms() + 5_000, 5_000);
    request
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(i64::MAX)
}

fn now_unix_nanos() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_nanos()).ok())
        .unwrap_or(i64::MAX)
}
