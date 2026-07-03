use async_trait::async_trait;
use std::sync::Arc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Represents any external source capable of triggering a pipeline cancellation.
/// This could be a database monitor, WebSocket listener, HTTP request, etc.
#[async_trait]
pub trait CancellationSource: Send + Sync {
    /// Start monitoring for a cancellation signal.
    /// When triggered, the provided `token.cancel()` should be called.
    async fn monitor(&self, token: CancellationToken);
}

/// Coordinates between an engine and its cancellation source.
/// Spawns background monitoring tasks that trigger the shared cancellation token.
pub struct CancellationListener {
    token: CancellationToken,
    handle: Option<JoinHandle<()>>,
}

impl CancellationListener {
    /// Create a new cancellation listener with its own cancellation token.
    pub fn new() -> Self {
        Self {
            token: CancellationToken::new(),
            handle: None,
        }
    }

    /// Get a clone of the cancellation token (to pass into executors).
    pub fn token(&self) -> CancellationToken {
        self.token.clone()
    }

    /// Start monitoring using a given cancellation source.
    /// This spawns the monitor loop on a background task.
    pub fn start<S>(&mut self, source: Arc<S>)
    where
        S: CancellationSource + 'static,
    {
        let token = self.token.clone();
        self.handle = Some(tokio::spawn(async move {
            source.monitor(token).await;
        }));
    }

    /// Manually trigger cancellation (e.g. via code or CLI).
    pub fn cancel(&self) {
        self.token.cancel();
    }

    /// Optionally wait for the background monitor to finish.
    pub async fn join(self) {
        if let Some(handle) = self.handle {
            let _ = handle.await;
        }
    }
}
