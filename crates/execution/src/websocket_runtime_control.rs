//! Hosted runtime-control transport.
//!
//! Socket I/O and correlation live in asynchronous connection actors. The existing synchronous
//! channel API waits on a standard response channel; on a multi-thread Tokio runtime it uses
//! `block_in_place`, while current-thread runtimes are rejected to avoid blocking their executor.

use std::{
    collections::HashMap,
    sync::{mpsc as std_mpsc, Arc, Mutex, OnceLock},
    time::Duration,
};

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};
use futures_util::{SinkExt, StreamExt};
use ryvus_protocol::{
    ControlMessageId, RuntimeControlCommand, RuntimeControlEvent, RuntimeHostId,
    RuntimeRegistration, RuntimeSessionId, RUNTIME_CONTROL_PROTOCOL_VERSION,
};
use tokio::sync::mpsc;

use crate::{
    ExecutionStateStore, RuntimeControlChannel, RuntimeControlError, RuntimeControlIngress,
    RuntimeControlResult, RuntimeControlService,
};

pub type WebSocketHeaderValidator = Arc<dyn Fn(&HeaderMap) -> bool + Send + Sync>;

#[derive(Debug, Clone)]
pub struct WebSocketRuntimeControlOptions {
    pub registration_timeout: Duration,
    pub heartbeat_timeout: Duration,
    pub command_timeout: Duration,
}

impl Default for WebSocketRuntimeControlOptions {
    fn default() -> Self {
        Self {
            registration_timeout: Duration::from_secs(5),
            heartbeat_timeout: Duration::from_secs(15),
            command_timeout: Duration::from_secs(5),
        }
    }
}

pub struct WebSocketRuntimeControlChannel {
    ingress: OnceLock<RuntimeControlIngress>,
    options: WebSocketRuntimeControlOptions,
    header_validator: Option<WebSocketHeaderValidator>,
    connections: Mutex<HashMap<RuntimeHostId, ConnectedSession>>,
}

#[derive(Clone)]
struct ConnectedSession {
    runtime_session_id: RuntimeSessionId,
    outbound: mpsc::UnboundedSender<Outbound>,
}

enum Outbound {
    Command {
        command: RuntimeControlCommand,
        response: std_mpsc::Sender<Result<RuntimeControlEvent, String>>,
    },
    Cancel(ControlMessageId),
    Close,
}

impl WebSocketRuntimeControlChannel {
    /// Creates the mutually connected service and transport without exposing service internals.
    pub fn new(
        options: WebSocketRuntimeControlOptions,
        header_validator: Option<WebSocketHeaderValidator>,
        store: Arc<dyn ExecutionStateStore>,
    ) -> (RuntimeControlService, Arc<Self>) {
        let channel = Arc::new(Self {
            ingress: OnceLock::new(),
            options,
            header_validator,
            connections: Mutex::new(HashMap::new()),
        });
        let service = RuntimeControlService::new(channel.clone(), store);
        assert!(
            channel.ingress.set(service.ingress()).is_ok(),
            "runtime control ingress should only be bound once"
        );
        (service, channel)
    }

    pub fn router(self: Arc<Self>) -> Router {
        Router::new()
            .route("/runtime-control", get(websocket_upgrade))
            .with_state(self)
    }

    pub fn connected_session(&self, runtime_host_id: &RuntimeHostId) -> Option<RuntimeSessionId> {
        self.connections
            .lock()
            .expect("websocket connections should lock")
            .get(runtime_host_id)
            .map(|connection| connection.runtime_session_id.clone())
    }

    pub fn disconnect(&self, runtime_host_id: &RuntimeHostId) {
        if let Some(connection) = self
            .connections
            .lock()
            .expect("websocket connections should lock")
            .remove(runtime_host_id)
        {
            let _ = connection.outbound.send(Outbound::Close);
        }
    }

    async fn serve(self: Arc<Self>, mut socket: WebSocket) {
        let registration = tokio::time::timeout(
            self.options.registration_timeout,
            receive_registration(&mut socket),
        )
        .await;
        let Ok(Ok(registration)) = registration else {
            let _ = socket.close().await;
            return;
        };
        if self
            .ingress
            .get()
            .expect("runtime control ingress should be bound")
            .register(registration.clone())
            .is_err()
        {
            let _ = socket.close().await;
            return;
        }

        let (outbound_tx, outbound_rx) = mpsc::unbounded_channel();
        let previous = self
            .connections
            .lock()
            .expect("websocket connections should lock")
            .insert(
                registration.runtime_host_id.clone(),
                ConnectedSession {
                    runtime_session_id: registration.runtime_session_id.clone(),
                    outbound: outbound_tx,
                },
            );
        if let Some(previous) = previous {
            let _ = previous.outbound.send(Outbound::Close);
        }

        let acknowledgement = RuntimeControlEvent::Registered {
            protocol_version: RUNTIME_CONTROL_PROTOCOL_VERSION.to_string(),
            message_id: ControlMessageId::new(),
            runtime_host_id: registration.runtime_host_id.clone(),
            runtime_session_id: registration.runtime_session_id.clone(),
        };
        if send_json(&mut socket, &acknowledgement).await.is_ok() {
            let ingress = self
                .ingress
                .get()
                .expect("runtime control ingress should be bound")
                .clone();
            tokio::task::spawn_blocking(move || {
                if let Err(error) = ingress.reconcile_cancellations() {
                    tracing::debug!(%error, "runtime cancellation reconciliation deferred");
                }
            });
            self.run_connection(socket, registration, outbound_rx).await;
        }
    }

    async fn run_connection(
        &self,
        mut socket: WebSocket,
        registration: RuntimeRegistration,
        mut outbound_rx: mpsc::UnboundedReceiver<Outbound>,
    ) {
        let mut pending = HashMap::<
            ControlMessageId,
            std_mpsc::Sender<Result<RuntimeControlEvent, String>>,
        >::new();
        let heartbeat = tokio::time::sleep(self.options.heartbeat_timeout);
        tokio::pin!(heartbeat);

        loop {
            tokio::select! {
                _ = &mut heartbeat => break,
                outbound = outbound_rx.recv() => match outbound {
                    Some(Outbound::Command { command, response }) => {
                        let message_id = command_message_id(&command).clone();
                        if send_json(&mut socket, &command).await.is_err() {
                            let _ = response.send(Err("runtime control connection closed".to_string()));
                            break;
                        }
                        pending.insert(message_id, response);
                    }
                    Some(Outbound::Cancel(message_id)) => {
                        pending.remove(&message_id);
                    }
                    Some(Outbound::Close) | None => break,
                },
                message = socket.next() => {
                    let Some(Ok(Message::Text(text))) = message else { break; };
                    let Ok(event) = serde_json::from_str::<RuntimeControlEvent>(&text) else {
                        tracing::warn!("ignoring invalid runtime-control websocket event");
                        continue;
                    };
                    if self
                        .ingress
                        .get()
                        .expect("runtime control ingress should be bound")
                        .apply(event.clone())
                        .is_err()
                    {
                        tracing::warn!("ignoring stale runtime-control websocket event");
                        continue;
                    }
                    if matches!(event, RuntimeControlEvent::Heartbeat { .. }) {
                        heartbeat.as_mut().reset(tokio::time::Instant::now() + self.options.heartbeat_timeout);
                    }
                    if let RuntimeControlEvent::CommandResult { command_message_id, .. } = &event {
                        if let Some(response) = pending.remove(command_message_id) {
                            let _ = response.send(Ok(event));
                        } else {
                            tracing::warn!(%command_message_id, "ignoring unknown runtime-control command result");
                        }
                    }
                }
            }
        }

        for (_, response) in pending.drain() {
            let _ = response.send(Err("runtime control connection closed".to_string()));
        }
        let _ = socket.close().await;
        let mut connections = self
            .connections
            .lock()
            .expect("websocket connections should lock");
        if connections
            .get(&registration.runtime_host_id)
            .is_some_and(|current| current.runtime_session_id == registration.runtime_session_id)
        {
            connections.remove(&registration.runtime_host_id);
        }
    }
}

impl RuntimeControlChannel for WebSocketRuntimeControlChannel {
    fn send(&self, command: RuntimeControlCommand) -> RuntimeControlResult<RuntimeControlEvent> {
        if tokio::runtime::Handle::try_current().is_ok_and(|handle| {
            handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::CurrentThread
        }) {
            return Err(RuntimeControlError::Channel(
                "synchronous runtime control must run outside a current-thread Tokio runtime"
                    .into(),
            ));
        }
        command
            .validate()
            .map_err(|error| RuntimeControlError::InvalidMessage(error.to_string()))?;
        let (runtime_host_id, runtime_session_id) = command_identity(&command);
        let connection = self
            .connections
            .lock()
            .expect("websocket connections should lock")
            .get(runtime_host_id)
            .filter(|connection| &connection.runtime_session_id == runtime_session_id)
            .cloned()
            .ok_or_else(|| {
                RuntimeControlError::Channel(format!(
                    "runtime session '{runtime_session_id}' is not connected"
                ))
            })?;
        let message_id = command_message_id(&command).clone();
        let (response_tx, response_rx) = std_mpsc::channel();
        connection
            .outbound
            .send(Outbound::Command {
                command,
                response: response_tx,
            })
            .map_err(|_| {
                RuntimeControlError::Channel("runtime control connection closed".into())
            })?;

        let wait = || response_rx.recv_timeout(self.options.command_timeout);
        let response = if tokio::runtime::Handle::try_current().is_ok() {
            tokio::task::block_in_place(wait)
        } else {
            wait()
        };
        match response {
            Ok(Ok(event)) => Ok(event),
            Ok(Err(error)) => Err(RuntimeControlError::Channel(error)),
            Err(error) => {
                let _ = connection.outbound.send(Outbound::Cancel(message_id));
                Err(RuntimeControlError::Channel(error.to_string()))
            }
        }
    }
}

async fn websocket_upgrade(
    State(channel): State<Arc<WebSocketRuntimeControlChannel>>,
    headers: HeaderMap,
    websocket: WebSocketUpgrade,
) -> impl IntoResponse {
    if channel
        .header_validator
        .as_ref()
        .is_some_and(|validate| !validate(&headers))
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    websocket
        .on_upgrade(move |socket| channel.serve(socket))
        .into_response()
}

async fn receive_registration(socket: &mut WebSocket) -> Result<RuntimeRegistration, ()> {
    let Some(Ok(Message::Text(text))) = socket.next().await else {
        return Err(());
    };
    let registration = serde_json::from_str::<RuntimeRegistration>(&text).map_err(|_| ())?;
    registration.validate().map_err(|_| ())?;
    Ok(registration)
}

async fn send_json<T: serde::Serialize>(socket: &mut WebSocket, value: &T) -> Result<(), ()> {
    let text = serde_json::to_string(value).map_err(|_| ())?;
    socket
        .send(Message::Text(text.into()))
        .await
        .map_err(|_| ())
}

fn command_message_id(command: &RuntimeControlCommand) -> &ControlMessageId {
    match command {
        RuntimeControlCommand::TerminateAttempt { message_id, .. }
        | RuntimeControlCommand::DrainRuntime { message_id, .. }
        | RuntimeControlCommand::ShutdownRuntime { message_id, .. } => message_id,
    }
}

fn command_identity(command: &RuntimeControlCommand) -> (&RuntimeHostId, &RuntimeSessionId) {
    match command {
        RuntimeControlCommand::TerminateAttempt {
            runtime_host_id,
            runtime_session_id,
            ..
        }
        | RuntimeControlCommand::DrainRuntime {
            runtime_host_id,
            runtime_session_id,
            ..
        }
        | RuntimeControlCommand::ShutdownRuntime {
            runtime_host_id,
            runtime_session_id,
            ..
        } => (runtime_host_id, runtime_session_id),
    }
}
