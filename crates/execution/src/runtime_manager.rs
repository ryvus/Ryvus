use std::{
    collections::HashMap,
    net::TcpListener,
    sync::{Arc, Mutex},
    thread::JoinHandle,
    time::{Duration, Instant},
};

use ryvus_protocol::{AttemptId, ExecutionAttempt};
use ryvus_runtime_host::{ProcessInvocationWorkerFactory, ProcessWorkerConfig, RuntimeHost};
use tokio::sync::oneshot;

use crate::{ExecutorError, ExecutorResult, LocalProcessTarget, RuntimeTarget};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeRelease {
    pub exit: RuntimeExit,
    pub disposition: RuntimeDisposition,
}

impl RuntimeRelease {
    fn external() -> Self {
        Self {
            exit: RuntimeExit::default(),
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
    ) -> ExecutorResult<RuntimeHandle>;

    fn release(
        &self,
        handle: RuntimeHandle,
        outcome: RuntimeOutcome,
    ) -> ExecutorResult<RuntimeRelease>;

    fn cancel(&self, attempt_id: &AttemptId) -> ExecutorResult<bool>;

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
    ) -> ExecutorResult<RuntimeHandle> {
        self.as_ref().acquire(target, attempt, timeout)
    }

    fn release(
        &self,
        handle: RuntimeHandle,
        outcome: RuntimeOutcome,
    ) -> ExecutorResult<RuntimeRelease> {
        self.as_ref().release(handle, outcome)
    }

    fn cancel(&self, attempt_id: &AttemptId) -> ExecutorResult<bool> {
        self.as_ref().cancel(attempt_id)
    }

    fn shutdown(&self, grace: Duration) -> ExecutorResult<()> {
        self.as_ref().shutdown(grace)
    }
}

struct ManagedHost {
    attempt: ExecutionAttempt,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<Result<(), String>>>,
}

#[derive(Default)]
struct RuntimeState {
    hosts: HashMap<String, ManagedHost>,
    completed: HashMap<String, RuntimeDisposition>,
    draining: bool,
}

#[derive(Default)]
pub struct LocalRuntimeManager {
    state: Mutex<RuntimeState>,
}

impl std::fmt::Debug for LocalRuntimeManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalRuntimeManager")
            .finish_non_exhaustive()
    }
}

impl LocalRuntimeManager {
    pub fn new() -> Self {
        Self::default()
    }
}

impl RuntimeManager for LocalRuntimeManager {
    fn acquire(
        &self,
        target: &RuntimeTarget,
        attempt: &ExecutionAttempt,
        timeout: Duration,
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
        let (endpoint, host) = start_runtime_host(target, attempt, timeout)?;
        let mut state = self.state.lock().expect("runtime state should lock");
        if state.draining {
            drop(state);
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
                    if let Some(disposition) = state.completed.remove(&handle.runtime_id) {
                        return Ok(RuntimeRelease {
                            exit: RuntimeExit::default(),
                            disposition,
                        });
                    }
                    return Err(ExecutorError::RuntimeHandleNotFound {
                        runtime_id: handle.runtime_id,
                    });
                }
            }
        };

        stop_runtime_host(host)?;
        Ok(RuntimeRelease {
            exit: RuntimeExit::default(),
            disposition: if outcome == RuntimeOutcome::TimedOut {
                RuntimeDisposition::TimedOut
            } else {
                RuntimeDisposition::Stopped
            },
        })
    }

    fn cancel(&self, attempt_id: &AttemptId) -> ExecutorResult<bool> {
        let selected = {
            let mut state = self.state.lock().expect("runtime state should lock");
            let runtime_id = state.hosts.iter().find_map(|(runtime_id, host)| {
                (&host.attempt.attempt_id == attempt_id).then(|| runtime_id.clone())
            });
            runtime_id.and_then(|runtime_id| {
                state
                    .hosts
                    .remove(&runtime_id)
                    .map(|host| (runtime_id, host))
            })
        };

        let Some((runtime_id, host)) = selected else {
            return Ok(false);
        };
        stop_runtime_host(host)?;
        self.state
            .lock()
            .expect("runtime state should lock")
            .completed
            .insert(runtime_id, RuntimeDisposition::Cancelled);
        Ok(true)
    }

    fn shutdown(&self, _grace: Duration) -> ExecutorResult<()> {
        let hosts = {
            let mut state = self.state.lock().expect("runtime state should lock");
            state.draining = true;
            state.hosts.drain().collect::<Vec<_>>()
        };
        for (runtime_id, host) in hosts {
            stop_runtime_host(host)?;
            self.state
                .lock()
                .expect("runtime state should lock")
                .completed
                .insert(runtime_id, RuntimeDisposition::Cancelled);
        }
        Ok(())
    }
}

fn start_runtime_host(
    target: &LocalProcessTarget,
    attempt: &ExecutionAttempt,
    timeout: Duration,
) -> ExecutorResult<(String, ManagedHost)> {
    let config = ProcessWorkerConfig {
        command: target.command.clone(),
        args: target.args.clone(),
        working_dir: target.working_dir.clone(),
        env: target.env.clone(),
    };
    let host = RuntimeHost::new(Arc::new(ProcessInvocationWorkerFactory::new(config)));
    let router = host.router();
    let shutdown_host = host.clone();
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
                axum::serve(listener, router)
                    .with_graceful_shutdown(async move {
                        let _ = shutdown_rx.await;
                        if let Err(error) = shutdown_host.shutdown().await {
                            tracing::error!(%error, "runtime host shutdown failed");
                        }
                    })
                    .await
                    .map_err(|error| error.to_string())
            })
        })?;
    let managed = ManagedHost {
        attempt: attempt.clone(),
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
