use thiserror::Error;

/// The unified error type across Rivus Core.
#[derive(Debug, Error)]
pub enum Error {
    #[error("Action error: {0}")]
    Action(String),

    #[error("Config error: {0}")]
    Config(String),

    #[error("Pipeline error: {0}")]
    Pipeline(String),

    #[error("System error: {0}")]
    System(String),

    #[error("Library error: {0}")]
    Unsupported(String),

    #[error("Not found error: {0}")]
    NotFound(String),
}

impl Error {
    pub fn action(msg: impl Into<String>) -> Self {
        Self::Action(msg.into())
    }

    pub fn pipeline(msg: impl Into<String>) -> Self {
        Self::Pipeline(msg.into())
    }

    pub fn system(msg: impl Into<String>) -> Self {
        Self::System(msg.into())
    }
}
