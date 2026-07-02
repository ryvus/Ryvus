use std::{path::PathBuf, sync::Arc};

use ryvus_protocol::{ActionDefinition, RuntimeKind};

use crate::error::{ExecutorError, ExecutorResult};
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
        let action_name = format!("{}::{}", action.source.display(), action.entrypoint);

        match action.runtime {
            RuntimeKind::Python => {
                if !source_path.is_file() {
                    return Err(ExecutorError::RuntimeSourceMissing {
                        runtime: "Python".to_string(),
                        action: action_name,
                        path: source_path.display().to_string(),
                    });
                }

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

            RuntimeKind::Node => {
                if !source_path.is_file() {
                    return Err(ExecutorError::RuntimeSourceMissing {
                        runtime: "Node".to_string(),
                        action: action_name,
                        path: source_path.display().to_string(),
                    });
                }

                Ok(ProcessTarget::new("node")
                    .arg(source_path.to_string_lossy().to_string())
                    .working_dir(&self.project_root))
            }

            RuntimeKind::Rust => {
                let manifest_path = source_path.join("Cargo.toml");

                if !manifest_path.is_file() {
                    return Err(ExecutorError::RuntimeSourceMissing {
                        runtime: "Rust".to_string(),
                        action: action_name,
                        path: manifest_path.display().to_string(),
                    });
                }

                Ok(ProcessTarget::new("cargo").args([
                    "run".to_string(),
                    "--quiet".to_string(),
                    "--manifest-path".to_string(),
                    manifest_path.to_string_lossy().to_string(),
                ]))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use ryvus_protocol::{ActionDefinition, ActionKind, ApiAction, RuntimeKind};

    use crate::{ExecutorError, LocalRuntimeResolver, RuntimeResolver};

    #[test]
    fn reports_missing_runtime_source_file() {
        let root = test_project_root("missing-source");
        let action = ActionDefinition {
            runtime: RuntimeKind::Node,
            kind: ActionKind::Api(ApiAction {
                method: "GET".to_string(),
                path: "/missing".to_string(),
                request_schema: None,
                response_schema: None,
                query_params: Vec::new(),
            }),
            source: "src/missing.js".into(),
            entrypoint: "default".to_string(),
            name: None,
        };

        let resolver = LocalRuntimeResolver::with_project_root(root);
        let error = resolver
            .resolve(&action)
            .expect_err("missing source should fail");

        assert!(matches!(
            error,
            ExecutorError::RuntimeSourceMissing { ref runtime, .. } if runtime == "Node"
        ));
        assert!(error.to_string().contains("src/missing.js"));
    }

    fn test_project_root(name: &str) -> std::path::PathBuf {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("ryvus-resolver-{name}-{id}"));
        fs::create_dir_all(root.join("src")).expect("test project should be created");
        root
    }
}
