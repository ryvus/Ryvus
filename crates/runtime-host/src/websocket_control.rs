use std::{sync::Arc, time::Duration};

use futures_util::{SinkExt, StreamExt};
use ryvus_protocol::{
    ControlMessageId, RuntimeControlCommand, RuntimeControlEvent, RUNTIME_CONTROL_PROTOCOL_VERSION,
};
use thiserror::Error;
use tokio::sync::watch;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, http::HeaderMap, Message},
};

use crate::RuntimeHost;

struct ControlSessionGuard {
    host: RuntimeHost,
    session_id: ryvus_protocol::RuntimeSessionId,
}

impl Drop for ControlSessionGuard {
    fn drop(&mut self) {
        self.host.end_control_session(&self.session_id);
    }
}

pub type WebSocketHeaderProvider = Arc<dyn Fn(&mut HeaderMap) -> Result<(), String> + Send + Sync>;

#[derive(Clone)]
pub struct WebSocketRuntimeHostClient {
    endpoint: String,
    revision: String,
    heartbeat_interval: Duration,
    reconnect_initial: Duration,
    reconnect_max: Duration,
    registration_timeout: Duration,
    header_provider: Option<WebSocketHeaderProvider>,
}

#[derive(Debug, Error)]
pub enum WebSocketRuntimeHostError {
    #[error("invalid WebSocket request: {0}")]
    InvalidRequest(String),
    #[error("WebSocket connection failed: {0}")]
    Connection(String),
    #[error("runtime registration failed: {0}")]
    Registration(String),
    #[error("runtime-control frame failed: {0}")]
    Frame(String),
}

impl WebSocketRuntimeHostClient {
    pub fn new(endpoint: impl Into<String>, revision: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            revision: revision.into(),
            heartbeat_interval: Duration::from_secs(5),
            reconnect_initial: Duration::from_millis(100),
            reconnect_max: Duration::from_secs(5),
            registration_timeout: Duration::from_secs(5),
            header_provider: None,
        }
    }

    pub fn heartbeat_interval(mut self, interval: Duration) -> Self {
        self.heartbeat_interval = interval;
        self
    }

    pub fn reconnect_backoff(mut self, initial: Duration, maximum: Duration) -> Self {
        self.reconnect_initial = initial;
        self.reconnect_max = maximum.max(initial);
        self
    }

    pub fn registration_timeout(mut self, timeout: Duration) -> Self {
        self.registration_timeout = timeout;
        self
    }

    pub fn header_provider(mut self, provider: WebSocketHeaderProvider) -> Self {
        self.header_provider = Some(provider);
        self
    }

    pub async fn run(&self, host: RuntimeHost, mut shutdown: watch::Receiver<bool>) {
        let mut backoff = self.reconnect_initial;
        while !*shutdown.borrow() {
            match self.connect_once(&host, &mut shutdown).await {
                Ok(()) if *shutdown.borrow() => return,
                Ok(()) => backoff = self.reconnect_initial,
                Err(error) => tracing::warn!(%error, "runtime-control WebSocket disconnected"),
            }
            let delay = jitter(backoff);
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        return;
                    }
                }
            }
            backoff = backoff.saturating_mul(2).min(self.reconnect_max);
        }
    }

    async fn connect_once(
        &self,
        host: &RuntimeHost,
        shutdown: &mut watch::Receiver<bool>,
    ) -> Result<(), WebSocketRuntimeHostError> {
        let mut request = self
            .endpoint
            .as_str()
            .into_client_request()
            .map_err(|error| WebSocketRuntimeHostError::InvalidRequest(error.to_string()))?;
        if let Some(provider) = &self.header_provider {
            provider(request.headers_mut()).map_err(WebSocketRuntimeHostError::InvalidRequest)?;
        }
        let (mut socket, _) = connect_async(request)
            .await
            .map_err(|error| WebSocketRuntimeHostError::Connection(error.to_string()))?;

        let session_id = host.begin_control_session();
        let _session = ControlSessionGuard {
            host: host.clone(),
            session_id: session_id.clone(),
        };
        let mut events = host.subscribe_control_events();
        let registration = host.registration(self.revision.clone()).await;
        send_json(&mut socket, &registration).await?;
        let acknowledgement = tokio::time::timeout(self.registration_timeout, socket.next())
            .await
            .map_err(|_| {
                WebSocketRuntimeHostError::Registration("acknowledgement timed out".into())
            })?
            .ok_or_else(|| WebSocketRuntimeHostError::Registration("connection closed".into()))?
            .map_err(|error| WebSocketRuntimeHostError::Registration(error.to_string()))?;
        let Message::Text(acknowledgement) = acknowledgement else {
            return Err(WebSocketRuntimeHostError::Registration(
                "expected registered event".into(),
            ));
        };
        let acknowledgement = serde_json::from_str::<RuntimeControlEvent>(&acknowledgement)
            .map_err(|error| WebSocketRuntimeHostError::Registration(error.to_string()))?;
        if !matches!(
            acknowledgement,
            RuntimeControlEvent::Registered {
                runtime_host_id,
                runtime_session_id,
                ..
            } if runtime_host_id == registration.runtime_host_id && runtime_session_id == session_id
        ) {
            return Err(WebSocketRuntimeHostError::Registration(
                "registration identity mismatch".into(),
            ));
        }

        let control = host.control_sender();
        let mut heartbeat = tokio::time::interval(self.heartbeat_interval);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        heartbeat.tick().await;

        loop {
            tokio::select! {
                _ = heartbeat.tick() => {
                    send_json(&mut socket, &RuntimeControlEvent::Heartbeat {
                        protocol_version: RUNTIME_CONTROL_PROTOCOL_VERSION.to_string(),
                        message_id: ControlMessageId::new(),
                        runtime_host_id: registration.runtime_host_id.clone(),
                        runtime_session_id: session_id.clone(),
                    }).await?;
                }
                event = events.recv() => match event {
                    Ok(event) => send_json(&mut socket, &event).await?,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped, "runtime-control lifecycle events lagged");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
                },
                message = socket.next() => {
                    let Some(message) = message else { return Ok(()); };
                    match message.map_err(|error| WebSocketRuntimeHostError::Frame(error.to_string()))? {
                        Message::Text(text) => {
                            let command = serde_json::from_str::<RuntimeControlCommand>(&text)
                                .map_err(|error| WebSocketRuntimeHostError::Frame(error.to_string()))?;
                            let result = control.send_async(command).await
                                .map_err(WebSocketRuntimeHostError::Frame)?;
                            send_json(&mut socket, &result).await?;
                        }
                        Message::Close(_) => return Ok(()),
                        Message::Ping(payload) => socket.send(Message::Pong(payload)).await
                            .map_err(|error| WebSocketRuntimeHostError::Frame(error.to_string()))?,
                        Message::Pong(_) | Message::Binary(_) | Message::Frame(_) => {}
                    }
                }
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        let _ = socket.close(None).await;
                        return Ok(());
                    }
                }
            }
        }
    }
}

async fn send_json<S, T>(
    socket: &mut tokio_tungstenite::WebSocketStream<S>,
    value: &T,
) -> Result<(), WebSocketRuntimeHostError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    T: serde::Serialize,
{
    let text = serde_json::to_string(value)
        .map_err(|error| WebSocketRuntimeHostError::Frame(error.to_string()))?;
    socket
        .send(Message::Text(text.into()))
        .await
        .map_err(|error| WebSocketRuntimeHostError::Frame(error.to_string()))
}

fn jitter(duration: Duration) -> Duration {
    if duration.is_zero() {
        return duration;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.subsec_nanos());
    let spread = duration / 4;
    let offset = spread.mul_f64(f64::from(nanos) / f64::from(u32::MAX));
    duration.saturating_sub(spread / 2).saturating_add(offset)
}
