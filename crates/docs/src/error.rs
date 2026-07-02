use thiserror::Error;

pub type DocsResult<T> = Result<T, DocsError>;

#[derive(Debug, Error)]
pub enum DocsError {
    #[error("duplicate docs path: {path}")]
    DuplicatePath { path: String },

    #[error("docs provider failed: {source}")]
    ProviderFailed {
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("docs page not found: {path}")]
    PageNotFound { path: String },

    #[error("docs page has invalid content type: {path}")]
    InvalidContentType { path: String },
}
