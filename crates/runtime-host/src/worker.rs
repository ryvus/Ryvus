use std::sync::Arc;

use async_trait::async_trait;
use ryvus_protocol::{
    InvocationEvent, InvocationRequest, InvocationResult, TerminationReason, WorkerId,
};
use thiserror::Error;
use tokio::time::Instant;

#[derive(Debug, Error)]
pub enum WorkerError {
    #[error("worker process failed to start: {0}")]
    Start(#[source] std::io::Error),
    #[error("worker process operation failed: {0}")]
    Process(#[source] std::io::Error),
    #[error("worker request serialization failed: {0}")]
    Serialize(#[source] serde_json::Error),
    #[error("worker frame deserialization failed: {0}")]
    Deserialize(#[source] serde_json::Error),
    #[error("worker protocol violation: {0}")]
    Protocol(String),
    #[error("worker deadline expired")]
    DeadlineExpired,
}

pub struct StartedWorker {
    pub worker_id: WorkerId,
    pub worker: Arc<dyn InvocationWorker>,
}

pub struct WorkerInvocation {
    pub result: InvocationResult,
    pub events: Vec<InvocationEvent>,
}

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait InvocationWorkerFactory: Send + Sync {
    async fn start(
        &self,
        request: &InvocationRequest,
        worker_id: WorkerId,
    ) -> Result<StartedWorker, WorkerError>;
}

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait InvocationWorker: Send + Sync {
    async fn wait_ready(&self, deadline: Instant) -> Result<(), WorkerError>;

    async fn invoke(
        &self,
        request: InvocationRequest,
        deadline: Instant,
    ) -> Result<WorkerInvocation, WorkerError>;

    async fn terminate(&self, reason: TerminationReason) -> Result<(), WorkerError>;
}
