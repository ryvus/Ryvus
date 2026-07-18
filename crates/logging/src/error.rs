use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LogModelError {
    #[error("{field} must not be empty")]
    EmptyField { field: &'static str },
    #[error("attempt number must be greater than zero")]
    InvalidAttemptNumber,
    #[error("span id requires a trace id")]
    SpanWithoutTrace,
    #[error("{kind} must contain exactly {expected} hexadecimal characters")]
    InvalidHexId { kind: &'static str, expected: usize },
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LogStoreError {
    #[error("invalid log batch: {0}")]
    InvalidBatch(String),
    #[error("log store conflict: {0}")]
    Conflict(String),
    #[error("invalid log query: {0}")]
    InvalidQuery(String),
    #[error("invalid log store configuration: {0}")]
    InvalidConfiguration(String),
    #[error("log stream was not found")]
    NotFound,
    #[error("log store capacity arithmetic overflowed")]
    CapacityOverflow,
    #[error("log store lock is unavailable")]
    Unavailable,
    #[error("log store I/O failed")]
    Io,
    #[error("log store is corrupt")]
    Corruption,
}
