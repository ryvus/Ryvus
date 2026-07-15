use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use ryvus_execution::{ExecutionStateStore, MemoryExecutionStateStore};
use ryvus_persistence::PostgresExecutionStateStore;

use crate::error::{CliError, Result};

pub fn load_environment() -> Result<()> {
    let root = std::env::current_dir().map_err(CliError::Io)?;
    load_environment_from(&root)
}

fn load_environment_from(root: &Path) -> Result<()> {
    let path = root.join(".env");
    if !path.exists() {
        return Ok(());
    }

    dotenvy::from_path(&path)
        .map(|_| ())
        .map_err(|_| CliError::EnvironmentFile { path })
}

#[derive(Debug, PartialEq, Eq)]
enum ExecutionStoreConfig {
    Memory,
    Postgres { database_url: String },
}

fn execution_store_config(
    selector: Option<String>,
    database_url: Option<String>,
) -> Result<ExecutionStoreConfig> {
    match selector.as_deref() {
        None | Some("memory") => Ok(ExecutionStoreConfig::Memory),
        Some("postgres") => database_url
            .filter(|url| !url.trim().is_empty())
            .map(|database_url| ExecutionStoreConfig::Postgres { database_url })
            .ok_or(CliError::ExecutionDatabaseUrlRequired),
        Some(value) => Err(CliError::InvalidExecutionStore {
            value: value.to_string(),
        }),
    }
}

fn execution_state_store() -> Result<Arc<dyn ExecutionStateStore>> {
    let config = execution_store_config(
        std::env::var("RYVUS_EXECUTION_STORE").ok(),
        std::env::var("DATABASE_URL").ok(),
    )?;

    match config {
        ExecutionStoreConfig::Memory => Ok(Arc::new(MemoryExecutionStateStore::default())),
        ExecutionStoreConfig::Postgres { database_url } => {
            let store = PostgresExecutionStateStore::connect(&database_url)
                .map_err(CliError::ExecutionStore)?;
            store
                .active_executions()
                .map_err(CliError::ExecutionStore)?;
            Ok(Arc::new(store))
        }
    }
}

pub fn build_execution_service(
    config: &ryvus_gateway::server::GatewayServerConfig,
) -> Result<Arc<ryvus_gateway::state::GatewayExecutionService>> {
    let store = execution_state_store()?;
    Ok(ryvus_gateway::server::build_execution_service_with_store(
        config.project_root.clone(),
        store,
    ))
}

pub fn configure_python_path() {
    let Ok(ryvus_root) = ryvus_root() else {
        return;
    };

    prepend_python_path(ryvus_root.join("sdk/python"));
}

pub fn python_path() -> Result<String> {
    let sdk_path = ryvus_root()?.join("sdk/python");
    let existing = std::env::var("PYTHONPATH").unwrap_or_default();

    Ok(if existing.is_empty() {
        sdk_path.display().to_string()
    } else {
        format!("{}:{}", sdk_path.display(), existing)
    })
}

pub fn ryvus_root() -> Result<PathBuf> {
    if let Some(root) = std::env::var_os("RYVUS_ROOT")
        .map(PathBuf::from)
        .and_then(valid_root)
    {
        return Ok(root);
    }

    if let Some(root) = option_env!("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .and_then(|path| path.parent().and_then(Path::parent).map(Path::to_path_buf))
        .and_then(valid_root)
    {
        return Ok(root);
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(root) = find_root_near(&exe) {
            return Ok(root);
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        if let Some(root) = find_root_near(&cwd) {
            return Ok(root);
        }
    }

    Err(CliError::Validation(
        "could not find Ryvus root; set RYVUS_ROOT".to_string(),
    ))
}

fn prepend_python_path(sdk_path: PathBuf) {
    let existing = std::env::var("PYTHONPATH").unwrap_or_default();
    let pythonpath = if existing.is_empty() {
        sdk_path.display().to_string()
    } else {
        format!("{}:{}", sdk_path.display(), existing)
    };

    std::env::set_var("PYTHONPATH", pythonpath);
}

fn find_root_near(path: &Path) -> Option<PathBuf> {
    for ancestor in path.ancestors() {
        if let Some(root) = valid_root(ancestor.to_path_buf()) {
            return Some(root);
        }

        if let Some(root) = valid_root(ancestor.join("ryvus")) {
            return Some(root);
        }
    }

    None
}

fn valid_root(path: PathBuf) -> Option<PathBuf> {
    let root = path.canonicalize().ok()?;

    root.join("sdk/python").is_dir().then_some(root)
}

pub fn gateway_config() -> Result<ryvus_gateway::server::GatewayServerConfig> {
    Ok(ryvus_gateway::server::GatewayServerConfig {
        project_root: std::env::current_dir().map_err(CliError::Io)?,
        manifest_path: ".ryvus/action-manifest.json".into(),
        addr: ([127, 0, 0, 1], 8080).into(),
    })
}

pub fn control_addr() -> std::net::SocketAddr {
    ([127, 0, 0, 1], 8079).into()
}

pub fn print_validation(validation: &ryvus_gateway::server::GatewayValidation) {
    println!("Validated {} action(s)", validation.action_count);

    if validation.routes.is_empty() {
        return;
    }

    println!("Routes:");

    for route in &validation.routes {
        println!("  {:<6} {:<32} {}", route.method, route.path, route.action);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::Mutex,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn missing_dotenv_is_allowed() {
        let root = unique_temp_dir("missing-dotenv");
        fs::create_dir_all(&root).unwrap();

        assert!(load_environment_from(&root).is_ok());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn process_environment_precedes_dotenv() {
        let _guard = ENV_LOCK.lock().unwrap();
        let root = unique_temp_dir("dotenv-precedence");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join(".env"), "RYVUS_DOTENV_TEST=file\n").unwrap();
        std::env::set_var("RYVUS_DOTENV_TEST", "process");

        load_environment_from(&root).unwrap();

        assert_eq!(std::env::var("RYVUS_DOTENV_TEST").unwrap(), "process");
        std::env::remove_var("RYVUS_DOTENV_TEST");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn malformed_dotenv_does_not_expose_contents() {
        let _guard = ENV_LOCK.lock().unwrap();
        let root = unique_temp_dir("malformed-dotenv");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join(".env"), "DATABASE_URL='secret-without-end\n").unwrap();

        let error = load_environment_from(&root).expect_err("malformed file should fail");

        assert!(matches!(error, CliError::EnvironmentFile { .. }));
        assert!(!error.to_string().contains("secret-without-end"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn execution_store_config_defaults_to_memory() {
        assert_eq!(
            execution_store_config(None, None).unwrap(),
            ExecutionStoreConfig::Memory
        );
        assert_eq!(
            execution_store_config(None, Some("postgres://application/database".into())).unwrap(),
            ExecutionStoreConfig::Memory
        );
        assert_eq!(
            execution_store_config(Some("memory".into()), None).unwrap(),
            ExecutionStoreConfig::Memory
        );
    }

    #[test]
    fn execution_store_config_requires_postgres_url() {
        assert!(matches!(
            execution_store_config(Some("postgres".into()), None),
            Err(CliError::ExecutionDatabaseUrlRequired)
        ));
        assert!(matches!(
            execution_store_config(Some("postgres".into()), Some("  ".into())),
            Err(CliError::ExecutionDatabaseUrlRequired)
        ));
        assert_eq!(
            execution_store_config(
                Some("postgres".into()),
                Some("postgres://localhost/ryvus".into())
            )
            .unwrap(),
            ExecutionStoreConfig::Postgres {
                database_url: "postgres://localhost/ryvus".into()
            }
        );
    }

    #[test]
    fn execution_store_config_rejects_unknown_or_empty_selector() {
        for value in ["", "sqlite", "Postgres"] {
            assert!(matches!(
                execution_store_config(Some(value.into()), None),
                Err(CliError::InvalidExecutionStore { value: actual }) if actual == value
            ));
        }
    }

    fn unique_temp_dir(name: &str) -> PathBuf {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("ryvus-{name}-{id}"))
    }
}
