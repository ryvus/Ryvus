use thiserror::Error;

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("Action failed: {0}")]
    Action(String),
    #[error("Pipeline canceled")]
    Canceled,
    #[error("There was an issue configuring the Action {0}")]
    Config(String),
    #[error("Other error: {0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, EngineError>;
