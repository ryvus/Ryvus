use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};

use ryvus_protocol::{
    AttemptId, AttemptOutcome, ControlCommandOutcome, ControlMessageId, ExecutionAttempt,
    ExecutionId, RuntimeControlCommand, RuntimeControlEvent, RuntimeHostId, RuntimeRegistration,
    RuntimeSessionId, TerminationReason, WorkerId, RUNTIME_CONTROL_PROTOCOL_VERSION,
};
use ryvus_runtime_host::RuntimeHostControlSender;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    ExecutionMutation, ExecutionStateStore, StateStoreError, TerminalState, TransitionResult,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    #[error("execution state store error: {0}")]
    StateStore(#[from] StateStoreError),
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
    runtimes: HashMap<RuntimeHostId, RuntimeSessionId>,
}

#[derive(Clone)]
pub struct RuntimeControlService {
    channel: Arc<dyn RuntimeControlChannel>,
    store: Arc<dyn ExecutionStateStore>,
    state: Arc<Mutex<ControlState>>,
}

#[derive(Clone)]
pub struct RuntimeControlIngress {
    state: Arc<Mutex<ControlState>>,
    store: Arc<dyn ExecutionStateStore>,
    control: RuntimeControlService,
}

impl RuntimeControlService {
    pub fn new(
        channel: Arc<dyn RuntimeControlChannel>,
        store: Arc<dyn ExecutionStateStore>,
    ) -> Self {
        Self {
            channel,
            store,
            state: Arc::new(Mutex::new(ControlState::default())),
        }
    }

    pub fn ingress(&self) -> RuntimeControlIngress {
        RuntimeControlIngress {
            state: Arc::clone(&self.state),
            store: Arc::clone(&self.store),
            control: self.clone(),
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

    pub fn attempt_ownership(
        &self,
        attempt_id: &AttemptId,
    ) -> RuntimeControlResult<Option<AttemptOwnership>> {
        Ok(self
            .store
            .active_executions()?
            .into_iter()
            .flat_map(|aggregate| aggregate.attempts)
            .find(|attempt| &attempt.attempt.attempt_id == attempt_id)
            .and_then(|attempt| attempt.ownership))
    }

    pub fn register_attempt(&self, ownership: AttemptOwnership) -> RuntimeControlResult<()> {
        assign_ownership(&self.store, ownership.clone())?;
        self.state
            .lock()
            .expect("runtime control state should lock")
            .runtimes
            .insert(
                ownership.runtime_host_id.clone(),
                ownership.runtime_session_id.clone(),
            );
        Ok(())
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
        let aggregate = match request_cancellation(&self.store, execution_id) {
            Ok(aggregate) => aggregate,
            Err(RuntimeControlError::StateStore(StateStoreError::NotFound { .. })) => {
                return Ok(ControlCommandOutcome::AttemptNotFound)
            }
            Err(error) => return Err(error),
        };
        if aggregate.terminal_state.is_some() {
            return Ok(ControlCommandOutcome::AlreadyTerminal);
        }
        let Some(ownership) = active_ownership(&aggregate) else {
            return Ok(ControlCommandOutcome::AttemptNotFound);
        };
        match self.current_session(&ownership.runtime_host_id) {
            None => {
                return Err(RuntimeControlError::Channel(format!(
                    "runtime host '{}' is not registered",
                    ownership.runtime_host_id
                )))
            }
            Some(current) if current != ownership.runtime_session_id => {
                return Err(RuntimeControlError::StaleSession {
                    runtime_host_id: ownership.runtime_host_id,
                    runtime_session_id: ownership.runtime_session_id,
                })
            }
            Some(_) => {}
        }
        self.deliver_cancellation(ownership)
    }

    fn deliver_cancellation(
        &self,
        ownership: AttemptOwnership,
    ) -> RuntimeControlResult<ControlCommandOutcome> {
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
        let outcome = command_outcome(&event)?;
        if outcome == ControlCommandOutcome::Confirmed {
            return accept_terminal(
                &self.store,
                &ownership.execution_id,
                &ownership.attempt_id,
                AttemptOutcome::Cancelled,
            )
            .map(|winner| {
                if winner == AttemptOutcome::Cancelled {
                    ControlCommandOutcome::Confirmed
                } else {
                    ControlCommandOutcome::AlreadyTerminal
                }
            });
        }
        Ok(outcome)
    }

    pub fn reconcile_cancellations(&self) -> RuntimeControlResult<()> {
        for aggregate in self.store.reconcilable_cancellations()? {
            let Some(ownership) = active_ownership(&aggregate) else {
                continue;
            };
            if self.current_session(&ownership.runtime_host_id).as_ref()
                == Some(&ownership.runtime_session_id)
            {
                self.deliver_cancellation(ownership)?;
            }
        }
        Ok(())
    }

    pub fn finish_attempt(
        &self,
        attempt: &ExecutionAttempt,
        outcome: AttemptOutcome,
    ) -> RuntimeControlResult<AttemptOutcome> {
        accept_terminal(
            &self.store,
            &attempt.execution_id,
            &attempt.attempt_id,
            outcome,
        )
    }

    pub fn terminal_outcome(
        &self,
        execution_id: &ExecutionId,
    ) -> RuntimeControlResult<Option<AttemptOutcome>> {
        Ok(self
            .store
            .load(execution_id)?
            .and_then(|aggregate| aggregate.terminal_state)
            .map(|terminal| state_outcome(terminal.state)))
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
            if self.store.active_executions()?.is_empty() {
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
                for aggregate in self.store.active_executions()? {
                    if let Some(ownership) = active_ownership(&aggregate) {
                        if ownership.runtime_host_id == completed_host_id {
                            let _ = accept_terminal(
                                &self.store,
                                &ownership.execution_id,
                                &ownership.attempt_id,
                                AttemptOutcome::Cancelled,
                            )?;
                        }
                    }
                }
                self.state
                    .lock()
                    .expect("runtime control state should lock")
                    .runtimes
                    .remove(&completed_host_id);
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
    pub fn reconcile_cancellations(&self) -> RuntimeControlResult<()> {
        self.control.reconcile_cancellations()
    }

    pub fn register(&self, registration: RuntimeRegistration) -> RuntimeControlResult<()> {
        registration
            .validate()
            .map_err(|error| RuntimeControlError::InvalidMessage(error.to_string()))?;
        let active_attempt_ids = registration
            .active_attempts
            .iter()
            .map(|attempt| attempt.attempt_id.clone())
            .collect::<Vec<_>>();
        let mut state = self
            .state
            .lock()
            .expect("runtime control state should lock");
        for aggregate in self.store.active_executions()? {
            let Some(ownership) = active_ownership(&aggregate) else {
                continue;
            };
            if ownership.runtime_host_id == registration.runtime_host_id
                && !active_attempt_ids.contains(&ownership.attempt_id)
            {
                clear_ownership(&self.store, ownership)?;
            }
        }
        for active in registration.active_attempts {
            let ownership = AttemptOwnership {
                execution_id: active.execution_id,
                attempt_id: active.attempt_id,
                attempt_number: active.attempt_number,
                runtime_host_id: registration.runtime_host_id.clone(),
                runtime_session_id: registration.runtime_session_id.clone(),
                worker_id: active.worker_id,
            };
            match assign_ownership(&self.store, ownership) {
                Ok(()) | Err(RuntimeControlError::StateStore(StateStoreError::NotFound { .. })) => {
                }
                Err(error) => return Err(error),
            }
        }
        state.runtimes.insert(
            registration.runtime_host_id,
            registration.runtime_session_id,
        );
        Ok(())
    }

    pub fn apply(&self, event: RuntimeControlEvent) -> RuntimeControlResult<()> {
        event
            .validate()
            .map_err(|error| RuntimeControlError::InvalidMessage(error.to_string()))?;
        let (runtime_host_id, runtime_session_id) = event_identity(&event);
        let runtime_host_id = runtime_host_id.clone();
        let runtime_session_id = runtime_session_id.clone();
        let state = self
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
                assign_ownership(
                    &self.store,
                    AttemptOwnership {
                        execution_id,
                        attempt_id,
                        attempt_number,
                        runtime_host_id,
                        runtime_session_id,
                        worker_id,
                    },
                )?;
            }
            RuntimeControlEvent::AttemptFinished {
                execution_id,
                attempt_id,
                attempt_number,
                worker_id,
                outcome,
                ..
            } => {
                let Some(aggregate) = self.store.load(&execution_id)? else {
                    tracing::debug!(%attempt_id, "ignoring terminal event for inactive attempt");
                    return Ok(());
                };
                let Some(current) = active_ownership(&aggregate) else {
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
                if outcome == AttemptOutcome::TimedOut {
                    accept_terminal(&self.store, &execution_id, &attempt_id, outcome)?;
                }
            }
            RuntimeControlEvent::Heartbeat { .. }
            | RuntimeControlEvent::Registered { .. }
            | RuntimeControlEvent::CommandResult { .. } => {}
        }
        Ok(())
    }
}

fn active_ownership(aggregate: &crate::ExecutionAggregate) -> Option<AttemptOwnership> {
    let active_attempt_id = aggregate.active_attempt_id.as_ref()?;
    aggregate
        .attempts
        .iter()
        .find(|attempt| &attempt.attempt.attempt_id == active_attempt_id)
        .and_then(|attempt| attempt.ownership.clone())
}

fn assign_ownership(
    store: &Arc<dyn ExecutionStateStore>,
    ownership: AttemptOwnership,
) -> RuntimeControlResult<()> {
    loop {
        let aggregate =
            store
                .load(&ownership.execution_id)?
                .ok_or_else(|| StateStoreError::NotFound {
                    execution_id: ownership.execution_id.clone(),
                })?;
        match store.compare_and_set(
            &ownership.execution_id,
            aggregate.execution_version,
            ExecutionMutation::AssignOwnership {
                attempt_id: ownership.attempt_id.clone(),
                ownership: ownership.clone(),
            },
        )? {
            TransitionResult::Applied { .. } | TransitionResult::Unchanged { .. } => return Ok(()),
            TransitionResult::Conflict { .. } => continue,
        }
    }
}

fn clear_ownership(
    store: &Arc<dyn ExecutionStateStore>,
    ownership: AttemptOwnership,
) -> RuntimeControlResult<()> {
    loop {
        let aggregate =
            store
                .load(&ownership.execution_id)?
                .ok_or_else(|| StateStoreError::NotFound {
                    execution_id: ownership.execution_id.clone(),
                })?;
        match store.compare_and_set(
            &ownership.execution_id,
            aggregate.execution_version,
            ExecutionMutation::ClearOwnership {
                attempt_id: ownership.attempt_id.clone(),
                expected_ownership: ownership.clone(),
            },
        )? {
            TransitionResult::Applied { .. } | TransitionResult::Unchanged { .. } => return Ok(()),
            TransitionResult::Conflict { .. } => continue,
        }
    }
}

fn request_cancellation(
    store: &Arc<dyn ExecutionStateStore>,
    execution_id: &ExecutionId,
) -> RuntimeControlResult<crate::ExecutionAggregate> {
    loop {
        let aggregate = store
            .load(execution_id)?
            .ok_or_else(|| StateStoreError::NotFound {
                execution_id: execution_id.clone(),
            });
        let aggregate = match aggregate {
            Ok(aggregate) => aggregate,
            Err(StateStoreError::NotFound { .. }) => {
                return Err(RuntimeControlError::StateStore(StateStoreError::NotFound {
                    execution_id: execution_id.clone(),
                }))
            }
            Err(error) => return Err(error.into()),
        };
        if aggregate.terminal_state.is_some() || aggregate.cancellation_intent.is_some() {
            return Ok(aggregate);
        }
        match store.compare_and_set(
            execution_id,
            aggregate.execution_version,
            ExecutionMutation::RequestCancellation {
                requested_at: SystemTime::now(),
            },
        )? {
            TransitionResult::Applied { aggregate } | TransitionResult::Unchanged { aggregate } => {
                return Ok(aggregate)
            }
            TransitionResult::Conflict { .. } => continue,
        }
    }
}

fn accept_terminal(
    store: &Arc<dyn ExecutionStateStore>,
    execution_id: &ExecutionId,
    attempt_id: &AttemptId,
    outcome: AttemptOutcome,
) -> RuntimeControlResult<AttemptOutcome> {
    loop {
        let aggregate = store
            .load(execution_id)?
            .ok_or_else(|| StateStoreError::NotFound {
                execution_id: execution_id.clone(),
            })?;
        if let Some(terminal) = aggregate.terminal_state {
            return Ok(state_outcome(terminal.state));
        }
        match store.compare_and_set(
            execution_id,
            aggregate.execution_version,
            ExecutionMutation::FinishAttempt {
                attempt_id: attempt_id.clone(),
                outcome,
                result: None,
                retry: None,
                terminal: Some(TerminalState::new(
                    outcome_state(outcome),
                    Some(attempt_id.clone()),
                )),
            },
        )? {
            TransitionResult::Applied { .. } => return Ok(outcome),
            TransitionResult::Unchanged { aggregate } => {
                return Ok(aggregate
                    .terminal_state
                    .map(|terminal| state_outcome(terminal.state))
                    .unwrap_or(outcome));
            }
            TransitionResult::Conflict { .. } => continue,
        }
    }
}

fn outcome_state(outcome: AttemptOutcome) -> crate::ExecutionState {
    match outcome {
        AttemptOutcome::Succeeded => crate::ExecutionState::Succeeded,
        AttemptOutcome::Failed | AttemptOutcome::InfrastructureFailed => {
            crate::ExecutionState::Failed
        }
        AttemptOutcome::Cancelled => crate::ExecutionState::Cancelled,
        AttemptOutcome::TimedOut => crate::ExecutionState::TimedOut,
    }
}

fn state_outcome(state: crate::ExecutionState) -> AttemptOutcome {
    match state {
        crate::ExecutionState::Succeeded => AttemptOutcome::Succeeded,
        crate::ExecutionState::Cancelled => AttemptOutcome::Cancelled,
        crate::ExecutionState::TimedOut => AttemptOutcome::TimedOut,
        _ => AttemptOutcome::Failed,
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
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Barrier, Mutex,
    };

    use ryvus_protocol::{ActionDefinition, ActionKind, ApiAction, InvocationRequest, RuntimeKind};
    use serde_json::json;

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

    struct SwitchableChannel {
        fail: AtomicBool,
        commands: Mutex<Vec<RuntimeControlCommand>>,
    }

    struct IntentInspectingChannel {
        store: Arc<crate::MemoryExecutionStateStore>,
    }

    struct BlockingAssignStore {
        inner: crate::MemoryExecutionStateStore,
        armed: AtomicBool,
        entered: Barrier,
        release: Barrier,
    }

    impl ExecutionStateStore for BlockingAssignStore {
        fn create(
            &self,
            execution: crate::NewExecution,
        ) -> crate::StateStoreResult<crate::ExecutionAggregate> {
            self.inner.create(execution)
        }

        fn create_idempotent(
            &self,
            execution: crate::NewExecution,
        ) -> crate::StateStoreResult<crate::CreateExecutionResult> {
            self.inner.create_idempotent(execution)
        }

        fn load(
            &self,
            execution_id: &ExecutionId,
        ) -> crate::StateStoreResult<Option<crate::ExecutionAggregate>> {
            self.inner.load(execution_id)
        }

        fn compare_and_set(
            &self,
            execution_id: &ExecutionId,
            expected_version: u64,
            mutation: ExecutionMutation,
        ) -> crate::StateStoreResult<TransitionResult> {
            if matches!(mutation, ExecutionMutation::AssignOwnership { .. })
                && self.armed.swap(false, Ordering::SeqCst)
            {
                self.entered.wait();
                self.release.wait();
            }
            self.inner
                .compare_and_set(execution_id, expected_version, mutation)
        }

        fn reconcilable_cancellations(
            &self,
        ) -> crate::StateStoreResult<Vec<crate::ExecutionAggregate>> {
            self.inner.reconcilable_cancellations()
        }

        fn active_executions(&self) -> crate::StateStoreResult<Vec<crate::ExecutionAggregate>> {
            self.inner.active_executions()
        }

        fn list_history(
            &self,
            query: crate::ExecutionHistoryQuery,
        ) -> crate::StateStoreResult<crate::ExecutionHistoryPage> {
            self.inner.list_history(query)
        }
    }

    impl RuntimeControlChannel for IntentInspectingChannel {
        fn send(
            &self,
            command: RuntimeControlCommand,
        ) -> RuntimeControlResult<RuntimeControlEvent> {
            let RuntimeControlCommand::TerminateAttempt {
                execution_id,
                runtime_host_id,
                runtime_session_id,
                message_id,
                ..
            } = command
            else {
                panic!("expected terminate command")
            };
            assert!(self
                .store
                .load(&execution_id)
                .unwrap()
                .unwrap()
                .cancellation_intent
                .is_some());
            Ok(RuntimeControlEvent::CommandResult {
                protocol_version: RUNTIME_CONTROL_PROTOCOL_VERSION.into(),
                message_id: ControlMessageId::new(),
                runtime_host_id,
                runtime_session_id,
                command_message_id: message_id,
                outcome: ControlCommandOutcome::Confirmed,
                message: None,
            })
        }
    }

    impl RuntimeControlChannel for SwitchableChannel {
        fn send(
            &self,
            command: RuntimeControlCommand,
        ) -> RuntimeControlResult<RuntimeControlEvent> {
            if self.fail.load(Ordering::SeqCst) {
                return Err(RuntimeControlError::Channel("disconnected".into()));
            }
            let runtime_host_id = command_host_id(&command).clone();
            let (runtime_session_id, command_message_id) = match &command {
                RuntimeControlCommand::TerminateAttempt {
                    runtime_session_id,
                    message_id,
                    ..
                }
                | RuntimeControlCommand::DrainRuntime {
                    runtime_session_id,
                    message_id,
                    ..
                }
                | RuntimeControlCommand::ShutdownRuntime {
                    runtime_session_id,
                    message_id,
                    ..
                } => (runtime_session_id.clone(), message_id.clone()),
            };
            self.commands.lock().unwrap().push(command);
            Ok(RuntimeControlEvent::CommandResult {
                protocol_version: RUNTIME_CONTROL_PROTOCOL_VERSION.into(),
                message_id: ControlMessageId::new(),
                runtime_host_id,
                runtime_session_id,
                command_message_id,
                outcome: ControlCommandOutcome::Confirmed,
                message: None,
            })
        }
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
        let service = test_service(channel.clone());
        let ownership = ownership();
        register_test_attempt(&service, ownership.clone());

        assert_eq!(
            service.cancel(&ownership.execution_id).unwrap(),
            ControlCommandOutcome::Confirmed
        );
        assert_eq!(
            service.cancel(&ownership.execution_id).unwrap(),
            ControlCommandOutcome::AlreadyTerminal
        );
        assert_eq!(
            service
                .finish_attempt(&attempt(&ownership), AttemptOutcome::Succeeded)
                .unwrap(),
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
            let service = test_service(RecordingChannel::confirming());
            let ownership = ownership();
            register_test_attempt(&service, ownership.clone());
            assert_eq!(
                service
                    .finish_attempt(&attempt(&ownership), outcome)
                    .unwrap(),
                outcome
            );
            assert_eq!(
                service.cancel(&ownership.execution_id).unwrap(),
                ControlCommandOutcome::AlreadyTerminal
            );
            assert_eq!(
                service.terminal_outcome(&ownership.execution_id).unwrap(),
                Some(outcome)
            );
        }
    }

    #[test]
    fn cancellation_racing_with_success_keeps_the_success_winner() {
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let service = test_service(Arc::new(PausedCancellationChannel {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        }));
        let ownership = ownership();
        register_test_attempt(&service, ownership.clone());
        let cancelling = {
            let service = service.clone();
            let execution_id = ownership.execution_id.clone();
            std::thread::spawn(move || service.cancel(&execution_id).unwrap())
        };
        entered.wait();

        assert_eq!(
            service
                .finish_attempt(&attempt(&ownership), AttemptOutcome::Succeeded)
                .unwrap(),
            AttemptOutcome::Succeeded
        );
        release.wait();
        assert_eq!(
            cancelling.join().unwrap(),
            ControlCommandOutcome::AlreadyTerminal
        );
        assert_eq!(
            service.terminal_outcome(&ownership.execution_id).unwrap(),
            Some(AttemptOutcome::Succeeded)
        );
    }

    #[test]
    fn cancellation_racing_with_timeout_keeps_the_timeout_winner() {
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let service = test_service(Arc::new(PausedCancellationChannel {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        }));
        let ownership = ownership();
        register_test_attempt(&service, ownership.clone());
        let cancelling = {
            let service = service.clone();
            let execution_id = ownership.execution_id.clone();
            std::thread::spawn(move || service.cancel(&execution_id).unwrap())
        };
        entered.wait();

        assert_eq!(
            service
                .finish_attempt(&attempt(&ownership), AttemptOutcome::TimedOut)
                .unwrap(),
            AttemptOutcome::TimedOut
        );
        release.wait();
        assert_eq!(
            cancelling.join().unwrap(),
            ControlCommandOutcome::AlreadyTerminal
        );
        assert_eq!(
            service.terminal_outcome(&ownership.execution_id).unwrap(),
            Some(AttemptOutcome::TimedOut)
        );
    }

    #[test]
    fn unknown_attempt_has_semantic_result() {
        let service = test_service(RecordingChannel::confirming());
        assert_eq!(
            service.cancel(&ExecutionId::new()).unwrap(),
            ControlCommandOutcome::AttemptNotFound
        );
    }

    #[test]
    fn failed_ownership_persistence_does_not_publish_runtime() {
        let mut store = crate::MockExecutionStateStore::new();
        store
            .expect_load()
            .once()
            .returning(|_| Err(StateStoreError::LockPoisoned));
        let service = RuntimeControlService::new(RecordingChannel::confirming(), Arc::new(store));
        let ownership = ownership();

        assert!(matches!(
            service.register_attempt(ownership.clone()),
            Err(RuntimeControlError::StateStore(
                StateStoreError::LockPoisoned
            ))
        ));
        assert_eq!(service.current_session(&ownership.runtime_host_id), None);
        let executor_error: crate::ExecutorError =
            RuntimeControlError::StateStore(StateStoreError::LockPoisoned).into();
        assert!(matches!(
            executor_error,
            crate::ExecutorError::RuntimeControl(RuntimeControlError::StateStore(
                StateStoreError::LockPoisoned
            ))
        ));
    }

    #[test]
    fn ownership_lookup_propagates_store_failure() {
        let mut store = crate::MockExecutionStateStore::new();
        store
            .expect_active_executions()
            .once()
            .returning(|| Err(StateStoreError::Backend("unavailable".into())));
        let service = RuntimeControlService::new(RecordingChannel::confirming(), Arc::new(store));

        assert!(matches!(
            service.attempt_ownership(&AttemptId::new()),
            Err(RuntimeControlError::StateStore(StateStoreError::Backend(
                message
            ))) if message == "unavailable"
        ));
    }

    #[test]
    fn terminal_lookup_propagates_store_failure() {
        let mut store = crate::MockExecutionStateStore::new();
        store
            .expect_load()
            .once()
            .returning(|_| Err(StateStoreError::Backend("unavailable".into())));
        let service = RuntimeControlService::new(RecordingChannel::confirming(), Arc::new(store));

        assert!(matches!(
            service.terminal_outcome(&ExecutionId::new()),
            Err(RuntimeControlError::StateStore(StateStoreError::Backend(
                message
            ))) if message == "unavailable"
        ));
    }

    #[test]
    fn transport_failure_keeps_intent_for_reconciliation() {
        let channel = Arc::new(SwitchableChannel {
            fail: AtomicBool::new(true),
            commands: Mutex::new(Vec::new()),
        });
        let service = test_service(channel.clone());
        let ownership = ownership();
        register_test_attempt(&service, ownership.clone());

        assert!(matches!(
            service.cancel(&ownership.execution_id),
            Err(RuntimeControlError::Channel(_))
        ));
        let pending = service
            .store
            .load(&ownership.execution_id)
            .unwrap()
            .unwrap();
        assert!(pending.cancellation_intent.is_some());
        assert!(pending.terminal_state.is_none());

        channel.fail.store(false, Ordering::SeqCst);
        service.reconcile_cancellations().unwrap();
        assert_eq!(
            service.terminal_outcome(&ownership.execution_id).unwrap(),
            Some(AttemptOutcome::Cancelled)
        );
        assert_eq!(channel.commands.lock().unwrap().len(), 1);
    }

    #[test]
    fn cancellation_reconciles_after_service_recreation_and_registration() {
        let store = Arc::new(crate::MemoryExecutionStateStore::default());
        let channel = Arc::new(SwitchableChannel {
            fail: AtomicBool::new(true),
            commands: Mutex::new(Vec::new()),
        });
        let ownership = ownership();
        {
            let service = RuntimeControlService::new(channel.clone(), store.clone());
            register_test_attempt(&service, ownership.clone());
            assert!(matches!(
                service.cancel(&ownership.execution_id),
                Err(RuntimeControlError::Channel(_))
            ));
        }

        channel.fail.store(false, Ordering::SeqCst);
        let restarted = RuntimeControlService::new(channel.clone(), store.clone());
        let ingress = restarted.ingress();
        ingress
            .register(registration(
                &ownership.runtime_host_id,
                &ownership.runtime_session_id,
                &ownership,
            ))
            .unwrap();
        ingress.reconcile_cancellations().unwrap();

        assert_eq!(
            restarted.terminal_outcome(&ownership.execution_id).unwrap(),
            Some(AttemptOutcome::Cancelled)
        );
        assert_eq!(
            restarted
                .finish_attempt(&attempt(&ownership), AttemptOutcome::Succeeded)
                .unwrap(),
            AttemptOutcome::Cancelled
        );
        assert_eq!(
            restarted.cancel(&ownership.execution_id).unwrap(),
            ControlCommandOutcome::AlreadyTerminal
        );
        assert_eq!(channel.commands.lock().unwrap().len(), 1);
    }

    #[test]
    fn ownership_and_terminal_outcomes_reconstruct_after_service_recreation() {
        for outcome in [
            AttemptOutcome::Succeeded,
            AttemptOutcome::Failed,
            AttemptOutcome::TimedOut,
        ] {
            let store = Arc::new(crate::MemoryExecutionStateStore::default());
            let ownership = ownership();
            {
                let service =
                    RuntimeControlService::new(RecordingChannel::confirming(), store.clone());
                register_test_attempt(&service, ownership.clone());
                assert_eq!(
                    service.attempt_ownership(&ownership.attempt_id).unwrap(),
                    Some(ownership.clone())
                );
                assert_eq!(
                    service
                        .finish_attempt(&attempt(&ownership), outcome)
                        .unwrap(),
                    outcome
                );
            }

            let restarted =
                RuntimeControlService::new(RecordingChannel::confirming(), store.clone());
            assert_eq!(restarted.current_session(&ownership.runtime_host_id), None);
            assert_eq!(
                restarted.terminal_outcome(&ownership.execution_id).unwrap(),
                Some(outcome)
            );
            assert_eq!(
                restarted
                    .finish_attempt(&attempt(&ownership), AttemptOutcome::Succeeded)
                    .unwrap(),
                outcome
            );
        }
    }

    #[test]
    fn recreated_service_rebuilds_ownership_and_fences_previous_session() {
        let store = Arc::new(crate::MemoryExecutionStateStore::default());
        let previous = ownership();
        {
            let service = RuntimeControlService::new(RecordingChannel::confirming(), store.clone());
            register_test_attempt(&service, previous.clone());
        }

        let restarted = RuntimeControlService::new(RecordingChannel::confirming(), store);
        assert_eq!(
            restarted.attempt_ownership(&previous.attempt_id).unwrap(),
            Some(previous.clone())
        );
        let mut current = previous.clone();
        current.runtime_session_id = RuntimeSessionId::new();
        current.worker_id = WorkerId::new();
        let ingress = restarted.ingress();
        ingress
            .register(registration(
                &current.runtime_host_id,
                &current.runtime_session_id,
                &current,
            ))
            .unwrap();

        let stale_event = RuntimeControlEvent::AttemptFinished {
            protocol_version: RUNTIME_CONTROL_PROTOCOL_VERSION.into(),
            message_id: ControlMessageId::new(),
            runtime_host_id: previous.runtime_host_id.clone(),
            runtime_session_id: previous.runtime_session_id,
            execution_id: previous.execution_id,
            attempt_id: previous.attempt_id,
            attempt_number: previous.attempt_number,
            worker_id: previous.worker_id,
            outcome: AttemptOutcome::Succeeded,
        };
        assert!(matches!(
            ingress.apply(stale_event),
            Err(RuntimeControlError::StaleSession { .. })
        ));
        assert_eq!(
            restarted.attempt_ownership(&current.attempt_id).unwrap(),
            Some(current)
        );
    }

    #[test]
    fn cancellation_intent_is_persisted_before_delivery() {
        let store = Arc::new(crate::MemoryExecutionStateStore::default());
        let service = RuntimeControlService::new(
            Arc::new(IntentInspectingChannel {
                store: store.clone(),
            }),
            store,
        );
        let ownership = ownership();
        register_test_attempt(&service, ownership.clone());

        assert_eq!(
            service.cancel(&ownership.execution_id).unwrap(),
            ControlCommandOutcome::Confirmed
        );
    }

    #[test]
    fn ownership_refresh_is_version_neutral_but_replacement_versions_once() {
        let service = test_service(RecordingChannel::confirming());
        let ownership = ownership();
        register_test_attempt(&service, ownership.clone());
        let assigned = service
            .store
            .load(&ownership.execution_id)
            .unwrap()
            .unwrap();

        service.register_attempt(ownership.clone()).unwrap();
        let unchanged = service
            .store
            .load(&ownership.execution_id)
            .unwrap()
            .unwrap();
        assert_eq!(unchanged.execution_version, assigned.execution_version);

        let mut replacement = ownership;
        replacement.runtime_session_id = RuntimeSessionId::new();
        service.register_attempt(replacement).unwrap();
        let replaced = service
            .store
            .load(&unchanged.execution_id)
            .unwrap()
            .unwrap();
        assert_eq!(replaced.execution_version, unchanged.execution_version + 1);
    }

    #[test]
    fn failed_lifecycle_event_does_not_preempt_retry_policy() {
        let service = test_service(RecordingChannel::confirming());
        let ownership = ownership();
        register_test_attempt(&service, ownership.clone());
        let ingress = service.ingress();

        ingress
            .apply(RuntimeControlEvent::AttemptFinished {
                protocol_version: RUNTIME_CONTROL_PROTOCOL_VERSION.into(),
                message_id: ControlMessageId::new(),
                runtime_host_id: ownership.runtime_host_id.clone(),
                runtime_session_id: ownership.runtime_session_id.clone(),
                execution_id: ownership.execution_id.clone(),
                attempt_id: ownership.attempt_id.clone(),
                attempt_number: ownership.attempt_number,
                worker_id: ownership.worker_id.clone(),
                outcome: AttemptOutcome::Failed,
            })
            .unwrap();

        let aggregate = service
            .store
            .load(&ownership.execution_id)
            .unwrap()
            .unwrap();
        assert!(aggregate.terminal_state.is_none());
        assert_eq!(aggregate.active_attempt_id, Some(ownership.attempt_id));
    }

    #[test]
    fn ingress_replaces_active_snapshot_and_fences_stale_events() {
        let service = test_service(RecordingChannel::confirming());
        let ingress = service.ingress();
        let runtime_host_id = RuntimeHostId::new();
        let stale_session = RuntimeSessionId::new();
        let current_session = RuntimeSessionId::new();
        let stale_attempt = ownership();
        let current_attempt = ownership();
        seed_attempt(&service, &stale_attempt);
        seed_attempt(&service, &current_attempt);

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
            .unwrap()
            .is_none());
        assert_eq!(
            service
                .attempt_ownership(&current_attempt.attempt_id)
                .unwrap()
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
            .unwrap()
            .is_some());
        let version_before_stale = service
            .store
            .load(&current_attempt.execution_id)
            .unwrap()
            .unwrap()
            .execution_version;
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
        assert_eq!(
            service
                .store
                .load(&current_attempt.execution_id)
                .unwrap()
                .unwrap()
                .execution_version,
            version_before_stale
        );
    }

    #[test]
    fn concurrent_session_replacement_fences_in_flight_old_event() {
        let store = Arc::new(BlockingAssignStore {
            inner: crate::MemoryExecutionStateStore::default(),
            armed: AtomicBool::new(false),
            entered: Barrier::new(2),
            release: Barrier::new(2),
        });
        let service = RuntimeControlService::new(RecordingChannel::confirming(), store.clone());
        let ingress = service.ingress();
        let old = ownership();
        seed_attempt(&service, &old);
        ingress
            .register(registration(
                &old.runtime_host_id,
                &old.runtime_session_id,
                &old,
            ))
            .unwrap();
        store.armed.store(true, Ordering::SeqCst);

        let old_event = RuntimeControlEvent::AttemptStarted {
            protocol_version: RUNTIME_CONTROL_PROTOCOL_VERSION.into(),
            message_id: ControlMessageId::new(),
            runtime_host_id: old.runtime_host_id.clone(),
            runtime_session_id: old.runtime_session_id.clone(),
            execution_id: old.execution_id.clone(),
            attempt_id: old.attempt_id.clone(),
            attempt_number: old.attempt_number,
            worker_id: WorkerId::from("old-event-worker"),
        };
        let applying = {
            let ingress = ingress.clone();
            std::thread::spawn(move || ingress.apply(old_event))
        };
        store.entered.wait();

        let mut current = old.clone();
        current.runtime_session_id = RuntimeSessionId::new();
        current.worker_id = WorkerId::from("current-worker");
        let (registered_tx, registered_rx) = std::sync::mpsc::channel();
        let registering = {
            let ingress = ingress.clone();
            let registration = registration(
                &current.runtime_host_id,
                &current.runtime_session_id,
                &current,
            );
            std::thread::spawn(move || {
                let result = ingress.register(registration);
                registered_tx.send(result).unwrap();
            })
        };
        assert!(registered_rx
            .recv_timeout(Duration::from_millis(20))
            .is_err());
        store.release.wait();
        applying.join().unwrap().unwrap();
        registered_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        registering.join().unwrap();

        assert_eq!(
            service.attempt_ownership(&current.attempt_id).unwrap(),
            Some(current)
        );
    }

    #[test]
    fn drain_keeps_active_work_and_shutdown_honors_grace_before_termination() {
        let channel = RecordingChannel::confirming();
        let service = test_service(channel.clone());
        let ownership = ownership();
        register_test_attempt(&service, ownership.clone());

        service.drain().unwrap();
        assert_eq!(
            service.terminal_outcome(&ownership.execution_id).unwrap(),
            None
        );
        let started = Instant::now();
        service.shutdown(Duration::from_millis(35)).unwrap();

        assert!(started.elapsed() >= Duration::from_millis(30));
        assert_eq!(
            service.terminal_outcome(&ownership.execution_id).unwrap(),
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
            attempt_number: 1,
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

    fn test_service(channel: Arc<dyn RuntimeControlChannel>) -> RuntimeControlService {
        RuntimeControlService::new(
            channel,
            Arc::new(crate::MemoryExecutionStateStore::default()),
        )
    }

    fn register_test_attempt(service: &RuntimeControlService, ownership: AttemptOwnership) {
        seed_attempt(service, &ownership);
        service.register_attempt(ownership).unwrap();
    }

    fn seed_attempt(service: &RuntimeControlService, ownership: &AttemptOwnership) {
        let attempt = attempt(ownership);
        let request =
            InvocationRequest::with_attempt(json!({}), Default::default(), attempt.clone());
        service
            .store
            .create(crate::NewExecution {
                action: ActionDefinition {
                    runtime: RuntimeKind::Python,
                    kind: ActionKind::Api(ApiAction {
                        method: "POST".into(),
                        path: "/test".into(),
                        consumes: vec![],
                        produces: vec![],
                        request_schema: None,
                        response_schema: None,
                        query_params: vec![],
                        authorizer: None,
                    }),
                    source: "test.py".into(),
                    entrypoint: "run".into(),
                    name: Some("test".into()),
                    policy: Default::default(),
                },
                action_revision: "runtime-control-test-revision".into(),
                execution_scope_id: crate::ExecutionScopeId::new("test").unwrap(),
                action_id: "test".into(),
                trigger: crate::ExecutionTrigger::Unknown,
                creation_fingerprint: "runtime-control-test".into(),
                data_refs: crate::ExecutionDataReferences::default(),
                request,
                policy: crate::ExecutionPolicy {
                    timeout: Duration::from_secs(1),
                    retry: crate::RetryPolicy {
                        max_attempts: 1,
                        initial_delay: Duration::ZERO,
                        backoff: 1.0,
                    },
                },
                created_at: SystemTime::now(),
            })
            .unwrap();
        service
            .store
            .compare_and_set(
                &attempt.execution_id,
                0,
                ExecutionMutation::StartAttempt {
                    attempt: crate::AttemptRecord::pending(attempt.clone(), 1),
                },
            )
            .unwrap();
    }
}
