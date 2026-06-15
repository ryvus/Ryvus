use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ActionCatalogError {
    #[error("action '{action}' was not found")]
    ActionNotFound { action: String },

    #[error("failed to load catalog from '{path}': {source}")]
    LoadFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse catalog from '{path}': {source}")]
    ParseFailed {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("invalid action definition for '{action}': {message}")]
    InvalidDefinition { action: String, message: String },

    #[error("action was not found: {0}")]
    NotFound(String),
}

pub type ActionCatalogResult<T> = Result<T, ActionCatalogError>;
