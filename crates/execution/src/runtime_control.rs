use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use ryvus_protocol::{
    AttemptId, AttemptOutcome, ControlCommandOutcome, ControlMessageId, ExecutionAttempt,
    ExecutionId, RuntimeControlCommand, RuntimeControlEvent, RuntimeHostId, RuntimeRegistration,
    RuntimeSessionId, TerminationReason, WorkerId, RUNTIME_CONTROL_PROTOCOL_VERSION,
};
use ryvus_runtime_host::RuntimeHostControlSender;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptOwnership {
    pub execution_id: ExecutionId,
    pub attempt_id: AttemptId,
    pub attempt_number: u32,
    pub runtime_host_id: RuntimeHostId,
    pub runtime_session_id: RuntimeSessionId,
    pub worker_id: WorkerId,
}

#[derive(Debug, Error)]
pub enum RuntimeControlError {
    #[error("runtime control channel failed: {0}")]
    Channel(String),
    #[error("runtime control returned an invalid command result")]
    InvalidCommandResult,
    #[error("invalid runtime-control message: {0}")]
    InvalidMessage(String),
    #[error("runtime session '{runtime_session_id}' is stale for host '{runtime_host_id}'")]
    StaleSession {
        runtime_host_id: RuntimeHostId,
        runtime_session_id: RuntimeSessionId,
    },
    #[error("runtime-control event does not match active attempt ownership")]
    OwnershipMismatch,
}

pub type RuntimeControlResult<T> = Result<T, RuntimeControlError>;

pub trait RuntimeControlChannel: Send + Sync {
    fn send(&self, command: RuntimeControlCommand) -> RuntimeControlResult<RuntimeControlEvent>;
}

#[derive(Default)]
pub struct InMemoryRuntimeControlChannel {
    hosts: Mutex<HashMap<RuntimeHostId, RuntimeHostControlSender>>,
}

impl InMemoryRuntimeControlChannel {
    pub fn register(&self, runtime_host_id: RuntimeHostId, sender: RuntimeHostControlSender) {
        self.hosts
            .lock()
            .expect("runtime control hosts should lock")
            .insert(runtime_host_id, sender);
    }

    pub fn unregister(&self, runtime_host_id: &RuntimeHostId) {
        self.hosts
            .lock()
            .expect("runtime control hosts should lock")
            .remove(runtime_host_id);
    }
}

impl RuntimeControlChannel for InMemoryRuntimeControlChannel {
    fn send(&self, command: RuntimeControlCommand) -> RuntimeControlResult<RuntimeControlEvent> {
        let runtime_host_id = command_host_id(&command);
        let sender = self
            .hosts
            .lock()
            .expect("runtime control hosts should lock")
            .get(runtime_host_id)
            .cloned()
            .ok_or_else(|| {
                RuntimeControlError::Channel(format!(
                    "runtime host '{runtime_host_id}' is not registered"
                ))
            })?;
        sender.send(command).map_err(RuntimeControlError::Channel)
    }
}

#[derive(Default)]
struct ControlState {
    attempts: HashMap<AttemptId, AttemptOwnership>,
    runtimes: HashMap<RuntimeHostId, RuntimeSessionId>,
    terminal: HashMap<ExecutionId, AttemptOutcome>,
}

#[derive(Clone)]
pub struct RuntimeControlService {
    channel: Arc<dyn RuntimeControlChannel>,
    state: Arc<Mutex<ControlState>>,
}

#[derive(Clone)]
pub struct RuntimeControlIngress {
    state: Arc<Mutex<ControlState>>,
}

impl RuntimeControlService {
    pub fn new(channel: Arc<dyn RuntimeControlChannel>) -> Self {
        Self {
            channel,
            state: Arc::new(Mutex::new(ControlState::default())),
        }
    }

    pub fn ingress(&self) -> RuntimeControlIngress {
        RuntimeControlIngress {
            state: Arc::clone(&self.state),
        }
    }

    pub fn current_session(&self, runtime_host_id: &RuntimeHostId) -> Option<RuntimeSessionId> {
        self.state
            .lock()
            .expect("runtime control state should lock")
            .runtimes
            .get(runtime_host_id)
            .cloned()
    }

    pub fn attempt_ownership(&self, attempt_id: &AttemptId) -> Option<AttemptOwnership> {
        self.state
            .lock()
            .expect("runtime control state should lock")
            .attempts
            .get(attempt_id)
            .cloned()
    }

    pub fn register_attempt(&self, ownership: AttemptOwnership) {
        let mut state = self
            .state
            .lock()
            .expect("runtime control state should lock");
        state.runtimes.insert(
            ownership.runtime_host_id.clone(),
            ownership.runtime_session_id.clone(),
        );
        state
            .attempts
            .insert(ownership.attempt_id.clone(), ownership);
    }

    pub fn unregister_attempt(&self, attempt_id: &AttemptId) {
        self.state
            .lock()
            .expect("runtime control state should lock")
            .attempts
            .remove(attempt_id);
    }

    pub fn unregister_runtime(&self, runtime_host_id: &RuntimeHostId) {
        self.state
            .lock()
            .expect("runtime control state should lock")
            .runtimes
            .remove(runtime_host_id);
    }

    pub fn cancel(
        &self,
        execution_id: &ExecutionId,
    ) -> RuntimeControlResult<ControlCommandOutcome> {
        let ownership = {
            let state = self
                .state
                .lock()
                .expect("runtime control state should lock");
            if state.terminal.contains_key(execution_id) {
                return Ok(ControlCommandOutcome::AlreadyTerminal);
            }
            state
                .attempts
                .values()
                .find(|ownership| &ownership.execution_id == execution_id)
                .cloned()
        };
        let Some(ownership) = ownership else {
            return Ok(ControlCommandOutcome::AttemptNotFound);
        };
        let event = self.channel.send(RuntimeControlCommand::TerminateAttempt {
            protocol_version: RUNTIME_CONTROL_PROTOCOL_VERSION.to_string(),
            message_id: ControlMessageId::new(),
            runtime_host_id: ownership.runtime_host_id,
            runtime_session_id: ownership.runtime_session_id,
            execution_id: ownership.execution_id.clone(),
            attempt_id: ownership.attempt_id.clone(),
            attempt_number: ownership.attempt_number,
            reason: TerminationReason::Cancellation,
        })?;
        let mut outcome = command_outcome(&event)?;
        if outcome == ControlCommandOutcome::Confirmed {
            let mut state = self
                .state
                .lock()
                .expect("runtime control state should lock");
            let winner = *state
                .terminal
                .entry(ownership.execution_id)
                .or_insert(AttemptOutcome::Cancelled);
            if winner != AttemptOutcome::Cancelled {
                outcome = ControlCommandOutcome::AlreadyTerminal;
            }
            state.attempts.remove(&ownership.attempt_id);
        }
        Ok(outcome)
    }

    pub fn finish_attempt(
        &self,
        attempt: &ExecutionAttempt,
        outcome: AttemptOutcome,
    ) -> AttemptOutcome {
        let mut state = self
            .state
            .lock()
            .expect("runtime control state should lock");
        let winner = *state
            .terminal
            .entry(attempt.execution_id.clone())
            .or_insert(outcome);
        state.attempts.remove(&attempt.attempt_id);
        winner
    }

    pub fn terminal_outcome(&self, execution_id: &ExecutionId) -> Option<AttemptOutcome> {
        self.state
            .lock()
            .expect("runtime control state should lock")
            .terminal
            .get(execution_id)
            .copied()
    }

    pub fn drain(&self) -> RuntimeControlResult<()> {
        for (runtime_host_id, runtime_session_id) in self.runtimes() {
            let event = self.channel.send(RuntimeControlCommand::DrainRuntime {
                protocol_version: RUNTIME_CONTROL_PROTOCOL_VERSION.to_string(),
                message_id: ControlMessageId::new(),
                runtime_host_id,
                runtime_session_id,
            })?;
            command_outcome(&event)?;
        }
        Ok(())
    }

    pub fn shutdown(&self, grace: Duration) -> RuntimeControlResult<()> {
        self.drain()?;
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if self
                .state
                .lock()
                .expect("runtime control state should lock")
                .attempts
                .is_empty()
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(10).min(grace));
        }
        for (runtime_host_id, runtime_session_id) in self.runtimes() {
            let completed_host_id = runtime_host_id.clone();
            let event = self.channel.send(RuntimeControlCommand::ShutdownRuntime {
                protocol_version: RUNTIME_CONTROL_PROTOCOL_VERSION.to_string(),
                message_id: ControlMessageId::new(),
                runtime_host_id,
                runtime_session_id,
            })?;
            let outcome = command_outcome(&event)?;
            if matches!(
                outcome,
                ControlCommandOutcome::Confirmed | ControlCommandOutcome::AlreadyTerminal
            ) {
                let mut state = self
                    .state
                    .lock()
                    .expect("runtime control state should lock");
                let terminated = state
                    .attempts
                    .values()
                    .filter(|ownership| ownership.runtime_host_id == completed_host_id)
                    .map(|ownership| (ownership.attempt_id.clone(), ownership.execution_id.clone()))
                    .collect::<Vec<_>>();
                for (attempt_id, execution_id) in terminated {
                    state.attempts.remove(&attempt_id);
                    state
                        .terminal
                        .entry(execution_id)
                        .or_insert(AttemptOutcome::Cancelled);
                }
                state.runtimes.remove(&completed_host_id);
            }
        }
        Ok(())
    }

    fn runtimes(&self) -> Vec<(RuntimeHostId, RuntimeSessionId)> {
        self.state
            .lock()
            .expect("runtime control state should lock")
            .runtimes
            .iter()
            .map(|(host, session)| (host.clone(), session.clone()))
            .collect()
    }
}

impl RuntimeControlIngress {
    pub fn register(&self, registration: RuntimeRegistration) -> RuntimeControlResult<()> {
        registration
            .validate()
            .map_err(|error| RuntimeControlError::InvalidMessage(error.to_string()))?;
        let mut state = self
            .state
            .lock()
            .expect("runtime control state should lock");
        state
            .attempts
            .retain(|_, ownership| ownership.runtime_host_id != registration.runtime_host_id);
        state.runtimes.insert(
            registration.runtime_host_id.clone(),
            registration.runtime_session_id.clone(),
        );
        for active in registration.active_attempts {
            let ownership = AttemptOwnership {
                execution_id: active.execution_id,
                attempt_id: active.attempt_id,
                attempt_number: active.attempt_number,
                runtime_host_id: registration.runtime_host_id.clone(),
                runtime_session_id: registration.runtime_session_id.clone(),
                worker_id: active.worker_id,
            };
            state
                .attempts
                .insert(ownership.attempt_id.clone(), ownership);
        }
        Ok(())
    }

    pub fn apply(&self, event: RuntimeControlEvent) -> RuntimeControlResult<()> {
        event
            .validate()
            .map_err(|error| RuntimeControlError::InvalidMessage(error.to_string()))?;
        let (runtime_host_id, runtime_session_id) = event_identity(&event);
        let runtime_host_id = runtime_host_id.clone();
        let runtime_session_id = runtime_session_id.clone();
        let mut state = self
            .state
            .lock()
            .expect("runtime control state should lock");
        if state.runtimes.get(&runtime_host_id) != Some(&runtime_session_id) {
            return Err(RuntimeControlError::StaleSession {
                runtime_host_id,
                runtime_session_id,
            });
        }
        match event {
            RuntimeControlEvent::AttemptStarted {
                execution_id,
                attempt_id,
                attempt_number,
                worker_id,
                ..
            } => {
                if state.attempts.get(&attempt_id).is_some_and(|current| {
                    current.execution_id != execution_id
                        || current.attempt_number != attempt_number
                        || current.worker_id != worker_id
                }) || state.attempts.values().any(|current| {
                    current.attempt_id != attempt_id
                        && current.runtime_host_id == runtime_host_id
                        && current.runtime_session_id == runtime_session_id
                        && current.worker_id == worker_id
                }) {
                    return Err(RuntimeControlError::OwnershipMismatch);
                }
                state.attempts.insert(
                    attempt_id.clone(),
                    AttemptOwnership {
                        execution_id,
                        attempt_id,
                        attempt_number,
                        runtime_host_id: runtime_host_id.clone(),
                        runtime_session_id: runtime_session_id.clone(),
                        worker_id,
                    },
                );
            }
            RuntimeControlEvent::AttemptFinished {
                execution_id,
                attempt_id,
                attempt_number,
                worker_id,
                outcome,
                ..
            } => {
                let Some(current) = state.attempts.get(&attempt_id) else {
                    tracing::debug!(%attempt_id, "ignoring terminal event for inactive attempt");
                    return Ok(());
                };
                if current.execution_id != execution_id
                    || current.attempt_number != attempt_number
                    || current.worker_id != worker_id
                    || current.runtime_host_id != runtime_host_id
                    || current.runtime_session_id != runtime_session_id
                {
                    return Err(RuntimeControlError::OwnershipMismatch);
                }
                state.terminal.entry(execution_id).or_insert(outcome);
                state.attempts.remove(&attempt_id);
            }
            RuntimeControlEvent::Heartbeat { .. }
            | RuntimeControlEvent::Registered { .. }
            | RuntimeControlEvent::CommandResult { .. } => {}
        }
        Ok(())
    }
}

fn command_outcome(event: &RuntimeControlEvent) -> RuntimeControlResult<ControlCommandOutcome> {
    match event {
        RuntimeControlEvent::CommandResult { outcome, .. } => Ok(*outcome),
        _ => Err(RuntimeControlError::InvalidCommandResult),
    }
}

fn command_host_id(command: &RuntimeControlCommand) -> &RuntimeHostId {
    match command {
        RuntimeControlCommand::TerminateAttempt {
            runtime_host_id, ..
        }
        | RuntimeControlCommand::DrainRuntime {
            runtime_host_id, ..
        }
        | RuntimeControlCommand::ShutdownRuntime {
            runtime_host_id, ..
        } => runtime_host_id,
    }
}

fn event_identity(event: &RuntimeControlEvent) -> (&RuntimeHostId, &RuntimeSessionId) {
    match event {
        RuntimeControlEvent::Registered {
            runtime_host_id,
            runtime_session_id,
            ..
        }
        | RuntimeControlEvent::AttemptStarted {
            runtime_host_id,
            runtime_session_id,
            ..
        }
        | RuntimeControlEvent::AttemptFinished {
            runtime_host_id,
            runtime_session_id,
            ..
        }
        | RuntimeControlEvent::CommandResult {
            runtime_host_id,
            runtime_session_id,
            ..
        }
        | RuntimeControlEvent::Heartbeat {
            runtime_host_id,
            runtime_session_id,
            ..
        } => (runtime_host_id, runtime_session_id),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Barrier, Mutex};

    use super::*;

    struct RecordingChannel {
        commands: Mutex<Vec<RuntimeControlCommand>>,
        terminate_outcome: ControlCommandOutcome,
    }

    impl RecordingChannel {
        fn confirming() -> Arc<Self> {
            Arc::new(Self {
                commands: Mutex::new(Vec::new()),
                terminate_outcome: ControlCommandOutcome::Confirmed,
            })
        }
    }

    impl RuntimeControlChannel for RecordingChannel {
        fn send(
            &self,
            command: RuntimeControlCommand,
        ) -> RuntimeControlResult<RuntimeControlEvent> {
            let outcome = if matches!(command, RuntimeControlCommand::TerminateAttempt { .. }) {
                self.terminate_outcome
            } else {
                ControlCommandOutcome::Confirmed
            };
            let command_message_id = match &command {
                RuntimeControlCommand::TerminateAttempt { message_id, .. }
                | RuntimeControlCommand::DrainRuntime { message_id, .. }
                | RuntimeControlCommand::ShutdownRuntime { message_id, .. } => message_id.clone(),
            };
            let runtime_host_id = command_host_id(&command).clone();
            let runtime_session_id = match &command {
                RuntimeControlCommand::TerminateAttempt {
                    runtime_session_id, ..
                }
                | RuntimeControlCommand::DrainRuntime {
                    runtime_session_id, ..
                }
                | RuntimeControlCommand::ShutdownRuntime {
                    runtime_session_id, ..
                } => runtime_session_id.clone(),
            };
            self.commands
                .lock()
                .expect("recorded commands should lock")
                .push(command);
            Ok(RuntimeControlEvent::CommandResult {
                protocol_version: RUNTIME_CONTROL_PROTOCOL_VERSION.to_string(),
                message_id: ControlMessageId::new(),
                runtime_host_id,
                runtime_session_id,
                command_message_id,
                outcome,
                message: None,
            })
        }
    }

    struct PausedCancellationChannel {
        entered: Arc<Barrier>,
        release: Arc<Barrier>,
    }

    impl RuntimeControlChannel for PausedCancellationChannel {
        fn send(
            &self,
            command: RuntimeControlCommand,
        ) -> RuntimeControlResult<RuntimeControlEvent> {
            self.entered.wait();
            self.release.wait();
            let runtime_host_id = command_host_id(&command).clone();
            let (runtime_session_id, command_message_id) = match command {
                RuntimeControlCommand::TerminateAttempt {
                    runtime_session_id,
                    message_id,
                    ..
                } => (runtime_session_id, message_id),
                _ => panic!("expected terminate command"),
            };
            Ok(RuntimeControlEvent::CommandResult {
                protocol_version: RUNTIME_CONTROL_PROTOCOL_VERSION.to_string(),
                message_id: ControlMessageId::new(),
                runtime_host_id,
                runtime_session_id,
                command_message_id,
                outcome: ControlCommandOutcome::Confirmed,
                message: None,
            })
        }
    }

    #[test]
    fn cancellation_is_exact_duplicate_safe_and_terminal() {
        let channel = RecordingChannel::confirming();
        let service = RuntimeControlService::new(channel.clone());
        let ownership = ownership();
        service.register_attempt(ownership.clone());

        assert_eq!(
            service.cancel(&ownership.execution_id).unwrap(),
            ControlCommandOutcome::Confirmed
        );
        assert_eq!(
            service.cancel(&ownership.execution_id).unwrap(),
            ControlCommandOutcome::AlreadyTerminal
        );
        assert_eq!(
            service.finish_attempt(&attempt(&ownership), AttemptOutcome::Succeeded),
            AttemptOutcome::Cancelled
        );
        let commands = channel.commands.lock().unwrap();
        assert_eq!(commands.len(), 1);
        match &commands[0] {
            RuntimeControlCommand::TerminateAttempt {
                execution_id,
                attempt_id,
                attempt_number,
                runtime_host_id,
                runtime_session_id,
                ..
            } => {
                assert_eq!(execution_id, &ownership.execution_id);
                assert_eq!(attempt_id, &ownership.attempt_id);
                assert_eq!(*attempt_number, ownership.attempt_number);
                assert_eq!(runtime_host_id, &ownership.runtime_host_id);
                assert_eq!(runtime_session_id, &ownership.runtime_session_id);
            }
            _ => panic!("expected terminate command"),
        }
    }

    #[test]
    fn completion_and_timeout_cannot_be_overwritten_by_cancellation() {
        for outcome in [AttemptOutcome::Succeeded, AttemptOutcome::TimedOut] {
            let service = RuntimeControlService::new(RecordingChannel::confirming());
            let ownership = ownership();
            service.register_attempt(ownership.clone());
            assert_eq!(
                service.finish_attempt(&attempt(&ownership), outcome),
                outcome
            );
            assert_eq!(
                service.cancel(&ownership.execution_id).unwrap(),
                ControlCommandOutcome::AlreadyTerminal
            );
            assert_eq!(
                service.terminal_outcome(&ownership.execution_id),
                Some(outcome)
            );
        }
    }

    #[test]
    fn cancellation_racing_with_success_keeps_the_success_winner() {
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let service = RuntimeControlService::new(Arc::new(PausedCancellationChannel {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        }));
        let ownership = ownership();
        service.register_attempt(ownership.clone());
        let cancelling = {
            let service = service.clone();
            let execution_id = ownership.execution_id.clone();
            std::thread::spawn(move || service.cancel(&execution_id).unwrap())
        };
        entered.wait();

        assert_eq!(
            service.finish_attempt(&attempt(&ownership), AttemptOutcome::Succeeded),
            AttemptOutcome::Succeeded
        );
        release.wait();
        assert_eq!(
            cancelling.join().unwrap(),
            ControlCommandOutcome::AlreadyTerminal
        );
        assert_eq!(
            service.terminal_outcome(&ownership.execution_id),
            Some(AttemptOutcome::Succeeded)
        );
    }

    #[test]
    fn cancellation_racing_with_timeout_keeps_the_timeout_winner() {
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let service = RuntimeControlService::new(Arc::new(PausedCancellationChannel {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        }));
        let ownership = ownership();
        service.register_attempt(ownership.clone());
        let cancelling = {
            let service = service.clone();
            let execution_id = ownership.execution_id.clone();
            std::thread::spawn(move || service.cancel(&execution_id).unwrap())
        };
        entered.wait();

        assert_eq!(
            service.finish_attempt(&attempt(&ownership), AttemptOutcome::TimedOut),
            AttemptOutcome::TimedOut
        );
        release.wait();
        assert_eq!(
            cancelling.join().unwrap(),
            ControlCommandOutcome::AlreadyTerminal
        );
        assert_eq!(
            service.terminal_outcome(&ownership.execution_id),
            Some(AttemptOutcome::TimedOut)
        );
    }

    #[test]
    fn unknown_attempt_has_semantic_result() {
        let service = RuntimeControlService::new(RecordingChannel::confirming());
        assert_eq!(
            service.cancel(&ExecutionId::new()).unwrap(),
            ControlCommandOutcome::AttemptNotFound
        );
    }

    #[test]
    fn ingress_replaces_active_snapshot_and_fences_stale_events() {
        let service = RuntimeControlService::new(RecordingChannel::confirming());
        let ingress = service.ingress();
        let runtime_host_id = RuntimeHostId::new();
        let stale_session = RuntimeSessionId::new();
        let current_session = RuntimeSessionId::new();
        let stale_attempt = ownership();
        let current_attempt = ownership();

        ingress
            .register(registration(
                &runtime_host_id,
                &stale_session,
                &stale_attempt,
            ))
            .unwrap();
        ingress
            .register(registration(
                &runtime_host_id,
                &current_session,
                &current_attempt,
            ))
            .unwrap();

        assert!(service
            .attempt_ownership(&stale_attempt.attempt_id)
            .is_none());
        assert_eq!(
            service
                .attempt_ownership(&current_attempt.attempt_id)
                .unwrap()
                .runtime_session_id,
            current_session
        );
        let mismatched_finish = RuntimeControlEvent::AttemptFinished {
            protocol_version: RUNTIME_CONTROL_PROTOCOL_VERSION.to_string(),
            message_id: ControlMessageId::new(),
            runtime_host_id: runtime_host_id.clone(),
            runtime_session_id: current_session,
            execution_id: current_attempt.execution_id.clone(),
            attempt_id: current_attempt.attempt_id.clone(),
            attempt_number: current_attempt.attempt_number,
            worker_id: WorkerId::new(),
            outcome: AttemptOutcome::Succeeded,
        };
        assert!(matches!(
            ingress.apply(mismatched_finish),
            Err(RuntimeControlError::OwnershipMismatch)
        ));
        assert!(service
            .attempt_ownership(&current_attempt.attempt_id)
            .is_some());
        let stale_event = RuntimeControlEvent::AttemptStarted {
            protocol_version: RUNTIME_CONTROL_PROTOCOL_VERSION.to_string(),
            message_id: ControlMessageId::new(),
            runtime_host_id,
            runtime_session_id: stale_session,
            execution_id: ExecutionId::new(),
            attempt_id: AttemptId::new(),
            attempt_number: 1,
            worker_id: WorkerId::new(),
        };
        assert!(matches!(
            ingress.apply(stale_event),
            Err(RuntimeControlError::StaleSession { .. })
        ));
    }

    #[test]
    fn drain_keeps_active_work_and_shutdown_honors_grace_before_termination() {
        let channel = RecordingChannel::confirming();
        let service = RuntimeControlService::new(channel.clone());
        let ownership = ownership();
        service.register_attempt(ownership.clone());

        service.drain().unwrap();
        assert_eq!(service.terminal_outcome(&ownership.execution_id), None);
        let started = Instant::now();
        service.shutdown(Duration::from_millis(35)).unwrap();

        assert!(started.elapsed() >= Duration::from_millis(30));
        assert_eq!(
            service.terminal_outcome(&ownership.execution_id),
            Some(AttemptOutcome::Cancelled)
        );
        let commands = channel.commands.lock().unwrap();
        assert!(matches!(
            commands[0],
            RuntimeControlCommand::DrainRuntime { .. }
        ));
        assert!(matches!(
            commands[1],
            RuntimeControlCommand::DrainRuntime { .. }
        ));
        assert!(matches!(
            commands[2],
            RuntimeControlCommand::ShutdownRuntime { .. }
        ));
    }

    fn ownership() -> AttemptOwnership {
        AttemptOwnership {
            execution_id: ExecutionId::new(),
            attempt_id: AttemptId::new(),
            attempt_number: 2,
            runtime_host_id: RuntimeHostId::new(),
            runtime_session_id: RuntimeSessionId::new(),
            worker_id: WorkerId::new(),
        }
    }

    fn registration(
        runtime_host_id: &RuntimeHostId,
        runtime_session_id: &RuntimeSessionId,
        ownership: &AttemptOwnership,
    ) -> RuntimeRegistration {
        RuntimeRegistration {
            protocol_version: RUNTIME_CONTROL_PROTOCOL_VERSION.to_string(),
            message_id: ControlMessageId::new(),
            runtime_host_id: runtime_host_id.clone(),
            runtime_session_id: runtime_session_id.clone(),
            revision: "test".into(),
            max_concurrency: 1,
            capabilities: ryvus_protocol::RuntimeCapabilities {
                terminate_attempt: true,
                drain: true,
                shutdown: true,
            },
            active_attempts: vec![ryvus_protocol::ActiveAttemptOwnership {
                execution_id: ownership.execution_id.clone(),
                attempt_id: ownership.attempt_id.clone(),
                attempt_number: ownership.attempt_number,
                worker_id: ownership.worker_id.clone(),
            }],
        }
    }

    fn attempt(ownership: &AttemptOwnership) -> ExecutionAttempt {
        ExecutionAttempt {
            execution_id: ownership.execution_id.clone(),
            attempt_id: ownership.attempt_id.clone(),
            attempt_number: ownership.attempt_number,
        }
    }
}
