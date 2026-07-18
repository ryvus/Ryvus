use std::{io, path::PathBuf};

use ryvus_execution::StateStoreError;
use ryvus_logging::LogStoreError;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, CliError>;

#[derive(Debug, Error)]
pub enum CliError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Invalid project: {0}")]
    InvalidProject(String),

    #[error("Project already exists: {0}")]
    ProjectAlreadyExists(String),

    #[error("Project not found: {0}")]
    ProjectNotFound(String),

    #[error("Template error: {0}")]
    Template(String),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Unsupported language: {0}")]
    UnsupportedLanguage(String),

    #[error("Unknown target: {0}")]
    UnknownTarget(String),

    #[error("Build failed: {0}")]
    Build(String),

    #[error("Deployment failed: {0}")]
    Deploy(String),

    #[error("Serialization failed")]
    SerializationFailed,

    #[error("Command failed: {0}")]
    CommandFailed(io::Error),

    #[error("Process failed: {command:?} with status {status:?}")]
    ProcessFailed {
        command: String,
        status: std::process::ExitStatus,
    },
    #[error("gateway failed: {0}")]
    Gateway(String),

    #[error("database URL is required; pass --database-url or set DATABASE_URL")]
    DatabaseUrlRequired,

    #[error("database migration failed")]
    DatabaseMigration,

    #[error("invalid project environment file: {path}")]
    EnvironmentFile { path: PathBuf },

    #[error("invalid RYVUS_EXECUTION_STORE '{value}'; expected 'memory' or 'postgres'")]
    InvalidExecutionStore { value: String },

    #[error("DATABASE_URL is required when RYVUS_EXECUTION_STORE=postgres")]
    ExecutionDatabaseUrlRequired,

    #[error("PostgreSQL execution store initialization failed: {0}")]
    ExecutionStore(#[source] StateStoreError),

    #[error("invalid log configuration: {key}")]
    InvalidLogConfig { key: &'static str },

    #[error("log store initialization failed")]
    LogStore(#[source] LogStoreError),
}
