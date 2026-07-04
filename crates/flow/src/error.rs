use thiserror::Error;

pub type FlowResult<T> = Result<T, FlowError>;

#[derive(Debug, Error)]
pub enum FlowError {
    #[error("flow '{key}' was not found")]
    FlowNotFound { key: String },

    #[error("flow run '{id}' was not found")]
    RunNotFound { id: String },

    #[error("invalid flow '{flow}': {message}")]
    InvalidFlow { flow: String, message: String },

    #[error("invalid flow step '{flow}.{step}': {message}")]
    InvalidStep {
        flow: String,
        step: String,
        message: String,
    },

    #[error("flow action '{action}' was not found")]
    ActionNotFound { action: String },

    #[error("flow step execution failed for '{action}': {message}")]
    ExecutionFailed { action: String, message: String },

    #[error("jsonpath resolution failed for '{expression}': {message}")]
    JsonPath { expression: String, message: String },
}
