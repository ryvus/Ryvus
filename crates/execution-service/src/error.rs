use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExecutionServiceError {
    #[error("executor error: {0}")]
    Executor(#[from] ryvus_executor::ExecutorError),

    #[error("persistence error: {0}")]
    Persistence(#[from] ryvus_persistence::PersistenceError),
}

pub type ExecutionServiceResult<T> = Result<T, ExecutionServiceError>;
