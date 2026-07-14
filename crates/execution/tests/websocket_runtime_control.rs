use std::{sync::Arc, time::Duration};

use axum::{body::Body, http::Request};
use futures_util::{SinkExt, StreamExt};
use ryvus_execution::{
    RuntimeControlChannel, WebSocketHeaderValidator, WebSocketRuntimeControlChannel,
    WebSocketRuntimeControlOptions,
};
use ryvus_protocol::{
    ActiveAttemptOwnership, AttemptId, ControlCommandOutcome, ControlMessageId, ExecutionId,
    InvocationRequest, RuntimeCapabilities, RuntimeControlCommand, RuntimeControlEvent,
    RuntimeHostId, RuntimeRegistration, RuntimeSessionId, WorkerId,
    RUNTIME_CONTROL_PROTOCOL_VERSION,
};
use ryvus_runtime_host::{
    ProcessInvocationWorkerFactory, ProcessWorkerConfig, RuntimeHost, WebSocketHeaderProvider,
    WebSocketRuntimeHostClient,
};
use tokio::sync::watch;
use tokio_tungstenite::{connect_async, tungstenite::Message, WebSocketStream};
use tower::ServiceExt;

type ClientSocket = WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registration_is_first_and_new_session_fences_the_old_connection() {
    let (service, channel, endpoint, server) = start_server(options()).await;

    let (mut invalid, _) = connect_async(&endpoint).await.unwrap();
    invalid
        .send(json_message(&RuntimeControlEvent::Heartbeat {
            protocol_version: RUNTIME_CONTROL_PROTOCOL_VERSION.to_string(),
            message_id: ControlMessageId::new(),
            runtime_host_id: RuntimeHostId::new(),
            runtime_session_id: RuntimeSessionId::new(),
        }))
        .await
        .unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), invalid.next())
            .await
            .unwrap(),
        None | Some(Ok(Message::Close(_)))
    ));

    let invalid_host = RuntimeHostId::new();
    let invalid_session = RuntimeSessionId::new();
    let (mut invalid, _) = connect_async(&endpoint).await.unwrap();
    let mut invalid_registration = registration(&invalid_host, &invalid_session, vec![]);
    invalid_registration.max_concurrency = 0;
    invalid
        .send(json_message(&invalid_registration))
        .await
        .unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), invalid.next())
            .await
            .unwrap(),
        None | Some(Ok(Message::Close(_)))
    ));

    let host_id = RuntimeHostId::new();
    let first_session = RuntimeSessionId::new();
    let mut first = register(&endpoint, registration(&host_id, &first_session, vec![])).await;
    assert_eq!(
        channel.connected_session(&host_id),
        Some(first_session.clone())
    );

    let second_session = RuntimeSessionId::new();
    let mut second = register(&endpoint, registration(&host_id, &second_session, vec![])).await;
    assert_eq!(
        channel.connected_session(&host_id),
        Some(second_session.clone())
    );

    let stale_attempt = ActiveAttemptOwnership {
        execution_id: ExecutionId::new(),
        attempt_id: AttemptId::new(),
        attempt_number: 1,
        worker_id: WorkerId::new(),
    };
    let stale_attempt_id = stale_attempt.attempt_id.clone();
    let _ = first
        .send(json_message(&RuntimeControlEvent::AttemptStarted {
            protocol_version: RUNTIME_CONTROL_PROTOCOL_VERSION.to_string(),
            message_id: ControlMessageId::new(),
            runtime_host_id: host_id.clone(),
            runtime_session_id: first_session,
            execution_id: stale_attempt.execution_id.clone(),
            attempt_id: stale_attempt.attempt_id.clone(),
            attempt_number: stale_attempt.attempt_number,
            worker_id: stale_attempt.worker_id,
        }))
        .await;
    let _ = first.close(None).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(channel.connected_session(&host_id), Some(second_session));
    assert!(service.attempt_ownership(&stale_attempt_id).is_none());
    assert!(second.send(Message::Ping(Vec::new().into())).await.is_ok());
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authentication_validator_rejects_before_upgrade() {
    let validator: WebSocketHeaderValidator = Arc::new(|_| false);
    let (_service, _channel, endpoint, server) =
        start_server_with_auth(options(), Some(validator)).await;

    assert!(connect_async(endpoint).await.is_err());
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn command_results_are_correlated_and_disconnect_fails_pending_commands() {
    let (_service, channel, endpoint, server) = start_server(options()).await;
    let host_id = RuntimeHostId::new();
    let session_id = RuntimeSessionId::new();
    let mut socket = register(&endpoint, registration(&host_id, &session_id, vec![])).await;
    let command = drain_command(&host_id, &session_id);
    let expected_command_id = command_id(&command).clone();

    let (done_tx, mut done_rx) = tokio::sync::mpsc::unbounded_channel();
    let sender = Arc::clone(&channel);
    std::thread::spawn(move || {
        let _ = done_tx.send(sender.send(command));
    });
    let delivered = receive_command(&mut socket).await;
    assert_eq!(command_id(&delivered), &expected_command_id);

    socket
        .send(json_message(&command_result(
            &host_id,
            &session_id,
            ControlMessageId::new(),
        )))
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(40), done_rx.recv())
            .await
            .is_err()
    );

    socket
        .send(json_message(&command_result(
            &host_id,
            &session_id,
            expected_command_id,
        )))
        .await
        .unwrap();
    assert!(done_rx.recv().await.unwrap().is_ok());

    let sender = Arc::clone(&channel);
    let command = drain_command(&host_id, &session_id);
    let (failed_tx, failed_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = failed_tx.send(sender.send(command));
    });
    let _ = receive_command(&mut socket).await;
    socket.close(None).await.unwrap();
    assert!(failed_rx
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
        .is_err());
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn heartbeat_expiry_removes_transport_routing_only() {
    let mut transport_options = options();
    transport_options.heartbeat_timeout = Duration::from_millis(60);
    let (_service, channel, endpoint, server) = start_server(transport_options).await;
    let host_id = RuntimeHostId::new();
    let session_id = RuntimeSessionId::new();
    let _socket = register(&endpoint, registration(&host_id, &session_id, vec![])).await;

    assert!(wait_until_value(Duration::from_secs(1), || {
        let channel = Arc::clone(&channel);
        let host_id = host_id.clone();
        async move { channel.connected_session(&host_id).is_none().then_some(()) }
    })
    .await
    .is_some());
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_reconnect_reports_active_attempt_and_routes_exact_duplicate_command() {
    let validator: WebSocketHeaderValidator = Arc::new(|headers| {
        headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            == Some("Bearer integration-test")
    });
    let (service, channel, endpoint, server) =
        start_server_with_auth(options(), Some(validator)).await;
    let host = sleeping_host();
    let host_id = host.identity().0;
    let control_host = host.clone();
    tokio::spawn(async move { control_host.run_control_loop().await });

    let request = invocation();
    let execution_id = request.execution_id.clone();
    let attempt_id = request.attempt_id.clone();
    let invocation_host = host.clone();
    let invocation = tokio::spawn(async move {
        invocation_host
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
    wait_for_active(&host).await;

    let client = WebSocketRuntimeHostClient::new(endpoint, "test-revision")
        .heartbeat_interval(Duration::from_millis(20))
        .reconnect_backoff(Duration::from_millis(10), Duration::from_millis(30))
        .header_provider({
            let provider: WebSocketHeaderProvider = Arc::new(|headers| {
                headers.insert(
                    "authorization",
                    tokio_tungstenite::tungstenite::http::HeaderValue::from_static(
                        "Bearer integration-test",
                    ),
                );
                Ok(())
            });
            provider
        });
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let client_host = host.clone();
    let client_task = tokio::spawn(async move { client.run(client_host, shutdown_rx).await });

    let first_session = wait_for_ownership(&service, &attempt_id)
        .await
        .runtime_session_id;
    channel.disconnect(&host_id);
    let ownership = wait_until_value(Duration::from_secs(2), || {
        let service = service.clone();
        let attempt_id = attempt_id.clone();
        let first_session = first_session.clone();
        async move {
            service
                .attempt_ownership(&attempt_id)
                .filter(|current| current.runtime_session_id != first_session)
        }
    })
    .await
    .expect("client did not reconnect with active attempt snapshot");
    assert_eq!(ownership.runtime_host_id, host_id);

    let message_id = ControlMessageId::new();
    let duplicate_command = RuntimeControlCommand::DrainRuntime {
        protocol_version: RUNTIME_CONTROL_PROTOCOL_VERSION.to_string(),
        message_id: message_id.clone(),
        runtime_host_id: ownership.runtime_host_id.clone(),
        runtime_session_id: ownership.runtime_session_id.clone(),
    };
    let first = send_blocking(Arc::clone(&channel), duplicate_command.clone()).await;
    let duplicate = send_blocking(Arc::clone(&channel), duplicate_command).await;
    assert_eq!(outcome(&first), ControlCommandOutcome::Confirmed);
    assert_eq!(first, duplicate);
    assert_eq!(
        tokio::task::spawn_blocking(move || service.cancel(&execution_id).unwrap())
            .await
            .unwrap(),
        ControlCommandOutcome::Confirmed
    );

    tokio::time::timeout(Duration::from_secs(2), invocation)
        .await
        .expect("terminated invocation did not finish")
        .unwrap();
    shutdown_tx.send(true).unwrap();
    client_task.await.unwrap();
    server.abort();
}

async fn start_server(
    options: WebSocketRuntimeControlOptions,
) -> (
    ryvus_execution::RuntimeControlService,
    Arc<WebSocketRuntimeControlChannel>,
    String,
    tokio::task::JoinHandle<()>,
) {
    start_server_with_auth(options, None).await
}

async fn start_server_with_auth(
    options: WebSocketRuntimeControlOptions,
    validator: Option<WebSocketHeaderValidator>,
) -> (
    ryvus_execution::RuntimeControlService,
    Arc<WebSocketRuntimeControlChannel>,
    String,
    tokio::task::JoinHandle<()>,
) {
    let (service, channel) = WebSocketRuntimeControlChannel::new(options, validator);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = Arc::clone(&channel).router();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (
        service,
        channel,
        format!("ws://{address}/runtime-control"),
        server,
    )
}

fn options() -> WebSocketRuntimeControlOptions {
    WebSocketRuntimeControlOptions {
        registration_timeout: Duration::from_millis(250),
        heartbeat_timeout: Duration::from_secs(2),
        command_timeout: Duration::from_secs(1),
    }
}

async fn register(endpoint: &str, registration: RuntimeRegistration) -> ClientSocket {
    let (mut socket, _) = connect_async(endpoint).await.unwrap();
    socket.send(json_message(&registration)).await.unwrap();
    let event: RuntimeControlEvent = serde_json::from_str(
        socket
            .next()
            .await
            .unwrap()
            .unwrap()
            .into_text()
            .unwrap()
            .as_ref(),
    )
    .unwrap();
    assert!(matches!(event, RuntimeControlEvent::Registered { .. }));
    socket
}

fn registration(
    host: &RuntimeHostId,
    session: &RuntimeSessionId,
    active_attempts: Vec<ActiveAttemptOwnership>,
) -> RuntimeRegistration {
    RuntimeRegistration {
        protocol_version: RUNTIME_CONTROL_PROTOCOL_VERSION.to_string(),
        message_id: ControlMessageId::new(),
        runtime_host_id: host.clone(),
        runtime_session_id: session.clone(),
        revision: "test".into(),
        max_concurrency: 1,
        capabilities: RuntimeCapabilities {
            terminate_attempt: true,
            drain: true,
            shutdown: true,
        },
        active_attempts,
    }
}

fn drain_command(host: &RuntimeHostId, session: &RuntimeSessionId) -> RuntimeControlCommand {
    RuntimeControlCommand::DrainRuntime {
        protocol_version: RUNTIME_CONTROL_PROTOCOL_VERSION.to_string(),
        message_id: ControlMessageId::new(),
        runtime_host_id: host.clone(),
        runtime_session_id: session.clone(),
    }
}

fn command_result(
    host: &RuntimeHostId,
    session: &RuntimeSessionId,
    command_message_id: ControlMessageId,
) -> RuntimeControlEvent {
    RuntimeControlEvent::CommandResult {
        protocol_version: RUNTIME_CONTROL_PROTOCOL_VERSION.to_string(),
        message_id: ControlMessageId::new(),
        runtime_host_id: host.clone(),
        runtime_session_id: session.clone(),
        command_message_id,
        outcome: ControlCommandOutcome::Confirmed,
        message: None,
    }
}

fn command_id(command: &RuntimeControlCommand) -> &ControlMessageId {
    match command {
        RuntimeControlCommand::TerminateAttempt { message_id, .. }
        | RuntimeControlCommand::DrainRuntime { message_id, .. }
        | RuntimeControlCommand::ShutdownRuntime { message_id, .. } => message_id,
    }
}

async fn receive_command(socket: &mut ClientSocket) -> RuntimeControlCommand {
    loop {
        let message = socket.next().await.unwrap().unwrap();
        if let Message::Text(text) = message {
            return serde_json::from_str(&text).unwrap();
        }
    }
}

fn json_message(value: &impl serde::Serialize) -> Message {
    Message::Text(serde_json::to_string(value).unwrap().into())
}

fn outcome(event: &RuntimeControlEvent) -> ControlCommandOutcome {
    match event {
        RuntimeControlEvent::CommandResult { outcome, .. } => *outcome,
        _ => panic!("expected command result"),
    }
}

fn sleeping_host() -> RuntimeHost {
    let script = r#"
import json, sys, time
print(json.dumps({"type":"ready"}), flush=True)
json.loads(sys.stdin.readline())
time.sleep(30)
"#;
    RuntimeHost::new(Arc::new(ProcessInvocationWorkerFactory::new(
        ProcessWorkerConfig::new("python3")
            .arg("-u")
            .arg("-c")
            .arg(script),
    )))
}

fn invocation() -> InvocationRequest {
    let mut request = InvocationRequest::new(serde_json::json!({}));
    request.set_deadline(now_unix_ms() + 10_000, 10_000);
    request
}

fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .try_into()
        .unwrap()
}

async fn wait_for_active(host: &RuntimeHost) {
    wait_until_value(Duration::from_secs(2), || {
        let host = host.clone();
        async move { host.active_attempt().await }
    })
    .await
    .expect("worker did not become active");
}

async fn wait_for_ownership(
    service: &ryvus_execution::RuntimeControlService,
    attempt_id: &AttemptId,
) -> ryvus_execution::AttemptOwnership {
    wait_until_value(Duration::from_secs(2), || {
        let service = service.clone();
        let attempt_id = attempt_id.clone();
        async move { service.attempt_ownership(&attempt_id) }
    })
    .await
    .expect("active attempt was not registered")
}

async fn send_blocking(
    channel: Arc<WebSocketRuntimeControlChannel>,
    command: RuntimeControlCommand,
) -> RuntimeControlEvent {
    tokio::task::spawn_blocking(move || channel.send(command).unwrap())
        .await
        .unwrap()
}

async fn wait_until_value<T, F, Fut>(timeout: Duration, mut value: F) -> Option<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(value) = value().await {
            return Some(value);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
