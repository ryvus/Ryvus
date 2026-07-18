use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use ryvus_execution::{ActorRef, ExecutionScopeId, ExecutionStateStore, MemoryExecutionStateStore};
use ryvus_logging::{
    ExecutionLogStore, FilesystemExecutionLogStore, FilesystemLogStoreConfig,
    InMemoryExecutionLogStore, MemoryLogStoreConfig,
};
use ryvus_persistence::{PostgresExecutionStateStore, PostgresScheduleStore};
use ryvus_runtime_host::{LogOverflowPolicy, RuntimeLogWriterConfig};
use ryvus_scheduler::{MemoryScheduleStore, ScheduleStore};

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

pub struct LocalExecutionComposition {
    pub execution_service: Arc<ryvus_gateway::state::GatewayExecutionService>,
    pub execution_store: Arc<dyn ExecutionStateStore>,
    pub schedule_store: Arc<dyn ScheduleStore>,
    pub log_store: Arc<dyn ExecutionLogStore>,
    pub log_writer_config: RuntimeLogWriterConfig,
    pub execution_scope: ExecutionScopeId,
    pub actor: ActorRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogStoreConfig {
    Memory(MemoryLogStoreConfig),
    Filesystem(FilesystemLogStoreConfig),
}

#[derive(Debug)]
struct LocalLogConfig {
    store: LogStoreConfig,
    writer: RuntimeLogWriterConfig,
}

fn state_stores() -> Result<(Arc<dyn ExecutionStateStore>, Arc<dyn ScheduleStore>)> {
    let config = execution_store_config(
        std::env::var("RYVUS_EXECUTION_STORE").ok(),
        std::env::var("DATABASE_URL").ok(),
    )?;

    match config {
        ExecutionStoreConfig::Memory => Ok((
            Arc::new(MemoryExecutionStateStore::default()),
            Arc::new(MemoryScheduleStore::default()),
        )),
        ExecutionStoreConfig::Postgres { database_url } => {
            let execution_store = PostgresExecutionStateStore::connect(&database_url)
                .map_err(CliError::ExecutionStore)?;
            execution_store
                .active_executions()
                .map_err(CliError::ExecutionStore)?;
            let schedule_store = PostgresScheduleStore::connect(&database_url)
                .map_err(|error| CliError::Validation(error.to_string()))?;
            Ok((Arc::new(execution_store), Arc::new(schedule_store)))
        }
    }
}

pub fn build_local_composition(
    config: &ryvus_gateway::server::GatewayServerConfig,
) -> Result<LocalExecutionComposition> {
    let (execution_store, schedule_store) = state_stores()?;
    let log_config = log_configuration()?;
    let log_store: Arc<dyn ExecutionLogStore> = match &log_config.store {
        LogStoreConfig::Memory(config) => {
            Arc::new(InMemoryExecutionLogStore::new(*config).map_err(CliError::LogStore)?)
        }
        LogStoreConfig::Filesystem(config) => {
            Arc::new(FilesystemExecutionLogStore::new(config.clone()).map_err(CliError::LogStore)?)
        }
    };
    let project_name = config
        .project_root
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| CliError::Validation("project directory has no valid name".into()))?;
    let normalized = project_name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    let execution_scope = ExecutionScopeId::new(format!("local-project:{normalized}"))
        .map_err(|error| CliError::Validation(error.to_string()))?;
    let execution_service = ryvus_gateway::server::build_execution_service_with_stores_and_scope(
        config.project_root.clone(),
        execution_store.clone(),
        execution_scope.clone(),
        log_store.clone(),
        log_config.writer.clone(),
    );
    let actor =
        ActorRef::new("local-user").map_err(|error| CliError::Validation(error.to_string()))?;
    Ok(LocalExecutionComposition {
        execution_service,
        execution_store,
        schedule_store,
        log_store,
        log_writer_config: log_config.writer,
        execution_scope,
        actor,
    })
}

pub fn validate_log_configuration() -> Result<()> {
    let config = log_configuration()?;
    match config.store {
        LogStoreConfig::Memory(config) => {
            InMemoryExecutionLogStore::new(config).map_err(CliError::LogStore)?;
        }
        LogStoreConfig::Filesystem(config) => {
            FilesystemExecutionLogStore::new(config).map_err(CliError::LogStore)?;
        }
    }
    Ok(())
}

fn log_configuration() -> Result<LocalLogConfig> {
    let defaults = RuntimeLogWriterConfig::default();
    let mut writer = defaults.clone();
    writer.capacity = env_usize("RYVUS_LOG_BUFFER_CAPACITY", defaults.capacity)?;
    writer.batch_size = env_usize("RYVUS_LOG_BATCH_SIZE", defaults.batch_size)?;
    writer.flush_interval = env_duration("RYVUS_LOG_FLUSH_INTERVAL_MS", defaults.flush_interval)?;
    writer.retry_max_attempts =
        env_u32("RYVUS_LOG_RETRY_MAX_ATTEMPTS", defaults.retry_max_attempts)?;
    writer.retry_initial_backoff = env_duration(
        "RYVUS_LOG_RETRY_INITIAL_BACKOFF_MS",
        defaults.retry_initial_backoff,
    )?;
    writer.retry_max_backoff =
        env_duration("RYVUS_LOG_RETRY_MAX_BACKOFF_MS", defaults.retry_max_backoff)?;
    writer.grace_period = env_duration("RYVUS_LOG_GRACE_MS", defaults.grace_period)?;
    writer.cleanup_period = env_duration("RYVUS_LOG_CLEANUP_MS", defaults.cleanup_period)?;
    writer.overflow_policy = match std::env::var("RYVUS_LOG_OVERFLOW_POLICY").ok().as_deref() {
        None | Some("drop_newest") => LogOverflowPolicy::DropNewest,
        Some("drop_oldest") => LogOverflowPolicy::DropOldest,
        Some(_) => return invalid_log_config("RYVUS_LOG_OVERFLOW_POLICY"),
    };
    if writer.capacity == 0 || writer.batch_size == 0 || writer.batch_size > writer.capacity {
        return invalid_log_config("RYVUS_LOG_BATCH_SIZE");
    }
    if writer.retry_max_attempts == 0 {
        return invalid_log_config("RYVUS_LOG_RETRY_MAX_ATTEMPTS");
    }
    if writer.retry_initial_backoff > writer.retry_max_backoff {
        return invalid_log_config("RYVUS_LOG_RETRY_INITIAL_BACKOFF_MS");
    }

    let store = match std::env::var("RYVUS_LOG_STORE").ok().as_deref() {
        None | Some("memory") => LogStoreConfig::Memory(MemoryLogStoreConfig {
            max_streams: env_usize(
                "RYVUS_LOG_MEMORY_MAX_STREAMS",
                MemoryLogStoreConfig::default().max_streams,
            )?,
            max_records: env_usize(
                "RYVUS_LOG_MEMORY_MAX_RECORDS",
                MemoryLogStoreConfig::default().max_records,
            )?,
            max_tombstones: env_usize(
                "RYVUS_LOG_MEMORY_MAX_TOMBSTONES",
                MemoryLogStoreConfig::default().max_tombstones,
            )?,
        }),
        Some("filesystem") => {
            let root = std::env::var_os("RYVUS_LOG_FILESYSTEM_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(|| FilesystemLogStoreConfig::default().root);
            let max_batch_bytes = env_usize(
                "RYVUS_LOG_FILESYSTEM_MAX_BATCH_BYTES",
                FilesystemLogStoreConfig::default().max_batch_bytes,
            )?;
            if max_batch_bytes < writer.normalization.max_record_bytes {
                return invalid_log_config("RYVUS_LOG_FILESYSTEM_MAX_BATCH_BYTES");
            }
            probe_writable(&root)?;
            LogStoreConfig::Filesystem(FilesystemLogStoreConfig {
                root,
                max_batch_bytes,
            })
        }
        Some(_) => return invalid_log_config("RYVUS_LOG_STORE"),
    };
    Ok(LocalLogConfig { store, writer })
}

fn env_usize(key: &'static str, default: usize) -> Result<usize> {
    match std::env::var(key) {
        Ok(value) => value
            .parse::<usize>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or(CliError::InvalidLogConfig { key }),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(_) => invalid_log_config(key),
    }
}

fn env_u32(key: &'static str, default: u32) -> Result<u32> {
    match std::env::var(key) {
        Ok(value) => value
            .parse::<u32>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or(CliError::InvalidLogConfig { key }),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(_) => invalid_log_config(key),
    }
}

fn env_duration(key: &'static str, default: Duration) -> Result<Duration> {
    match std::env::var(key) {
        Ok(value) => value
            .parse::<u64>()
            .ok()
            .filter(|value| *value > 0)
            .map(Duration::from_millis)
            .ok_or(CliError::InvalidLogConfig { key }),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(_) => invalid_log_config(key),
    }
}

fn invalid_log_config<T>(key: &'static str) -> Result<T> {
    Err(CliError::InvalidLogConfig { key })
}

fn probe_writable(root: &Path) -> Result<()> {
    fs::create_dir_all(root).map_err(|_| CliError::InvalidLogConfig {
        key: "RYVUS_LOG_FILESYSTEM_ROOT",
    })?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CliError::InvalidLogConfig {
            key: "RYVUS_LOG_FILESYSTEM_ROOT",
        })?
        .as_nanos();
    let path = root.join(format!(".ryvus-write-probe-{}-{nonce}", std::process::id()));
    let result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        file.write_all(b"probe")?;
        file.sync_all()
    })();
    let cleanup = fs::remove_file(&path);
    result.and(cleanup).map_err(|_| CliError::InvalidLogConfig {
        key: "RYVUS_LOG_FILESYSTEM_ROOT",
    })
}

pub fn build_execution_service(
    config: &ryvus_gateway::server::GatewayServerConfig,
) -> Result<Arc<ryvus_gateway::state::GatewayExecutionService>> {
    Ok(build_local_composition(config)?.execution_service)
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

    #[test]
    fn log_config_defaults_and_accepts_typed_values() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_log_environment();
        let defaults = log_configuration().expect("default log config");
        assert!(matches!(defaults.store, LogStoreConfig::Memory(_)));
        assert_eq!(defaults.writer.capacity, 1024);
        assert_eq!(defaults.writer.batch_size, 64);
        assert_eq!(
            defaults.writer.overflow_policy,
            LogOverflowPolicy::DropNewest
        );

        std::env::set_var("RYVUS_LOG_BUFFER_CAPACITY", "16");
        std::env::set_var("RYVUS_LOG_BATCH_SIZE", "8");
        std::env::set_var("RYVUS_LOG_OVERFLOW_POLICY", "drop_oldest");
        std::env::set_var("RYVUS_LOG_MEMORY_MAX_STREAMS", "2");
        std::env::set_var("RYVUS_LOG_MEMORY_MAX_RECORDS", "3");
        std::env::set_var("RYVUS_LOG_MEMORY_MAX_TOMBSTONES", "4");
        let configured = log_configuration().expect("custom log config");
        assert_eq!(configured.writer.capacity, 16);
        assert_eq!(configured.writer.batch_size, 8);
        assert_eq!(
            configured.writer.overflow_policy,
            LogOverflowPolicy::DropOldest
        );
        assert!(matches!(
            configured.store,
            LogStoreConfig::Memory(MemoryLogStoreConfig {
                max_streams: 2,
                max_records: 3,
                max_tombstones: 4
            })
        ));
        clear_log_environment();
    }

    #[test]
    fn log_config_rejects_invalid_values_without_exposing_them() {
        let _guard = ENV_LOCK.lock().unwrap();
        for (key, value) in [
            ("RYVUS_LOG_STORE", "secret-provider"),
            ("RYVUS_LOG_OVERFLOW_POLICY", "secret-policy"),
            ("RYVUS_LOG_BUFFER_CAPACITY", "0"),
            ("RYVUS_LOG_FLUSH_INTERVAL_MS", "not-a-duration"),
            ("RYVUS_LOG_RETRY_MAX_ATTEMPTS", "0"),
            ("RYVUS_LOG_MEMORY_MAX_STREAMS", "0"),
        ] {
            clear_log_environment();
            std::env::set_var(key, value);
            let error = log_configuration().expect_err("configuration should fail");
            assert!(matches!(error, CliError::InvalidLogConfig { key: actual } if actual == key));
            assert!(!error.to_string().contains(value));
        }

        clear_log_environment();
        std::env::set_var("RYVUS_LOG_BUFFER_CAPACITY", "1");
        std::env::set_var("RYVUS_LOG_BATCH_SIZE", "2");
        assert!(matches!(
            log_configuration(),
            Err(CliError::InvalidLogConfig {
                key: "RYVUS_LOG_BATCH_SIZE"
            })
        ));

        clear_log_environment();
        std::env::set_var("RYVUS_LOG_RETRY_INITIAL_BACKOFF_MS", "2");
        std::env::set_var("RYVUS_LOG_RETRY_MAX_BACKOFF_MS", "1");
        assert!(matches!(
            log_configuration(),
            Err(CliError::InvalidLogConfig {
                key: "RYVUS_LOG_RETRY_INITIAL_BACKOFF_MS"
            })
        ));
        clear_log_environment();
    }

    #[test]
    fn filesystem_log_config_probes_root_and_batch_limit() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_log_environment();
        let root = unique_temp_dir("log-filesystem");
        std::env::set_var("RYVUS_LOG_STORE", "filesystem");
        std::env::set_var("RYVUS_LOG_FILESYSTEM_ROOT", &root);
        let configured = log_configuration().expect("writable filesystem config");
        assert!(matches!(configured.store, LogStoreConfig::Filesystem(_)));

        std::env::set_var("RYVUS_LOG_FILESYSTEM_MAX_BATCH_BYTES", "1");
        assert!(matches!(
            log_configuration(),
            Err(CliError::InvalidLogConfig {
                key: "RYVUS_LOG_FILESYSTEM_MAX_BATCH_BYTES"
            })
        ));

        clear_log_environment();
        let file = root.join("not-a-directory");
        fs::write(&file, "occupied").unwrap();
        std::env::set_var("RYVUS_LOG_STORE", "filesystem");
        std::env::set_var("RYVUS_LOG_FILESYSTEM_ROOT", &file);
        assert!(matches!(
            log_configuration(),
            Err(CliError::InvalidLogConfig {
                key: "RYVUS_LOG_FILESYSTEM_ROOT"
            })
        ));
        assert!(!log_configuration()
            .expect_err("root should fail")
            .to_string()
            .contains(file.to_string_lossy().as_ref()));
        clear_log_environment();
        fs::remove_dir_all(root).unwrap();
    }

    fn clear_log_environment() {
        for key in [
            "RYVUS_LOG_STORE",
            "RYVUS_LOG_FILESYSTEM_ROOT",
            "RYVUS_LOG_BUFFER_CAPACITY",
            "RYVUS_LOG_BATCH_SIZE",
            "RYVUS_LOG_FLUSH_INTERVAL_MS",
            "RYVUS_LOG_RETRY_MAX_ATTEMPTS",
            "RYVUS_LOG_RETRY_INITIAL_BACKOFF_MS",
            "RYVUS_LOG_RETRY_MAX_BACKOFF_MS",
            "RYVUS_LOG_OVERFLOW_POLICY",
            "RYVUS_LOG_GRACE_MS",
            "RYVUS_LOG_CLEANUP_MS",
            "RYVUS_LOG_MEMORY_MAX_STREAMS",
            "RYVUS_LOG_MEMORY_MAX_RECORDS",
            "RYVUS_LOG_MEMORY_MAX_TOMBSTONES",
            "RYVUS_LOG_FILESYSTEM_MAX_BATCH_BYTES",
        ] {
            std::env::remove_var(key);
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
