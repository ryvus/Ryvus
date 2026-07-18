use std::{
    collections::HashMap,
    net::TcpListener,
    sync::{mpsc::SyncSender, Arc, Mutex},
    thread::JoinHandle,
    time::{Duration, Instant},
};

use ryvus_logging::{
    ExecutionLogRecord, ExecutionLogStore, InMemoryExecutionLogStore, RuntimeLogContext,
};
use ryvus_protocol::{
    ActiveAttemptOwnership, ExecutionAttempt, RuntimeHostId, RuntimeSessionId, WorkerId,
};
use ryvus_runtime_host::{
    ProcessInvocationWorkerFactory, ProcessWorkerConfig, RuntimeHost, RuntimeLogWriterConfig,
};
use tokio::sync::oneshot;

use crate::{
    AttemptOwnership, ExecutorError, ExecutorResult, InMemoryRuntimeControlChannel,
    LocalProcessTarget, RuntimeControlService, RuntimeTarget,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeOutcome {
    Success,
    HandlerFailure,
    InfrastructureFailure,
    TimedOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeDisposition {
    Unmanaged,
    Stopped,
    Cancelled,
    TimedOut,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeHandle {
    pub runtime_id: String,
    pub attempt: ExecutionAttempt,
    pub endpoint: String,
    managed: bool,
}

impl RuntimeHandle {
    fn managed(
        runtime_id: impl Into<String>,
        attempt: ExecutionAttempt,
        endpoint: impl Into<String>,
    ) -> Self {
        Self {
            runtime_id: runtime_id.into(),
            attempt,
            endpoint: endpoint.into(),
            managed: true,
        }
    }

    pub fn existing(attempt: ExecutionAttempt, endpoint: impl Into<String>) -> Self {
        Self {
            runtime_id: uuid::Uuid::new_v4().to_string(),
            attempt,
            endpoint: endpoint.into(),
            managed: false,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeExit {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct RuntimeRelease {
    pub exit: RuntimeExit,
    pub events: Vec<ryvus_protocol::InvocationEvent>,
    pub disposition: RuntimeDisposition,
}

impl RuntimeRelease {
    fn external() -> Self {
        Self {
            exit: RuntimeExit::default(),
            events: Vec::new(),
            disposition: RuntimeDisposition::Unmanaged,
        }
    }
}

#[cfg_attr(test, mockall::automock)]
pub trait RuntimeManager: Send + Sync {
    fn acquire(
        &self,
        target: &RuntimeTarget,
        attempt: &ExecutionAttempt,
        timeout: Duration,
        log_context: &RuntimeLogContext,
    ) -> ExecutorResult<RuntimeHandle>;

    fn release(
        &self,
        handle: RuntimeHandle,
        outcome: RuntimeOutcome,
    ) -> ExecutorResult<RuntimeRelease>;

    fn shutdown(&self, grace: Duration) -> ExecutorResult<()>;
}

impl<T> RuntimeManager for Arc<T>
where
    T: RuntimeManager + ?Sized,
{
    fn acquire(
        &self,
        target: &RuntimeTarget,
        attempt: &ExecutionAttempt,
        timeout: Duration,
        log_context: &RuntimeLogContext,
    ) -> ExecutorResult<RuntimeHandle> {
        self.as_ref().acquire(target, attempt, timeout, log_context)
    }

    fn release(
        &self,
        handle: RuntimeHandle,
        outcome: RuntimeOutcome,
    ) -> ExecutorResult<RuntimeRelease> {
        self.as_ref().release(handle, outcome)
    }

    fn shutdown(&self, grace: Duration) -> ExecutorResult<()> {
        self.as_ref().shutdown(grace)
    }
}

struct ManagedHost {
    runtime_host_id: RuntimeHostId,
    runtime_host: RuntimeHost,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<Result<(), String>>>,
}

struct RuntimeHostLogging {
    context: RuntimeLogContext,
    store: Arc<dyn ExecutionLogStore>,
    config: RuntimeLogWriterConfig,
    console: Option<SyncSender<ExecutionLogRecord>>,
}

#[derive(Default)]
struct RuntimeState {
    hosts: HashMap<String, ManagedHost>,
    draining: bool,
}

pub struct LocalRuntimeManager {
    state: Mutex<RuntimeState>,
    control: RuntimeControlService,
    channel: Arc<InMemoryRuntimeControlChannel>,
    log_store: Arc<dyn ExecutionLogStore>,
    log_writer_config: RuntimeLogWriterConfig,
    log_console: Option<SyncSender<ExecutionLogRecord>>,
}

impl std::fmt::Debug for LocalRuntimeManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalRuntimeManager")
            .finish_non_exhaustive()
    }
}

impl LocalRuntimeManager {
    pub fn new(
        control: RuntimeControlService,
        channel: Arc<InMemoryRuntimeControlChannel>,
    ) -> Self {
        Self::with_logging(
            control,
            channel,
            Arc::new(InMemoryExecutionLogStore::default()),
            RuntimeLogWriterConfig::default(),
            None,
        )
    }

    pub fn with_logging(
        control: RuntimeControlService,
        channel: Arc<InMemoryRuntimeControlChannel>,
        log_store: Arc<dyn ExecutionLogStore>,
        log_writer_config: RuntimeLogWriterConfig,
        log_console: Option<SyncSender<ExecutionLogRecord>>,
    ) -> Self {
        Self {
            state: Mutex::new(RuntimeState::default()),
            control,
            channel,
            log_store,
            log_writer_config,
            log_console,
        }
    }
}

impl RuntimeManager for LocalRuntimeManager {
    fn acquire(
        &self,
        target: &RuntimeTarget,
        attempt: &ExecutionAttempt,
        timeout: Duration,
        log_context: &RuntimeLogContext,
    ) -> ExecutorResult<RuntimeHandle> {
        if self
            .state
            .lock()
            .expect("runtime state should lock")
            .draining
        {
            return Err(ExecutorError::RuntimeUnavailable);
        }

        let RuntimeTarget::LocalProcess(target) = target else {
            if let RuntimeTarget::Http { endpoint } = target {
                return Ok(RuntimeHandle::existing(attempt.clone(), endpoint));
            }
            return Err(ExecutorError::UnsupportedRuntimeTarget {
                target: format!("{target:?}"),
            });
        };

        let runtime_id = uuid::Uuid::new_v4().to_string();
        let runtime_host_id = RuntimeHostId::new();
        let runtime_session_id = RuntimeSessionId::new();
        let worker_id = WorkerId::new();
        let ownership = AttemptOwnership {
            execution_id: attempt.execution_id.clone(),
            attempt_id: attempt.attempt_id.clone(),
            attempt_number: attempt.attempt_number,
            runtime_host_id: runtime_host_id.clone(),
            runtime_session_id: runtime_session_id.clone(),
            worker_id: worker_id.clone(),
        };
        let (endpoint, host) = start_runtime_host(
            target,
            attempt,
            timeout,
            runtime_host_id.clone(),
            runtime_session_id,
            worker_id,
            RuntimeHostLogging {
                context: log_context.clone(),
                store: self.log_store.clone(),
                config: self.log_writer_config.clone(),
                console: self.log_console.clone(),
            },
        )?;
        self.channel
            .register(runtime_host_id, host.runtime_host.control_sender());
        if let Err(error) = self.control.register_attempt(ownership) {
            self.channel.unregister(&host.runtime_host_id);
            stop_runtime_host(host)?;
            return Err(error.into());
        }
        let mut state = self.state.lock().expect("runtime state should lock");
        if state.draining {
            drop(state);
            self.control.unregister_runtime(&host.runtime_host_id);
            self.channel.unregister(&host.runtime_host_id);
            stop_runtime_host(host)?;
            return Err(ExecutorError::RuntimeUnavailable);
        }
        state.hosts.insert(runtime_id.clone(), host);

        Ok(RuntimeHandle::managed(
            runtime_id,
            attempt.clone(),
            endpoint,
        ))
    }

    fn release(
        &self,
        handle: RuntimeHandle,
        outcome: RuntimeOutcome,
    ) -> ExecutorResult<RuntimeRelease> {
        if !handle.managed {
            if outcome == RuntimeOutcome::TimedOut {
                return Err(ExecutorError::UnsupportedCancellation {
                    endpoint: handle.endpoint,
                });
            }
            return Ok(RuntimeRelease::external());
        }

        let host = {
            let mut state = self.state.lock().expect("runtime state should lock");
            match state.hosts.remove(&handle.runtime_id) {
                Some(host) => host,
                None => {
                    return Err(ExecutorError::RuntimeHandleNotFound {
                        runtime_id: handle.runtime_id,
                    })
                }
            }
        };

        let events = host.runtime_host.take_events(&handle.attempt);
        self.control.unregister_runtime(&host.runtime_host_id);
        self.channel.unregister(&host.runtime_host_id);
        stop_runtime_host(host)?;
        Ok(RuntimeRelease {
            exit: RuntimeExit::default(),
            events,
            disposition: if outcome == RuntimeOutcome::TimedOut {
                RuntimeDisposition::TimedOut
            } else {
                RuntimeDisposition::Stopped
            },
        })
    }

    fn shutdown(&self, _grace: Duration) -> ExecutorResult<()> {
        let hosts = {
            let mut state = self.state.lock().expect("runtime state should lock");
            state.draining = true;
            state.hosts.drain().collect::<Vec<_>>()
        };
        for (_, host) in hosts {
            self.control.unregister_runtime(&host.runtime_host_id);
            self.channel.unregister(&host.runtime_host_id);
            stop_runtime_host(host)?;
        }
        Ok(())
    }
}

fn start_runtime_host(
    target: &LocalProcessTarget,
    attempt: &ExecutionAttempt,
    timeout: Duration,
    runtime_host_id: RuntimeHostId,
    runtime_session_id: RuntimeSessionId,
    worker_id: WorkerId,
    logging: RuntimeHostLogging,
) -> ExecutorResult<(String, ManagedHost)> {
    let config = ProcessWorkerConfig {
        command: target.command.clone(),
        args: target.args.clone(),
        working_dir: target.working_dir.clone(),
        env: target.env.clone(),
    };
    let expected_attempt = ActiveAttemptOwnership {
        execution_id: attempt.execution_id.clone(),
        attempt_id: attempt.attempt_id.clone(),
        attempt_number: attempt.attempt_number,
        worker_id,
    };
    let host = RuntimeHost::logged(
        Arc::new(ProcessInvocationWorkerFactory::new(config)),
        runtime_host_id.clone(),
        Some(runtime_session_id),
        Some(expected_attempt),
        logging.context,
        logging.store,
        logging.config,
        logging.console,
    )?;
    let router = host.router();
    let shutdown_host = host.clone();
    let control_host = host.clone();
    let stopped_host = host.clone();
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    listener.set_nonblocking(true)?;
    let endpoint = format!("http://{}", listener.local_addr()?);
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let thread = std::thread::Builder::new()
        .name(format!("ryvus-runtime-host-{}", attempt.attempt_id))
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| error.to_string())?;
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener)
                    .map_err(|error| error.to_string())?;
                let control = tokio::spawn(async move { control_host.run_control_loop().await });
                let result = axum::serve(listener, router)
                    .with_graceful_shutdown(async move {
                        tokio::select! {
                            _ = shutdown_rx => {
                                if let Err(error) = shutdown_host.shutdown().await {
                                    tracing::error!(%error, "runtime host shutdown failed");
                                }
                            }
                            _ = stopped_host.wait_stopped() => {}
                        }
                    })
                    .await
                    .map_err(|error| error.to_string());
                control.abort();
                result
            })
        })?;
    let managed = ManagedHost {
        runtime_host_id,
        runtime_host: host,
        shutdown: Some(shutdown_tx),
        thread: Some(thread),
    };

    let ready_endpoint = endpoint.clone();
    let ready_attempt = attempt.clone();
    let readiness =
        std::thread::spawn(move || wait_until_ready(&ready_endpoint, &ready_attempt, timeout))
            .join()
            .map_err(|_| ExecutorError::HttpWorkerPanicked)?;
    if let Err(error) = readiness {
        let _ = stop_runtime_host(managed);
        return Err(error);
    }

    Ok((endpoint, managed))
}

fn wait_until_ready(
    endpoint: &str,
    attempt: &ExecutionAttempt,
    timeout: Duration,
) -> ExecutorResult<()> {
    let started = Instant::now();
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(100))
        .build()?;
    let ready_url = format!("{}/ready", endpoint.trim_end_matches('/'));
    while started.elapsed() < timeout {
        if client
            .get(&ready_url)
            .send()
            .is_ok_and(|response| response.status().is_success())
        {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err(ExecutorError::RuntimeReadinessTimedOut {
        attempt: attempt.clone(),
        endpoint: endpoint.to_string(),
        timeout_ms: timeout.as_millis(),
    })
}

fn stop_runtime_host(mut host: ManagedHost) -> ExecutorResult<()> {
    if let Some(shutdown) = host.shutdown.take() {
        let _ = shutdown.send(());
    }
    if let Some(thread) = host.thread.take() {
        match thread.join() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                return Err(ExecutorError::ProcessFailed {
                    command: "ryvus-runtime-host".to_string(),
                    exit_code: None,
                    stderr: error,
                });
            }
            Err(_) => return Err(ExecutorError::HttpWorkerPanicked),
        }
    }
    Ok(())
}
