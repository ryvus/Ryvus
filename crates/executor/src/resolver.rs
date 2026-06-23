use std::sync::Arc;

use ryvus_protocol::{ActionDefinition, RuntimeKind};

use crate::error::ExecutorResult;
use crate::target::ProcessTarget;

pub trait RuntimeResolver: Send + Sync {
    fn resolve(&self, action: &ActionDefinition) -> ExecutorResult<ProcessTarget>;
}

impl<T> RuntimeResolver for Arc<T>
where
    T: RuntimeResolver + ?Sized,
{
    fn resolve(&self, action: &ActionDefinition) -> ExecutorResult<ProcessTarget> {
        self.as_ref().resolve(action)
    }
}

#[derive(Debug, Clone, Default)]
pub struct LocalRuntimeResolver;

impl LocalRuntimeResolver {
    pub fn new() -> Self {
        Self
    }
}

impl RuntimeResolver for LocalRuntimeResolver {
    fn resolve(&self, action: &ActionDefinition) -> ExecutorResult<ProcessTarget> {
        let source_path = action.source.clone();

        match action.runtime {
            RuntimeKind::Python => Ok(ProcessTarget::new(".venv/bin/python")
                .arg(source_path.to_string_lossy().to_string())),

            RuntimeKind::Node => {
                Ok(ProcessTarget::new("node").arg(source_path.to_string_lossy().to_string()))
            }

            RuntimeKind::Rust => Ok(ProcessTarget::new("cargo").args([
                "run".to_string(),
                "--quiet".to_string(),
                "--manifest-path".to_string(),
                source_path.join("Cargo.toml").to_string_lossy().to_string(),
            ])),
        }
    }
}
