use std::{
    collections::HashMap,
    fs,
    hash::{DefaultHasher, Hash, Hasher},
    io::Read,
    net::TcpListener,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, Condvar, Mutex},
    thread::JoinHandle,
    time::{Duration, Instant},
};

use crate::{ExecutorError, ExecutorResult, LocalProcessTarget, RuntimeTarget};
use ryvus_protocol::{AttemptId, ExecutionAttempt};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RuntimeLifecycle {
    #[default]
    PerInvocation,
    LongLived,
}

impl std::fmt::Display for RuntimeLifecycle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PerInvocation => f.write_str("per_invocation"),
            Self::LongLived => f.write_str("long_lived"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeOutcome {
    Success,
    HandlerFailure,
    InfrastructureFailure,
    TimedOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeDisposition {
    Reusable,
    Stopped,
    Cancelled,
    TimedOut,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeHandle {
    pub runtime_id: String,
    pub attempt: ExecutionAttempt,
    pub endpoint: String,
    pub lifecycle: RuntimeLifecycle,
    managed: bool,
}

impl RuntimeHandle {
    fn managed(
        runtime_id: impl Into<String>,
        attempt: ExecutionAttempt,
        endpoint: impl Into<String>,
        lifecycle: RuntimeLifecycle,
    ) -> Self {
        Self {
            runtime_id: runtime_id.into(),
            attempt,
            endpoint: endpoint.into(),
            lifecycle,
            managed: true,
        }
    }

    pub fn existing(attempt: ExecutionAttempt, endpoint: impl Into<String>) -> Self {
        Self {
            runtime_id: uuid::Uuid::new_v4().to_string(),
            attempt,
            endpoint: endpoint.into(),
            lifecycle: RuntimeLifecycle::LongLived,
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
    fn reusable() -> Self {
        Self {
            exit: RuntimeExit::default(),
            disposition: RuntimeDisposition::Reusable,
        }
    }
}

#[cfg_attr(test, mockall::automock)]
pub trait RuntimeManager: Send + Sync {
    fn acquire(
        &self,
        target: &RuntimeTarget,
        attempt: &ExecutionAttempt,
        lifecycle: RuntimeLifecycle,
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
        lifecycle: RuntimeLifecycle,
        timeout: Duration,
    ) -> ExecutorResult<RuntimeHandle> {
        self.as_ref().acquire(target, attempt, lifecycle, timeout)
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

#[derive(Debug, Clone)]
pub struct LongLivedRuntimePolicy {
    pub max_instances_per_runtime: usize,
    pub idle_timeout: Duration,
    pub shutdown_grace: Duration,
}

impl Default for LongLivedRuntimePolicy {
    fn default() -> Self {
        Self {
            max_instances_per_runtime: 4,
            idle_timeout: Duration::from_secs(60),
            shutdown_grace: Duration::from_secs(3),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RuntimeBaseKey {
    command: String,
    args: Vec<String>,
    working_dir: Option<PathBuf>,
    env: Vec<(String, String)>,
}

impl From<&LocalProcessTarget> for RuntimeBaseKey {
    fn from(target: &LocalProcessTarget) -> Self {
        let mut env = target
            .env
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<Vec<_>>();
        env.sort_unstable();

        Self {
            command: target.command.clone(),
            args: target.args.clone(),
            working_dir: target.working_dir.clone(),
            env,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RuntimeKey {
    base: RuntimeBaseKey,
    revision: u64,
}

enum RuntimeInstanceState {
    Idle { last_used: Instant },
    Busy { attempt_id: AttemptId },
    Cancelled,
}

struct RuntimeInstance {
    key: Option<RuntimeKey>,
    state: RuntimeInstanceState,
    endpoint: String,
    child: Child,
    stdout: JoinHandle<std::io::Result<Vec<u8>>>,
    stderr: JoinHandle<std::io::Result<Vec<u8>>>,
}

#[derive(Default)]
struct RuntimeState {
    instances: HashMap<String, RuntimeInstance>,
    latest_revisions: HashMap<RuntimeBaseKey, u64>,
    completed: HashMap<String, RuntimeDisposition>,
    draining: bool,
}

#[derive(Default)]
struct SharedPool {
    state: Mutex<RuntimeState>,
    changed: Condvar,
}

pub struct LocalRuntimeManager {
    shared: Arc<SharedPool>,
    policy: LongLivedRuntimePolicy,
    reaper: Mutex<Option<JoinHandle<()>>>,
}

impl std::fmt::Debug for LocalRuntimeManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalRuntimeManager")
            .field("policy", &self.policy)
            .finish()
    }
}

impl Default for LocalRuntimeManager {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalRuntimeManager {
    pub fn new() -> Self {
        Self::with_policy(LongLivedRuntimePolicy::default())
    }

    pub fn with_policy(mut policy: LongLivedRuntimePolicy) -> Self {
        policy.max_instances_per_runtime = policy.max_instances_per_runtime.max(1);
        let shared = Arc::new(SharedPool::default());
        let reaper = spawn_reaper(Arc::clone(&shared), policy.idle_timeout);
        Self {
            shared,
            policy,
            reaper: Mutex::new(Some(reaper)),
        }
    }

    fn acquire_long_lived(
        &self,
        target: &LocalProcessTarget,
        attempt: &ExecutionAttempt,
        timeout: Duration,
    ) -> ExecutorResult<RuntimeHandle> {
        let key = runtime_key(target)?;
        let deadline = Instant::now() + timeout;
        let mut state = self.shared.state.lock().expect("runtime state should lock");

        loop {
            if state.draining {
                return Err(ExecutorError::RuntimeUnavailable);
            }

            state
                .latest_revisions
                .insert(key.base.clone(), key.revision);
            reap_stale_idle(&mut state, &key)?;
            reap_dead_idle(&mut state, &key)?;

            if let Some((runtime_id, runtime)) = state.instances.iter_mut().find(|(_, runtime)| {
                runtime.key.as_ref() == Some(&key)
                    && matches!(runtime.state, RuntimeInstanceState::Idle { .. })
            }) {
                if runtime.child.try_wait()?.is_none() {
                    runtime.state = RuntimeInstanceState::Busy {
                        attempt_id: attempt.attempt_id.clone(),
                    };
                    return Ok(RuntimeHandle::managed(
                        runtime_id,
                        attempt.clone(),
                        &runtime.endpoint,
                        RuntimeLifecycle::LongLived,
                    ));
                }
            }

            let count = state
                .instances
                .values()
                .filter(|runtime| runtime.key.as_ref() == Some(&key))
                .count();
            if count < self.policy.max_instances_per_runtime {
                // ponytail: startup holds the pool lock; split per-key startup locks if measured startup contention matters.
                let (runtime_id, runtime, handle) = start_local_runtime(
                    target,
                    attempt,
                    RuntimeLifecycle::LongLived,
                    Some(key.clone()),
                    deadline.saturating_duration_since(Instant::now()),
                )?;
                state.instances.insert(runtime_id, runtime);
                return Ok(handle);
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(ExecutorError::RuntimePoolExhausted {
                    attempt: attempt.clone(),
                });
            }
            let (next, wait) = self
                .shared
                .changed
                .wait_timeout(state, remaining)
                .expect("runtime state should lock");
            state = next;
            if wait.timed_out() {
                return Err(ExecutorError::RuntimePoolExhausted {
                    attempt: attempt.clone(),
                });
            }
        }
    }
}

impl RuntimeManager for LocalRuntimeManager {
    fn acquire(
        &self,
        target: &RuntimeTarget,
        attempt: &ExecutionAttempt,
        lifecycle: RuntimeLifecycle,
        timeout: Duration,
    ) -> ExecutorResult<RuntimeHandle> {
        let RuntimeTarget::LocalProcess(target) = target else {
            if let RuntimeTarget::Http { endpoint } = target {
                return Ok(RuntimeHandle::existing(attempt.clone(), endpoint));
            }
            return Err(ExecutorError::UnsupportedRuntimeTarget {
                target: format!("{target:?}"),
            });
        };

        if lifecycle == RuntimeLifecycle::LongLived {
            return self.acquire_long_lived(target, attempt, timeout);
        }

        let (runtime_id, runtime, handle) = start_local_runtime(
            target,
            attempt,
            RuntimeLifecycle::PerInvocation,
            None,
            timeout,
        )?;
        self.shared
            .state
            .lock()
            .expect("runtime state should lock")
            .instances
            .insert(runtime_id, runtime);
        Ok(handle)
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
            return Ok(RuntimeRelease::reusable());
        }

        let mut state = self.shared.state.lock().expect("runtime state should lock");
        let Some(runtime) = state.instances.get(&handle.runtime_id) else {
            if let Some(disposition) = state.completed.remove(&handle.runtime_id) {
                return Ok(RuntimeRelease {
                    exit: RuntimeExit::default(),
                    disposition,
                });
            }
            return Err(ExecutorError::RuntimeHandleNotFound {
                runtime_id: handle.runtime_id,
            });
        };

        let current_revision = runtime.key.as_ref().and_then(|key| {
            state
                .latest_revisions
                .get(&key.base)
                .map(|revision| *revision == key.revision)
        });
        let runtime = state
            .instances
            .get_mut(&handle.runtime_id)
            .expect("released runtime should exist");

        if matches!(runtime.state, RuntimeInstanceState::Cancelled) {
            let runtime = state
                .instances
                .remove(&handle.runtime_id)
                .expect("cancelled runtime should exist");
            drop(state);
            let exit = finish_runtime(runtime, false)?;
            self.shared.changed.notify_all();
            return Ok(RuntimeRelease {
                exit,
                disposition: RuntimeDisposition::Cancelled,
            });
        }

        if outcome == RuntimeOutcome::TimedOut {
            let runtime = state
                .instances
                .remove(&handle.runtime_id)
                .expect("timed out runtime should exist");
            drop(state);
            let exit = finish_runtime(runtime, true)?;
            self.shared.changed.notify_all();
            return Ok(RuntimeRelease {
                exit,
                disposition: RuntimeDisposition::TimedOut,
            });
        }

        let reusable_outcome = matches!(
            outcome,
            RuntimeOutcome::Success | RuntimeOutcome::HandlerFailure
        );
        // ponytail: release health checks hold the pool lock; use per-instance locks if measured contention matters.
        let reusable = handle.lifecycle == RuntimeLifecycle::LongLived
            && reusable_outcome
            && current_revision.unwrap_or(false)
            && runtime.child.try_wait()?.is_none()
            && runtime_is_idle(&runtime.endpoint);

        if reusable {
            runtime.state = RuntimeInstanceState::Idle {
                last_used: Instant::now(),
            };
            self.shared.changed.notify_all();
            return Ok(RuntimeRelease::reusable());
        }

        let runtime = state
            .instances
            .remove(&handle.runtime_id)
            .expect("released runtime should exist");
        drop(state);
        let exit = finish_runtime(runtime, true)?;
        self.shared.changed.notify_all();
        Ok(RuntimeRelease {
            exit,
            disposition: RuntimeDisposition::Stopped,
        })
    }

    fn cancel(&self, attempt_id: &AttemptId) -> ExecutorResult<bool> {
        let mut state = self.shared.state.lock().expect("runtime state should lock");
        let Some(runtime) = state.instances.values_mut().find(|runtime| {
            matches!(
                &runtime.state,
                RuntimeInstanceState::Busy {
                    attempt_id: active
                } if active == attempt_id
            )
        }) else {
            return Ok(false);
        };

        runtime.state = RuntimeInstanceState::Cancelled;
        if runtime.child.try_wait()?.is_none() {
            runtime.child.kill()?;
            runtime.child.wait()?;
        }
        self.shared.changed.notify_all();
        Ok(true)
    }

    fn shutdown(&self, grace: Duration) -> ExecutorResult<()> {
        let deadline = Instant::now() + grace;
        let mut state = self.shared.state.lock().expect("runtime state should lock");
        state.draining = true;
        self.shared.changed.notify_all();

        while state
            .instances
            .values()
            .any(|runtime| matches!(runtime.state, RuntimeInstanceState::Busy { .. }))
        {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let (next, wait) = self
                .shared
                .changed
                .wait_timeout(state, remaining)
                .expect("runtime state should lock");
            state = next;
            if wait.timed_out() {
                break;
            }
        }

        let runtimes = state.instances.drain().collect::<Vec<_>>();
        for (runtime_id, runtime) in &runtimes {
            if matches!(runtime.state, RuntimeInstanceState::Busy { .. }) {
                state
                    .completed
                    .insert(runtime_id.clone(), RuntimeDisposition::Cancelled);
            }
        }
        drop(state);

        let mut cleanup_error = None;
        for (_, runtime) in runtimes {
            if let Err(error) = finish_runtime(runtime, true) {
                cleanup_error.get_or_insert(error);
            }
        }

        if let Some(reaper) = self.reaper.lock().expect("reaper should lock").take() {
            self.shared.changed.notify_all();
            let _ = reaper.join();
        }
        match cleanup_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl Drop for LocalRuntimeManager {
    fn drop(&mut self) {
        let _ = self.shutdown(Duration::ZERO);
    }
}

fn runtime_key(target: &LocalProcessTarget) -> ExecutorResult<RuntimeKey> {
    let revision = match &target.source {
        Some(source) => {
            let mut hasher = DefaultHasher::new();
            fs::read(source)?.hash(&mut hasher);
            hasher.finish()
        }
        None => 0,
    };
    Ok(RuntimeKey {
        base: RuntimeBaseKey::from(target),
        revision,
    })
}

fn reap_stale_idle(state: &mut RuntimeState, key: &RuntimeKey) -> ExecutorResult<()> {
    let stale_ids = state
        .instances
        .iter()
        .filter_map(|(runtime_id, runtime)| {
            let stale = runtime.key.as_ref().is_some_and(|runtime_key| {
                runtime_key.base == key.base && runtime_key.revision != key.revision
            });
            (stale && matches!(runtime.state, RuntimeInstanceState::Idle { .. }))
                .then(|| runtime_id.clone())
        })
        .collect::<Vec<_>>();

    for runtime_id in stale_ids {
        if let Some(runtime) = state.instances.remove(&runtime_id) {
            finish_runtime(runtime, true)?;
        }
    }
    Ok(())
}

fn reap_dead_idle(state: &mut RuntimeState, key: &RuntimeKey) -> ExecutorResult<()> {
    let mut dead_ids = Vec::new();
    for (runtime_id, runtime) in &mut state.instances {
        if runtime.key.as_ref() == Some(key)
            && matches!(runtime.state, RuntimeInstanceState::Idle { .. })
            && runtime.child.try_wait()?.is_some()
        {
            dead_ids.push(runtime_id.clone());
        }
    }

    for runtime_id in dead_ids {
        if let Some(runtime) = state.instances.remove(&runtime_id) {
            finish_runtime(runtime, false)?;
        }
    }
    Ok(())
}

fn spawn_reaper(shared: Arc<SharedPool>, idle_timeout: Duration) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let interval = idle_timeout
            .min(Duration::from_secs(1))
            .max(Duration::from_millis(10));
        loop {
            let mut state = shared.state.lock().expect("runtime state should lock");
            let (next, _) = shared
                .changed
                .wait_timeout(state, interval)
                .expect("runtime state should lock");
            state = next;
            if state.draining {
                return;
            }

            let now = Instant::now();
            let expired = state
                .instances
                .iter()
                .filter_map(|(runtime_id, runtime)| match runtime.state {
                    RuntimeInstanceState::Idle { last_used }
                        if now.duration_since(last_used) >= idle_timeout =>
                    {
                        Some(runtime_id.clone())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            let runtimes = expired
                .into_iter()
                .filter_map(|runtime_id| state.instances.remove(&runtime_id))
                .collect::<Vec<_>>();
            drop(state);
            for runtime in runtimes {
                let _ = finish_runtime(runtime, true);
            }
            shared.changed.notify_all();
        }
    })
}

fn start_local_runtime(
    target: &LocalProcessTarget,
    attempt: &ExecutionAttempt,
    lifecycle: RuntimeLifecycle,
    key: Option<RuntimeKey>,
    timeout: Duration,
) -> ExecutorResult<(String, RuntimeInstance, RuntimeHandle)> {
    // ponytail: local dynamic-port reservation has a bind race; use socket activation if contention becomes measurable.
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    drop(listener);

    let endpoint = format!("http://127.0.0.1:{port}");
    let mut command = Command::new(&target.command);
    command
        .args(&target.args)
        .envs(&target.env)
        .env("RYVUS_RUNTIME_HOST", "127.0.0.1")
        .env("RYVUS_RUNTIME_PORT", port.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(working_dir) = &target.working_dir {
        command.current_dir(working_dir);
    }

    let mut child = command
        .spawn()
        .map_err(|io_error| ExecutorError::ProcessStartFailed {
            attempt: attempt.clone(),
            command: target.command.clone(),
            io_error,
        })?;
    let stdout = read_output(child.stdout.take().expect("stdout should be piped"));
    let stderr = read_output(child.stderr.take().expect("stderr should be piped"));
    let started = Instant::now();

    loop {
        if let Some(status) = child.try_wait()? {
            let (stdout, stderr) = join_output(stdout, stderr)?;
            return Err(ExecutorError::RuntimeStartupFailed {
                attempt: attempt.clone(),
                command: target.command.clone(),
                exit_code: status.code(),
                stdout,
                stderr,
            });
        }

        let elapsed = started.elapsed();
        if elapsed >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            let (stdout, stderr) = join_output(stdout, stderr)?;
            return Err(ExecutorError::RuntimeReadinessTimedOut {
                attempt: attempt.clone(),
                endpoint,
                timeout_ms: timeout.as_millis(),
                stdout,
                stderr,
            });
        }

        if runtime_is_ready(
            &endpoint,
            (timeout - elapsed).min(Duration::from_millis(100)),
        ) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let runtime_id = uuid::Uuid::new_v4().to_string();
    let handle = RuntimeHandle::managed(&runtime_id, attempt.clone(), &endpoint, lifecycle);
    Ok((
        runtime_id,
        RuntimeInstance {
            key,
            state: RuntimeInstanceState::Busy {
                attempt_id: attempt.attempt_id.clone(),
            },
            endpoint,
            child,
            stdout,
            stderr,
        },
        handle,
    ))
}

fn runtime_is_idle(endpoint: &str) -> bool {
    runtime_health(endpoint, Duration::from_millis(100)).is_some_and(|health| {
        !health
            .get("busy")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
    })
}

fn runtime_is_ready(endpoint: &str, timeout: Duration) -> bool {
    runtime_health(endpoint, timeout).is_some()
}

fn runtime_health(endpoint: &str, timeout: Duration) -> Option<serde_json::Value> {
    let health_url = format!("{endpoint}/health");
    std::thread::spawn(move || {
        let response = reqwest::blocking::Client::builder()
            .timeout(timeout)
            .build()
            .ok()?
            .get(health_url)
            .send()
            .ok()?;
        response
            .status()
            .is_success()
            .then(|| response.json().unwrap_or_default())
    })
    .join()
    .ok()
    .flatten()
}

fn finish_runtime(mut runtime: RuntimeInstance, terminate: bool) -> ExecutorResult<RuntimeExit> {
    if terminate && runtime.child.try_wait()?.is_none() {
        runtime.child.kill()?;
    }
    let status = runtime.child.wait()?;
    let (stdout, stderr) = join_output(runtime.stdout, runtime.stderr)?;
    Ok(RuntimeExit {
        stdout,
        stderr,
        exit_code: status.code(),
    })
}

fn read_output(mut output: impl Read + Send + 'static) -> JoinHandle<std::io::Result<Vec<u8>>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        output.read_to_end(&mut bytes)?;
        Ok(bytes)
    })
}

fn join_output(
    stdout: JoinHandle<std::io::Result<Vec<u8>>>,
    stderr: JoinHandle<std::io::Result<Vec<u8>>>,
) -> ExecutorResult<(String, String)> {
    let stdout = stdout.join().expect("stdout reader should not panic")?;
    let stderr = stderr.join().expect("stderr reader should not panic")?;
    Ok((
        String::from_utf8_lossy(&stdout).to_string(),
        String::from_utf8_lossy(&stderr).to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ryvus_protocol::ExecutionId;

    #[test]
    fn defaults_to_per_invocation() {
        assert_eq!(RuntimeLifecycle::default(), RuntimeLifecycle::PerInvocation);
    }

    #[test]
    fn healthy_long_lived_runtime_is_reused_only_after_release() {
        let manager = LocalRuntimeManager::new();
        let target = health_runtime_target();
        let first = acquire(&manager, &target, "inv_1");
        let second = acquire(&manager, &target, "inv_2");
        assert_ne!(first.runtime_id, second.runtime_id);
        let runtime_ids = [first.runtime_id.clone(), second.runtime_id.clone()];
        manager
            .release(first.clone(), RuntimeOutcome::HandlerFailure)
            .unwrap();
        manager.release(second, RuntimeOutcome::Success).unwrap();

        let third = acquire(&manager, &target, "inv_3");
        assert!(runtime_ids.contains(&third.runtime_id));
        manager.release(third, RuntimeOutcome::Success).unwrap();
    }

    #[test]
    fn cancellation_terminates_only_the_assigned_runtime_and_is_not_reused() {
        let manager = LocalRuntimeManager::new();
        let target = health_runtime_target();
        let first = acquire(&manager, &target, "inv_1");
        let second = acquire(&manager, &target, "inv_2");

        assert!(manager.cancel(&first.attempt.attempt_id).unwrap());
        assert!(!manager.cancel(&first.attempt.attempt_id).unwrap());
        let release = manager
            .release(first.clone(), RuntimeOutcome::InfrastructureFailure)
            .unwrap();
        assert_eq!(release.disposition, RuntimeDisposition::Cancelled);
        assert!(runtime_is_idle(&second.endpoint));
        manager.release(second, RuntimeOutcome::Success).unwrap();

        let replacement = acquire(&manager, &target, "inv_3");
        assert_ne!(replacement.runtime_id, first.runtime_id);
        manager
            .release(replacement, RuntimeOutcome::Success)
            .unwrap();
    }

    #[test]
    fn stale_attempt_id_cannot_cancel_reassigned_runtime() {
        let manager = LocalRuntimeManager::new();
        let target = health_runtime_target();
        let first = acquire(&manager, &target, "attempt_1");
        manager
            .release(first.clone(), RuntimeOutcome::Success)
            .unwrap();

        let second = acquire(&manager, &target, "attempt_2");
        assert_eq!(second.runtime_id, first.runtime_id);
        assert!(!manager.cancel(&first.attempt.attempt_id).unwrap());
        assert!(runtime_is_idle(&second.endpoint));
        manager.release(second, RuntimeOutcome::Success).unwrap();
    }

    #[test]
    fn timeout_invalidates_runtime() {
        let manager = LocalRuntimeManager::new();
        let target = health_runtime_target();
        let first = acquire(&manager, &target, "inv_1");
        let release = manager
            .release(first.clone(), RuntimeOutcome::TimedOut)
            .unwrap();
        assert_eq!(release.disposition, RuntimeDisposition::TimedOut);

        let second = acquire(&manager, &target, "inv_2");
        assert_ne!(second.runtime_id, first.runtime_id);
        manager.release(second, RuntimeOutcome::Success).unwrap();
    }

    #[test]
    fn capacity_wait_is_bounded() {
        let manager = LocalRuntimeManager::with_policy(LongLivedRuntimePolicy {
            max_instances_per_runtime: 1,
            idle_timeout: Duration::from_secs(1),
            shutdown_grace: Duration::ZERO,
        });
        let target = health_runtime_target();
        let first = acquire(&manager, &target, "inv_1");
        let error = manager
            .acquire(
                &target,
                &test_attempt("inv_2"),
                RuntimeLifecycle::LongLived,
                Duration::from_millis(25),
            )
            .unwrap_err();
        assert!(matches!(error, ExecutorError::RuntimePoolExhausted { .. }));
        manager.release(first, RuntimeOutcome::Success).unwrap();
    }

    #[test]
    fn source_change_invalidates_idle_runtime() {
        let source = std::env::temp_dir().join(format!("ryvus-source-{}", uuid::Uuid::new_v4()));
        fs::write(&source, "one").unwrap();
        let target = health_runtime_target().source(&source);
        let manager = LocalRuntimeManager::new();
        let first = acquire(&manager, &target, "inv_1");
        manager
            .release(first.clone(), RuntimeOutcome::Success)
            .unwrap();

        fs::write(&source, "two").unwrap();
        let second = acquire(&manager, &target, "inv_2");
        assert_ne!(second.runtime_id, first.runtime_id);
        manager.release(second, RuntimeOutcome::Success).unwrap();
        let _ = fs::remove_file(source);
    }

    #[test]
    fn crashed_idle_runtime_is_replaced() {
        let manager = LocalRuntimeManager::new();
        let target = health_runtime_target();
        let first = acquire(&manager, &target, "inv_1");
        manager
            .release(first.clone(), RuntimeOutcome::Success)
            .unwrap();
        let _ = reqwest::blocking::get(format!("{}/die", first.endpoint));

        let second = acquire(&manager, &target, "inv_2");
        assert_ne!(second.runtime_id, first.runtime_id);
        manager.release(second, RuntimeOutcome::Success).unwrap();
    }

    #[test]
    fn idle_timeout_reaps_runtime() {
        let manager = LocalRuntimeManager::with_policy(LongLivedRuntimePolicy {
            max_instances_per_runtime: 2,
            idle_timeout: Duration::from_millis(20),
            shutdown_grace: Duration::ZERO,
        });
        let target = health_runtime_target();
        let first = acquire(&manager, &target, "inv_1");
        manager
            .release(first.clone(), RuntimeOutcome::Success)
            .unwrap();
        std::thread::sleep(Duration::from_millis(60));

        let second = acquire(&manager, &target, "inv_2");
        assert_ne!(second.runtime_id, first.runtime_id);
        manager.release(second, RuntimeOutcome::Success).unwrap();
    }

    #[test]
    fn shutdown_terminates_idle_and_busy_runtimes() {
        let pid_dir = std::env::temp_dir().join(format!("ryvus-pids-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&pid_dir).unwrap();
        let manager = LocalRuntimeManager::new();
        let target = health_runtime_target().env("RYVUS_PID_DIR", pid_dir.to_string_lossy());
        let idle = acquire(&manager, &target, "inv_1");
        let busy = acquire(&manager, &target, "inv_2");
        manager.release(idle, RuntimeOutcome::Success).unwrap();

        manager.shutdown(Duration::ZERO).unwrap();

        let release = manager
            .release(busy, RuntimeOutcome::InfrastructureFailure)
            .unwrap();
        assert_eq!(release.disposition, RuntimeDisposition::Cancelled);
        let entries = fs::read_dir(&pid_dir)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(entries.len(), 2);
        for entry in entries {
            let pid = fs::read_to_string(entry.path()).unwrap();
            let status = Command::new("kill")
                .args(["-0", pid.trim()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap();
            assert!(!status.success());
        }
        let _ = fs::remove_dir_all(pid_dir);
    }

    #[test]
    fn per_invocation_still_starts_a_new_runtime() {
        let manager = LocalRuntimeManager::new();
        let target = health_runtime_target();
        let first = manager
            .acquire(
                &target,
                &test_attempt("inv_1"),
                RuntimeLifecycle::PerInvocation,
                Duration::from_secs(2),
            )
            .unwrap();
        let first_id = first.runtime_id.clone();
        manager.release(first, RuntimeOutcome::Success).unwrap();
        let second = manager
            .acquire(
                &target,
                &test_attempt("inv_2"),
                RuntimeLifecycle::PerInvocation,
                Duration::from_secs(2),
            )
            .unwrap();
        assert_ne!(second.runtime_id, first_id);
        manager.release(second, RuntimeOutcome::Success).unwrap();
    }

    #[test]
    fn startup_failure_includes_process_output() {
        let error = LocalRuntimeManager::new()
            .acquire(
                &RuntimeTarget::local_process("sh")
                    .args(["-c", "echo startup-out; echo startup-err >&2; exit 7"]),
                &test_attempt("inv_1"),
                RuntimeLifecycle::PerInvocation,
                Duration::from_secs(1),
            )
            .unwrap_err();
        assert!(error.to_string().contains("startup-out"));
        assert!(error.to_string().contains("startup-err"));
    }

    fn acquire(
        manager: &LocalRuntimeManager,
        target: &RuntimeTarget,
        attempt_id: &str,
    ) -> RuntimeHandle {
        manager
            .acquire(
                target,
                &test_attempt(attempt_id),
                RuntimeLifecycle::LongLived,
                Duration::from_secs(2),
            )
            .unwrap()
    }

    fn test_attempt(attempt_id: &str) -> ExecutionAttempt {
        ExecutionAttempt {
            execution_id: ExecutionId::from("exec_1"),
            attempt_id: AttemptId::from(attempt_id),
            attempt_number: 1,
        }
    }

    fn health_runtime_target() -> RuntimeTarget {
        RuntimeTarget::local_process("python3").args([
            "-c",
            r#"
import json
import os
from http.server import BaseHTTPRequestHandler, HTTPServer

if pid_dir := os.environ.get("RYVUS_PID_DIR"):
    with open(os.path.join(pid_dir, str(os.getpid())), "w") as file:
        file.write(str(os.getpid()))

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/die":
            os._exit(9)
        payload = json.dumps({"status": "ready", "busy": False}).encode()
        self.send_response(200 if self.path == "/health" else 404)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, format, *args):
        pass

HTTPServer(
    (os.environ["RYVUS_RUNTIME_HOST"], int(os.environ["RYVUS_RUNTIME_PORT"])),
    Handler,
).serve_forever()
"#,
        ])
    }
}
