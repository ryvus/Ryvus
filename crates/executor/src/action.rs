use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ActionDefinition {
    pub runtime: RuntimeKind,
    pub source: PathBuf,
    pub handler: String,
}

impl ActionDefinition {
    pub fn new(
        runtime: RuntimeKind,
        source: impl Into<PathBuf>,
        handler: impl Into<String>,
    ) -> Self {
        Self {
            runtime,
            source: source.into(),
            handler: handler.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub enum RuntimeKind {
    Python,
    Node,
    Rust,
}

impl RuntimeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            RuntimeKind::Python => "python",
            RuntimeKind::Node => "node",
            RuntimeKind::Rust => "rust",
        }
    }
}
