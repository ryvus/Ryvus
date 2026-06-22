use std::sync::Arc;

use crate::action::{ActionDefinition, RuntimeKind};
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
        let handler_path = action.source.join(&action.handler);

        match action.runtime {
            RuntimeKind::Python => {
                Ok(ProcessTarget::new("python3").arg(handler_path.to_string_lossy().to_string()))
            }

            RuntimeKind::Node => {
                Ok(ProcessTarget::new("node").arg(handler_path.to_string_lossy().to_string()))
            }

            RuntimeKind::Rust => Ok(ProcessTarget::new("cargo").args([
                "run".to_string(),
                "--quiet".to_string(),
                "--manifest-path".to_string(),
                action
                    .source
                    .join("Cargo.toml")
                    .to_string_lossy()
                    .to_string(),
            ])),
        }
    }
}
