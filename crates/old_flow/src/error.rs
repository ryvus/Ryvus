use thiserror::Error;

#[derive(Debug, Error)]
pub enum FlowError {
    #[error("Pipeline not found: {0}")]
    PipelineNotFound(String),

    #[error("Storage error: {0}")]
    Store(String),

    #[error("Engine error: {0}")]
    Engine(String),

    #[error("Loader error: {0}")]
    Loader(String),
}
