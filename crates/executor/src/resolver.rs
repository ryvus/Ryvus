use std::{path::PathBuf, sync::Arc};

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

#[derive(Debug, Clone)]
pub struct LocalRuntimeResolver {
    project_root: PathBuf,
}

impl LocalRuntimeResolver {
    pub fn new() -> Self {
        Self {
            project_root: PathBuf::from("."),
        }
    }

    pub fn with_project_root(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
        }
    }

    fn source_path(&self, action: &ActionDefinition) -> PathBuf {
        if action.source.is_absolute() {
            action.source.clone()
        } else {
            self.project_root.join(&action.source)
        }
    }

    fn python_path(&self) -> String {
        let source_root = self.project_root.join("src");
        let existing = std::env::var("PYTHONPATH").unwrap_or_default();

        if existing.is_empty() {
            source_root.to_string_lossy().to_string()
        } else {
            format!("{}:{}", source_root.display(), existing)
        }
    }
}

impl Default for LocalRuntimeResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeResolver for LocalRuntimeResolver {
    fn resolve(&self, action: &ActionDefinition) -> ExecutorResult<ProcessTarget> {
        let source_path = self.source_path(action);

        match action.runtime {
            RuntimeKind::Python => {
                let venv_python = self.project_root.join(".venv/bin/python");
                let command = if venv_python.exists() {
                    venv_python.to_string_lossy().to_string()
                } else {
                    "python".to_string()
                };

                Ok(ProcessTarget::new(command)
                    .arg(source_path.to_string_lossy().to_string())
                    .working_dir(&self.project_root)
                    .env("PYTHONPATH", self.python_path()))
            }

            RuntimeKind::Node => Ok(ProcessTarget::new("node")
                .arg(source_path.to_string_lossy().to_string())
                .working_dir(&self.project_root)),

            RuntimeKind::Rust => Ok(ProcessTarget::new("cargo").args([
                "run".to_string(),
                "--quiet".to_string(),
                "--manifest-path".to_string(),
                source_path.join("Cargo.toml").to_string_lossy().to_string(),
            ])),
        }
    }
}
