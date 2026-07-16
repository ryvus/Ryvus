use ryvus_protocol::ActionDefinition;

use ryvus_protocol::{
    AttemptOutcome, ControlCommandOutcome, ExecutionAttempt, ExecutionId, InvocationRequest,
};
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::SystemTime};

use crate::{
    action_revision, assign_attempt_deadline, execution_creation_fingerprint, AttemptRecord,
    CreateExecutionResult, ExecutionAggregate, ExecutionDataReferences, ExecutionMutation,
    ExecutionOptions, ExecutionPersistence, ExecutionRecord, ExecutionScopeId,
    ExecutionServiceError, ExecutionServiceResult, ExecutionState, ExecutionStateStore,
    ExecutionTrigger, Executor, ExecutorError, NewExecution, RecordingExecutor,
    RuntimeControlService, RuntimeResolver, StateStoreError, TerminalState, TransitionResult,
};

#[derive(Debug, Clone)]
pub struct ExecutionSubmission {
    pub scope: ExecutionScopeId,
    pub action_id: String,
    pub trigger: ExecutionTrigger,
    pub request: InvocationRequest,
    pub policy: ExecutionPolicy,
    pub data_refs: ExecutionDataReferences,
}

#[derive(Debug, Clone)]
pub enum ExecutionSubmissionResult {
    Executed(ExecutionRecord),
    Existing(ExecutionAggregate),
    AlreadyActive(ExecutionAggregate),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionPolicy {
    pub timeout: std::time::Duration,
    pub retry: RetryPolicy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub initial_delay: std::time::Duration,
    pub backoff: f64,
}

impl ExecutionPolicy {
    pub fn from_action_policy(
        policy: &ryvus_protocol::ActionExecutionPolicy,
    ) -> ExecutionServiceResult<Self> {
        if policy.retry.max_attempts == 0 {
            return Err(ExecutionServiceError::InvalidPolicy(
                "retry.max_attempts must be greater than 0".to_string(),
            ));
        }

        if !policy.retry.backoff.is_finite() || policy.retry.backoff <= 0.0 {
            return Err(ExecutionServiceError::InvalidPolicy(
                "retry.backoff must be finite and greater than 0".to_string(),
            ));
        }

        Ok(Self {
            timeout: parse_duration(&policy.timeout)?,
            retry: RetryPolicy {
                max_attempts: policy.retry.max_attempts,
                initial_delay: parse_duration(&policy.retry.initial_delay)?,
                backoff: policy.retry.backoff,
            },
        })
    }
}

fn parse_duration(value: &str) -> ExecutionServiceResult<std::time::Duration> {
    let (number, unit) = if let Some(number) = value.strip_suffix("ms") {
        (number, "ms")
    } else if let Some(number) = value.strip_suffix('s') {
        (number, "s")
    } else if let Some(number) = value.strip_suffix('m') {
        (number, "m")
    } else {
        return Err(ExecutionServiceError::InvalidPolicy(format!(
            "unsupported duration '{value}'"
        )));
    };

    let amount = number
        .parse::<u64>()
        .map_err(|_| ExecutionServiceError::InvalidPolicy(format!("invalid duration '{value}'")))?;

    if amount == 0 {
        return Err(ExecutionServiceError::InvalidPolicy(format!(
            "duration '{value}' must be greater than zero"
        )));
    }

    Ok(match unit {
        "ms" => std::time::Duration::from_millis(amount),
        "s" => std::time::Duration::from_secs(amount),
        "m" => std::time::Duration::from_secs(amount * 60),
        _ => unreachable!("unit is checked above"),
    })
}

pub struct ExecutionService<RR, E, EP> {
    resolver: RR,
    executor: RecordingExecutor<E>,
    persistence: EP,
    runtime_control: RuntimeControlService,
    store: Arc<dyn ExecutionStateStore>,
    default_scope: ExecutionScopeId,
}

impl<RR, E, EP> ExecutionService<RR, E, EP>
where
    RR: RuntimeResolver,
    E: Executor,
    EP: ExecutionPersistence,
{
    pub fn new(
        resolver: RR,
        executor: E,
        persistence: EP,
        runtime_control: RuntimeControlService,
        store: Arc<dyn ExecutionStateStore>,
    ) -> Self {
        Self::new_with_scope(
            resolver,
            executor,
            persistence,
            runtime_control,
            store,
            ExecutionScopeId::local_default(),
        )
    }

    pub fn new_with_scope(
        resolver: RR,
        executor: E,
        persistence: EP,
        runtime_control: RuntimeControlService,
        store: Arc<dyn ExecutionStateStore>,
        default_scope: ExecutionScopeId,
    ) -> Self {
        Self {
            resolver,
            executor: RecordingExecutor::new(executor),
            persistence,
            runtime_control,
            store,
            default_scope,
        }
    }

    pub fn execute(
        &self,
        action: &ActionDefinition,
        request: &InvocationRequest,
        policy: &ExecutionPolicy,
    ) -> ExecutionServiceResult<ExecutionRecord> {
        self.execute_triggered(action, request, policy, ExecutionTrigger::Api)
    }

    pub fn execute_triggered(
        &self,
        action: &ActionDefinition,
        request: &InvocationRequest,
        policy: &ExecutionPolicy,
        trigger: ExecutionTrigger,
    ) -> ExecutionServiceResult<ExecutionRecord> {
        let submission = ExecutionSubmission {
            scope: self.default_scope.clone(),
            action_id: action
                .name
                .clone()
                .unwrap_or_else(|| action.entrypoint.clone()),
            trigger,
            request: request.clone(),
            policy: policy.clone(),
            data_refs: ExecutionDataReferences::default(),
        };
        match self.execute_submission(action, submission)? {
            ExecutionSubmissionResult::Executed(record) => Ok(record),
            ExecutionSubmissionResult::Existing(aggregate)
            | ExecutionSubmissionResult::AlreadyActive(aggregate) => {
                Err(StateStoreError::IdentityConflict {
                    execution_id: aggregate.execution_id,
                }
                .into())
            }
        }
    }

    pub fn execute_submission(
        &self,
        action: &ActionDefinition,
        submission: ExecutionSubmission,
    ) -> ExecutionServiceResult<ExecutionSubmissionResult> {
        let request = &submission.request;
        let policy = &submission.policy;
        if request.attempt_number != 1 {
            return Err(ExecutionServiceError::InvalidInitialAttempt {
                attempt_number: request.attempt_number,
            });
        }

        let execution_id = request.execution_id.clone();
        let action_revision = action_revision(action)?;
        let creation_fingerprint = execution_creation_fingerprint(
            &submission.scope,
            &submission.action_id,
            &action_revision,
            &submission.trigger,
            request,
            policy,
            &submission.data_refs,
        )?;
        let created = self.store.create_idempotent(NewExecution {
            action: action.clone(),
            action_revision,
            execution_scope_id: submission.scope,
            action_id: submission.action_id,
            trigger: submission.trigger,
            creation_fingerprint,
            data_refs: submission.data_refs,
            request: submission.request,
            policy: submission.policy,
            created_at: SystemTime::now(),
        })?;
        let mut aggregate = match created {
            CreateExecutionResult::Created(aggregate) => aggregate,
            CreateExecutionResult::Existing(aggregate) if aggregate.terminal_state.is_some() => {
                return Ok(ExecutionSubmissionResult::Existing(aggregate));
            }
            CreateExecutionResult::Existing(aggregate)
                if matches!(
                    aggregate.state,
                    ExecutionState::Running | ExecutionState::CancellationRequested
                ) =>
            {
                return Ok(ExecutionSubmissionResult::AlreadyActive(aggregate));
            }
            CreateExecutionResult::Existing(aggregate) => aggregate,
        };
        let action = aggregate.action.clone();
        let policy = aggregate.policy.clone();
        let request = aggregate.request.clone();
        let target = match self.resolver.resolve(&action) {
            Ok(target) => target,
            Err(error) => {
                self.finish_without_attempt(&mut aggregate, ExecutionState::Failed)?;
                return Err(error.into());
            }
        };
        let mut delay = policy.retry.initial_delay;
        let mut attempt_request = request.clone();
        assign_attempt_deadline(&mut attempt_request, policy.timeout)?;

        loop {
            let attempt = attempt_request.attempt();
            match self.start_attempt(&mut aggregate, &attempt_request)? {
                true => {}
                false => {
                    self.finish_without_attempt(&mut aggregate, ExecutionState::Cancelled)?;
                    return Err(ExecutionServiceError::CancellationRequested { execution_id });
                }
            }
            let record = match self.executor.invoke_recorded(
                &target,
                &attempt_request,
                &ExecutionOptions {
                    timeout: policy.timeout,
                },
            ) {
                Ok(record) => record,
                Err(error) => {
                    let outcome = if matches!(error, ExecutorError::RuntimeCancelled { .. }) {
                        AttemptOutcome::Cancelled
                    } else if matches!(
                        error,
                        ExecutorError::ProcessTimedOut { .. }
                            | ExecutorError::RuntimeTimedOut { .. }
                    ) {
                        AttemptOutcome::TimedOut
                    } else {
                        AttemptOutcome::InfrastructureFailed
                    };
                    let winner = self.finish_terminal(&mut aggregate, &attempt, outcome, None)?;
                    if winner == AttemptOutcome::Cancelled && outcome != AttemptOutcome::Cancelled {
                        return Err(ExecutorError::RuntimeCancelled { attempt }.into());
                    }
                    if winner == AttemptOutcome::TimedOut && outcome != AttemptOutcome::TimedOut {
                        return Err(ExecutorError::RuntimeTimedOut { attempt }.into());
                    }
                    return Err(error.into());
                }
            };

            if let Err(error) = self.persistence.save_execution(&record) {
                self.finish_terminal(
                    &mut aggregate,
                    &attempt,
                    AttemptOutcome::InfrastructureFailed,
                    Some(record.result.clone()),
                )?;
                return Err(error.into());
            }

            aggregate = self.reload(&execution_id)?;
            if let Some(terminal) = &aggregate.terminal_state {
                match terminal.state {
                    ExecutionState::Cancelled => {
                        return Err(ExecutorError::RuntimeCancelled { attempt }.into())
                    }
                    ExecutionState::TimedOut => {
                        return Err(ExecutorError::RuntimeTimedOut { attempt }.into())
                    }
                    _ => {}
                }
            }

            let result = &record.result.invocation_result;
            if result.status == ryvus_protocol::InvocationStatus::Success {
                let winner = self.finish_terminal(
                    &mut aggregate,
                    &attempt,
                    AttemptOutcome::Succeeded,
                    Some(record.result.clone()),
                )?;
                if winner == AttemptOutcome::Cancelled {
                    return Err(ExecutorError::RuntimeCancelled { attempt }.into());
                }
                if winner == AttemptOutcome::TimedOut {
                    return Err(ExecutorError::RuntimeTimedOut { attempt }.into());
                }
                return Ok(ExecutionSubmissionResult::Executed(record));
            }

            let retryable = result.error.as_ref().is_some_and(|error| error.retryable);
            let attempts_remain = attempt.attempt_number < policy.retry.max_attempts;
            if !retryable || !attempts_remain {
                let winner = self.finish_terminal(
                    &mut aggregate,
                    &attempt,
                    AttemptOutcome::Failed,
                    Some(record.result.clone()),
                )?;
                if winner == AttemptOutcome::Cancelled {
                    return Err(ExecutorError::RuntimeCancelled { attempt }.into());
                }
                if winner == AttemptOutcome::TimedOut {
                    return Err(ExecutorError::RuntimeTimedOut { attempt }.into());
                }
                return Ok(ExecutionSubmissionResult::Executed(record));
            }

            let mut retry_request = attempt_request.retry();
            assign_attempt_deadline(&mut retry_request, policy.timeout)?;
            let delay_ms =
                i64::try_from(delay.as_millis()).map_err(|_| ExecutorError::DeadlineOutOfRange)?;
            retry_request.deadline_unix_ms = retry_request
                .deadline_unix_ms
                .checked_add(delay_ms)
                .ok_or(ExecutorError::DeadlineOutOfRange)?;
            if !self.finish_with_retry(&mut aggregate, &attempt, &record, &retry_request)? {
                if aggregate
                    .terminal_state
                    .as_ref()
                    .is_some_and(|terminal| terminal.state == ExecutionState::Cancelled)
                {
                    return Err(ExecutorError::RuntimeCancelled { attempt }.into());
                }
                if aggregate
                    .terminal_state
                    .as_ref()
                    .is_some_and(|terminal| terminal.state == ExecutionState::TimedOut)
                {
                    return Err(ExecutorError::RuntimeTimedOut { attempt }.into());
                }
                return Ok(ExecutionSubmissionResult::Executed(record));
            }
            std::thread::sleep(delay);
            delay = delay.mul_f64(policy.retry.backoff);
            attempt_request = retry_request;

            if attempt_request.attempt_number > policy.retry.max_attempts {
                unreachable!("retry should only be created while attempts remain");
            }
        }
    }

    pub fn execute_event(
        &self,
        action: &ActionDefinition,
        event: serde_json::Value,
    ) -> ExecutionServiceResult<ExecutionRecord> {
        let request = InvocationRequest::new(event);
        let policy = ExecutionPolicy::from_action_policy(&action.policy)?;
        self.execute(action, &request, &policy)
    }

    pub fn cancel(&self, execution_id: &ExecutionId) -> ExecutionServiceResult<bool> {
        match self.runtime_control.cancel(execution_id)? {
            ControlCommandOutcome::Confirmed => Ok(true),
            ControlCommandOutcome::AlreadyTerminal => {
                Ok(self.runtime_control.terminal_outcome(execution_id)?
                    == Some(AttemptOutcome::Cancelled))
            }
            ControlCommandOutcome::AttemptNotFound
            | ControlCommandOutcome::OwnershipMismatch
            | ControlCommandOutcome::StaleSession
            | ControlCommandOutcome::Unsupported
            | ControlCommandOutcome::Failed => Ok(false),
        }
    }

    pub fn execution_state(
        &self,
        execution_id: &ExecutionId,
    ) -> ExecutionServiceResult<Option<ExecutionState>> {
        Ok(self
            .store
            .load(execution_id)?
            .map(|aggregate| aggregate.state))
    }

    pub fn shutdown(&self, grace: std::time::Duration) -> ExecutionServiceResult<()> {
        self.runtime_control.shutdown(grace)?;
        self.executor.shutdown(grace).map_err(Into::into)
    }

    fn reload(
        &self,
        execution_id: &ExecutionId,
    ) -> ExecutionServiceResult<crate::ExecutionAggregate> {
        self.store.load(execution_id)?.ok_or_else(|| {
            StateStoreError::NotFound {
                execution_id: execution_id.clone(),
            }
            .into()
        })
    }

    fn start_attempt(
        &self,
        aggregate: &mut crate::ExecutionAggregate,
        request: &InvocationRequest,
    ) -> ExecutionServiceResult<bool> {
        loop {
            if aggregate.terminal_state.is_some() || aggregate.cancellation_intent.is_some() {
                return Ok(false);
            }
            match self.store.compare_and_set(
                &aggregate.execution_id,
                aggregate.execution_version,
                ExecutionMutation::StartAttempt {
                    attempt: AttemptRecord::pending(request.attempt(), request.deadline_unix_ms),
                },
            )? {
                TransitionResult::Applied { aggregate: current } => {
                    *aggregate = current;
                    return Ok(true);
                }
                TransitionResult::Unchanged { aggregate: current } => {
                    *aggregate = current;
                    return Ok(true);
                }
                TransitionResult::Conflict { .. } => {
                    *aggregate = self.reload(&aggregate.execution_id)?
                }
            }
        }
    }

    fn finish_terminal(
        &self,
        aggregate: &mut crate::ExecutionAggregate,
        attempt: &ExecutionAttempt,
        outcome: AttemptOutcome,
        result: Option<crate::ExecutionResult>,
    ) -> ExecutionServiceResult<AttemptOutcome> {
        loop {
            if let Some(terminal) = &aggregate.terminal_state {
                return Ok(state_outcome(terminal.state));
            }
            match self.store.compare_and_set(
                &aggregate.execution_id,
                aggregate.execution_version,
                ExecutionMutation::FinishAttempt {
                    attempt_id: attempt.attempt_id.clone(),
                    outcome,
                    result: result.clone(),
                    retry: None,
                    terminal: Some(TerminalState::new(
                        outcome_state(outcome),
                        Some(attempt.attempt_id.clone()),
                    )),
                },
            )? {
                TransitionResult::Applied { aggregate: current }
                | TransitionResult::Unchanged { aggregate: current } => {
                    *aggregate = current;
                    return Ok(aggregate
                        .terminal_state
                        .as_ref()
                        .map(|terminal| state_outcome(terminal.state))
                        .unwrap_or(outcome));
                }
                TransitionResult::Conflict { .. } => {
                    *aggregate = self.reload(&aggregate.execution_id)?
                }
            }
        }
    }

    fn finish_with_retry(
        &self,
        aggregate: &mut crate::ExecutionAggregate,
        attempt: &ExecutionAttempt,
        record: &ExecutionRecord,
        retry_request: &InvocationRequest,
    ) -> ExecutionServiceResult<bool> {
        loop {
            if aggregate.terminal_state.is_some() {
                return Ok(false);
            }
            if aggregate.cancellation_intent.is_some() {
                self.finish_terminal(
                    aggregate,
                    attempt,
                    AttemptOutcome::Failed,
                    Some(record.result.clone()),
                )?;
                return Ok(false);
            }
            match self.store.compare_and_set(
                &aggregate.execution_id,
                aggregate.execution_version,
                ExecutionMutation::FinishAttempt {
                    attempt_id: attempt.attempt_id.clone(),
                    outcome: AttemptOutcome::Failed,
                    result: Some(record.result.clone()),
                    retry: Some(AttemptRecord::pending(
                        retry_request.attempt(),
                        retry_request.deadline_unix_ms,
                    )),
                    terminal: None,
                },
            )? {
                TransitionResult::Applied { aggregate: current }
                | TransitionResult::Unchanged { aggregate: current } => {
                    *aggregate = current;
                    return Ok(true);
                }
                TransitionResult::Conflict { .. } => {
                    *aggregate = self.reload(&aggregate.execution_id)?
                }
            }
        }
    }

    fn finish_without_attempt(
        &self,
        aggregate: &mut crate::ExecutionAggregate,
        state: ExecutionState,
    ) -> ExecutionServiceResult<()> {
        loop {
            if aggregate.terminal_state.is_some() {
                return Ok(());
            }
            match self.store.compare_and_set(
                &aggregate.execution_id,
                aggregate.execution_version,
                ExecutionMutation::FinishExecution {
                    terminal: TerminalState::new(state, None),
                },
            )? {
                TransitionResult::Applied { aggregate: current }
                | TransitionResult::Unchanged { aggregate: current } => {
                    *aggregate = current;
                    return Ok(());
                }
                TransitionResult::Conflict { .. } => {
                    *aggregate = self.reload(&aggregate.execution_id)?
                }
            }
        }
    }
}

fn outcome_state(outcome: AttemptOutcome) -> ExecutionState {
    match outcome {
        AttemptOutcome::Succeeded => ExecutionState::Succeeded,
        AttemptOutcome::Failed | AttemptOutcome::InfrastructureFailed => ExecutionState::Failed,
        AttemptOutcome::Cancelled => ExecutionState::Cancelled,
        AttemptOutcome::TimedOut => ExecutionState::TimedOut,
    }
}

fn state_outcome(state: ExecutionState) -> AttemptOutcome {
    match state {
        ExecutionState::Succeeded => AttemptOutcome::Succeeded,
        ExecutionState::Cancelled => AttemptOutcome::Cancelled,
        ExecutionState::TimedOut => AttemptOutcome::TimedOut,
        _ => AttemptOutcome::Failed,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };

    use ryvus_protocol::{
        ActionDefinition, ActionKind, ApiAction, ExecutionAttempt, InvocationError,
        InvocationResult, InvocationStatus, RuntimeKind,
    };
    use serde_json::json;

    use crate::{ExecutionPersistence, ExecutionResult, Executor, RuntimeTarget};

    use super::*;

    #[test]
    fn parses_policy_duration_units() {
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("2m").unwrap(), Duration::from_secs(120));
    }

    #[test]
    fn rejects_invalid_policy_values() {
        assert!(parse_duration("0s").is_err());
        assert!(parse_duration("1h").is_err());

        let policy = ryvus_protocol::ActionExecutionPolicy {
            timeout: "3s".to_string(),
            retry: ryvus_protocol::ActionRetryPolicy {
                max_attempts: 0,
                initial_delay: "1s".to_string(),
                backoff: 2.0,
            },
        };

        assert!(ExecutionPolicy::from_action_policy(&policy).is_err());
    }

    #[test]
    fn store_failure_prevents_dispatch() {
        let executor = FailsThenSucceeds::default();
        let attempts = Arc::clone(&executor.attempts);
        let mut store = crate::MockExecutionStateStore::new();
        store
            .expect_create_idempotent()
            .once()
            .returning(|_| Err(StateStoreError::LockPoisoned));
        let store: Arc<dyn ExecutionStateStore> = Arc::new(store);
        let service = ExecutionService::new(
            StaticResolver,
            executor,
            NoopPersistence,
            test_runtime_control(store.clone()),
            store,
        );
        let request = InvocationRequest::new(json!({}));
        let policy = ExecutionPolicy {
            timeout: Duration::from_secs(1),
            retry: RetryPolicy {
                max_attempts: 1,
                initial_delay: Duration::ZERO,
                backoff: 1.0,
            },
        };

        assert!(matches!(
            service.execute(&test_action(), &request, &policy),
            Err(ExecutionServiceError::StateStore(
                StateStoreError::LockPoisoned
            ))
        ));
        assert!(attempts.lock().unwrap().is_empty());
    }

    #[test]
    fn execution_state_propagates_store_failure() {
        let mut store = crate::MockExecutionStateStore::new();
        store
            .expect_load()
            .once()
            .returning(|_| Err(StateStoreError::Backend("unavailable".into())));
        let store: Arc<dyn ExecutionStateStore> = Arc::new(store);
        let service = ExecutionService::new(
            StaticResolver,
            FailsThenSucceeds::default(),
            NoopPersistence,
            test_runtime_control(store.clone()),
            store,
        );

        assert!(matches!(
            service.execution_state(&ExecutionId::new()),
            Err(ExecutionServiceError::StateStore(StateStoreError::Backend(
                message
            ))) if message == "unavailable"
        ));
    }

    #[test]
    fn store_failure_while_recording_resolution_failure_is_propagated() {
        let request = InvocationRequest::new(json!({}));
        let policy = ExecutionPolicy {
            timeout: Duration::from_secs(1),
            retry: RetryPolicy {
                max_attempts: 1,
                initial_delay: Duration::ZERO,
                backoff: 1.0,
            },
        };
        let memory = crate::MemoryExecutionStateStore::default();
        let created = memory
            .create(NewExecution {
                action: test_action(),
                action_revision: "test-action-revision".into(),
                execution_scope_id: crate::ExecutionScopeId::new("test").unwrap(),
                action_id: "test".into(),
                trigger: crate::ExecutionTrigger::Unknown,
                creation_fingerprint: "test-fingerprint".into(),
                data_refs: crate::ExecutionDataReferences::default(),
                request: request.clone(),
                policy: policy.clone(),
                created_at: SystemTime::now(),
            })
            .unwrap();
        let mut store = crate::MockExecutionStateStore::new();
        store
            .expect_create_idempotent()
            .once()
            .return_once(move |_| Ok(crate::CreateExecutionResult::Created(created)));
        store
            .expect_compare_and_set()
            .once()
            .returning(|_, _, _| Err(StateStoreError::LockPoisoned));
        let store: Arc<dyn ExecutionStateStore> = Arc::new(store);
        let service = ExecutionService::new(
            FailingResolver,
            FailsThenSucceeds::default(),
            NoopPersistence,
            test_runtime_control(store.clone()),
            store,
        );

        assert!(matches!(
            service.execute(&test_action(), &request, &policy),
            Err(ExecutionServiceError::StateStore(
                StateStoreError::LockPoisoned
            ))
        ));
    }

    #[test]
    fn execution_and_running_attempt_exist_before_dispatch() {
        let store = Arc::new(crate::MemoryExecutionStateStore::default());
        let observed = Arc::new(Mutex::new(false));
        let executor = StoreInspectingSuccess {
            store: store.clone(),
            observed: observed.clone(),
        };
        let service = ExecutionService::new(
            StaticResolver,
            executor,
            NoopPersistence,
            test_runtime_control(store.clone()),
            store,
        );
        let request = InvocationRequest::new(json!({}));
        let policy = ExecutionPolicy {
            timeout: Duration::from_secs(1),
            retry: RetryPolicy {
                max_attempts: 1,
                initial_delay: Duration::ZERO,
                backoff: 1.0,
            },
        };

        service.execute(&test_action(), &request, &policy).unwrap();
        assert!(*observed.lock().unwrap());
    }

    #[test]
    fn cancellation_intent_during_retryable_result_finishes_active_attempt() {
        let store = Arc::new(crate::MemoryExecutionStateStore::default());
        let executor = CancellingFailure {
            store: store.clone(),
            invocations: Arc::new(Mutex::new(0)),
        };
        let invocations = executor.invocations.clone();
        let service = ExecutionService::new(
            StaticResolver,
            executor,
            NoopPersistence,
            test_runtime_control(store.clone()),
            store.clone(),
        );
        let request = InvocationRequest::new(json!({}));
        let policy = ExecutionPolicy {
            timeout: Duration::from_secs(1),
            retry: RetryPolicy {
                max_attempts: 2,
                initial_delay: Duration::ZERO,
                backoff: 1.0,
            },
        };

        let record = service.execute(&test_action(), &request, &policy).unwrap();
        assert_eq!(
            record.result.invocation_result.status,
            InvocationStatus::Failed
        );
        assert_eq!(*invocations.lock().unwrap(), 1);
        let aggregate = store.load(&request.execution_id).unwrap().unwrap();
        assert_eq!(aggregate.state, ExecutionState::Failed);
        assert!(aggregate.active_attempt_id.is_none());
        assert_eq!(aggregate.attempts.len(), 1);
    }

    #[test]
    fn cancellation_before_dispatch_becomes_terminal_without_an_owner() {
        let store = Arc::new(crate::MemoryExecutionStateStore::default());
        let executor = FailsThenSucceeds::default();
        let attempts = executor.attempts.clone();
        let request = InvocationRequest::new(json!({}));
        let service = ExecutionService::new(
            CancellingResolver {
                store: store.clone(),
                execution_id: request.execution_id.clone(),
            },
            executor,
            NoopPersistence,
            test_runtime_control(store.clone()),
            store.clone(),
        );
        let policy = ExecutionPolicy {
            timeout: Duration::from_secs(1),
            retry: RetryPolicy {
                max_attempts: 1,
                initial_delay: Duration::ZERO,
                backoff: 1.0,
            },
        };

        assert!(matches!(
            service.execute(&test_action(), &request, &policy),
            Err(ExecutionServiceError::CancellationRequested { .. })
        ));
        assert!(attempts.lock().unwrap().is_empty());
        let aggregate = store.load(&request.execution_id).unwrap().unwrap();
        assert_eq!(aggregate.state, ExecutionState::Cancelled);
        assert!(aggregate.terminal_state.is_some());
        assert!(aggregate.active_attempt_id.is_none());
        assert!(aggregate.attempts.is_empty());
    }

    #[test]
    fn retries_until_success() {
        let executor = FailsThenSucceeds::default();
        let attempts = Arc::clone(&executor.attempts);
        let deadlines = Arc::clone(&executor.deadlines);
        let persistence = RecordingPersistence::default();
        let persisted_attempts = Arc::clone(&persistence.attempts);
        let store = Arc::new(crate::MemoryExecutionStateStore::default());
        let service = ExecutionService::new(
            StaticResolver,
            executor,
            persistence,
            test_runtime_control(store.clone()),
            store.clone(),
        );
        let action = test_action();
        let request = InvocationRequest::new(json!({}));
        let policy = ExecutionPolicy {
            timeout: Duration::from_secs(3),
            retry: RetryPolicy {
                max_attempts: 2,
                initial_delay: Duration::from_millis(30),
                backoff: 1.0,
            },
        };

        let record = service.execute(&action, &request, &policy).unwrap();

        assert_eq!(
            record.result.invocation_result.status,
            InvocationStatus::Success
        );
        assert_eq!(
            service.execution_state(&request.execution_id).unwrap(),
            Some(ExecutionState::Succeeded)
        );
        let attempts = attempts.lock().unwrap();
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].attempt_number, 1);
        assert_eq!(attempts[1].attempt_number, 2);
        assert_eq!(attempts[0].execution_id, attempts[1].execution_id);
        assert_ne!(attempts[0].attempt_id, attempts[1].attempt_id);
        assert_eq!(*attempts, *persisted_attempts.lock().unwrap());
        let deadlines = deadlines.lock().unwrap();
        assert_eq!(deadlines.len(), 2);
        assert!(deadlines
            .iter()
            .all(|(deadline, budget)| *deadline > 0 && *budget == 3_000));
        let aggregate = store.load(&request.execution_id).unwrap().unwrap();
        assert_eq!(aggregate.attempts[0].deadline_unix_ms, deadlines[0].0);
        assert_eq!(aggregate.attempts[1].deadline_unix_ms, deadlines[1].0);
    }

    #[test]
    fn non_retryable_handler_failure_stops_after_one_attempt() {
        let executor = NonRetryableFailure::default();
        let attempts = Arc::clone(&executor.attempts);
        let store = Arc::new(crate::MemoryExecutionStateStore::default());
        let service = ExecutionService::new(
            StaticResolver,
            executor,
            NoopPersistence,
            test_runtime_control(store.clone()),
            store,
        );
        let request = InvocationRequest::new(json!({}));
        let policy = ExecutionPolicy {
            timeout: Duration::from_secs(3),
            retry: RetryPolicy {
                max_attempts: 3,
                initial_delay: Duration::ZERO,
                backoff: 1.0,
            },
        };

        let record = service.execute(&test_action(), &request, &policy).unwrap();

        assert_eq!(
            record.result.invocation_result.status,
            InvocationStatus::Failed
        );
        assert_eq!(attempts.lock().unwrap().len(), 1);
    }

    #[test]
    fn timeout_and_cancellation_have_distinct_terminal_states() {
        let action = test_action();
        let policy = ExecutionPolicy {
            timeout: Duration::from_millis(10),
            retry: RetryPolicy {
                max_attempts: 1,
                initial_delay: Duration::ZERO,
                backoff: 1.0,
            },
        };

        let store = Arc::new(crate::MemoryExecutionStateStore::default());
        let timed_out = ExecutionService::new(
            StaticResolver,
            TimedOutExecutor,
            NoopPersistence,
            test_runtime_control(store.clone()),
            store,
        );
        let timeout_request = InvocationRequest::new(json!({}));
        assert!(timed_out
            .execute(&action, &timeout_request, &policy)
            .is_err());
        assert_eq!(
            timed_out
                .execution_state(&timeout_request.execution_id)
                .unwrap(),
            Some(ExecutionState::TimedOut)
        );
    }

    #[test]
    fn accepted_timeout_winner_is_returned_over_late_success() {
        let store = Arc::new(crate::MemoryExecutionStateStore::default());
        let service = ExecutionService::new(
            StaticResolver,
            TimeoutWinningSuccess {
                store: store.clone(),
            },
            NoopPersistence,
            test_runtime_control(store.clone()),
            store,
        );
        let request = InvocationRequest::new(json!({}));
        let policy = ExecutionPolicy {
            timeout: Duration::from_secs(1),
            retry: RetryPolicy {
                max_attempts: 1,
                initial_delay: Duration::ZERO,
                backoff: 1.0,
            },
        };

        assert!(matches!(
            service.execute(&test_action(), &request, &policy),
            Err(ExecutionServiceError::Executor(
                ExecutorError::RuntimeTimedOut { .. }
            ))
        ));
        assert_eq!(
            service.execution_state(&request.execution_id).unwrap(),
            Some(ExecutionState::TimedOut)
        );
    }

    #[derive(Clone)]
    struct StaticResolver;

    #[derive(Clone)]
    struct FailingResolver;

    #[derive(Clone)]
    struct CancellingResolver {
        store: Arc<crate::MemoryExecutionStateStore>,
        execution_id: ExecutionId,
    }

    impl RuntimeResolver for CancellingResolver {
        fn resolve(&self, _action: &ActionDefinition) -> crate::ExecutorResult<RuntimeTarget> {
            let execution = self
                .store
                .load(&self.execution_id)
                .unwrap()
                .expect("execution should be created before resolution");
            self.store
                .compare_and_set(
                    &execution.execution_id,
                    execution.execution_version,
                    ExecutionMutation::RequestCancellation {
                        requested_at: SystemTime::now(),
                    },
                )
                .unwrap();
            Ok(RuntimeTarget::http("http://runtime.test"))
        }
    }

    impl RuntimeResolver for FailingResolver {
        fn resolve(&self, _action: &ActionDefinition) -> crate::ExecutorResult<RuntimeTarget> {
            Err(ExecutorError::UnsupportedRuntimeTarget {
                target: "test".into(),
            })
        }
    }

    impl RuntimeResolver for StaticResolver {
        fn resolve(&self, _action: &ActionDefinition) -> crate::ExecutorResult<RuntimeTarget> {
            Ok(RuntimeTarget::http("http://runtime.test"))
        }
    }

    struct NoopPersistence;

    impl ExecutionPersistence for NoopPersistence {
        fn save_execution(
            &self,
            _record: &ExecutionRecord,
        ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct RecordingPersistence {
        attempts: Arc<Mutex<Vec<ExecutionAttempt>>>,
    }

    impl ExecutionPersistence for RecordingPersistence {
        fn save_execution(
            &self,
            record: &ExecutionRecord,
        ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            self.attempts.lock().unwrap().push(record.attempt.clone());
            Ok(())
        }
    }

    #[derive(Default)]
    struct FailsThenSucceeds {
        attempts: Arc<Mutex<Vec<ExecutionAttempt>>>,
        deadlines: Arc<Mutex<Vec<(i64, u64)>>>,
    }

    impl Executor for FailsThenSucceeds {
        fn invoke(
            &self,
            _target: &RuntimeTarget,
            request: &InvocationRequest,
            _options: &ExecutionOptions,
        ) -> crate::ExecutorResult<ExecutionResult> {
            let mut attempts = self.attempts.lock().unwrap();
            attempts.push(request.attempt());
            self.deadlines
                .lock()
                .unwrap()
                .push((request.deadline_unix_ms, request.remaining_budget_ms));
            let invocation_result = if attempts.len() == 1 {
                InvocationResult::failed(
                    request,
                    InvocationError::new("retryable", "try again", true),
                )
            } else {
                InvocationResult::success(request, json!({ "attempt": attempts.len() }))
            };

            Ok(ExecutionResult {
                invocation_result,
                events: Vec::new(),
                stdout: String::new(),
                stderr: String::new(),
                duration: Duration::from_millis(1),
                exit_code: Some(0),
            })
        }
    }

    #[derive(Default)]
    struct NonRetryableFailure {
        attempts: Arc<Mutex<Vec<ExecutionAttempt>>>,
    }

    impl Executor for NonRetryableFailure {
        fn invoke(
            &self,
            _target: &RuntimeTarget,
            request: &InvocationRequest,
            _options: &ExecutionOptions,
        ) -> crate::ExecutorResult<ExecutionResult> {
            self.attempts.lock().unwrap().push(request.attempt());
            Ok(ExecutionResult {
                invocation_result: InvocationResult::failed(
                    request,
                    InvocationError::new("invalid", "do not retry", false),
                ),
                events: Vec::new(),
                stdout: String::new(),
                stderr: String::new(),
                duration: Duration::from_millis(1),
                exit_code: Some(0),
            })
        }
    }

    struct TimedOutExecutor;

    struct TimeoutWinningSuccess {
        store: Arc<crate::MemoryExecutionStateStore>,
    }

    impl Executor for TimeoutWinningSuccess {
        fn invoke(
            &self,
            _target: &RuntimeTarget,
            request: &InvocationRequest,
            _options: &ExecutionOptions,
        ) -> crate::ExecutorResult<ExecutionResult> {
            let aggregate = self.store.load(&request.execution_id).unwrap().unwrap();
            self.store
                .compare_and_set(
                    &request.execution_id,
                    aggregate.execution_version,
                    ExecutionMutation::FinishAttempt {
                        attempt_id: request.attempt_id.clone(),
                        outcome: AttemptOutcome::TimedOut,
                        result: None,
                        retry: None,
                        terminal: Some(TerminalState::new(
                            ExecutionState::TimedOut,
                            Some(request.attempt_id.clone()),
                        )),
                    },
                )
                .unwrap();
            Ok(ExecutionResult {
                invocation_result: InvocationResult::success(request, json!({})),
                events: vec![],
                stdout: String::new(),
                stderr: String::new(),
                duration: Duration::ZERO,
                exit_code: Some(0),
            })
        }
    }

    struct CancellingFailure {
        store: Arc<crate::MemoryExecutionStateStore>,
        invocations: Arc<Mutex<u32>>,
    }

    struct StoreInspectingSuccess {
        store: Arc<crate::MemoryExecutionStateStore>,
        observed: Arc<Mutex<bool>>,
    }

    impl Executor for StoreInspectingSuccess {
        fn invoke(
            &self,
            _target: &RuntimeTarget,
            request: &InvocationRequest,
            _options: &ExecutionOptions,
        ) -> crate::ExecutorResult<ExecutionResult> {
            let aggregate = self.store.load(&request.execution_id).unwrap().unwrap();
            *self.observed.lock().unwrap() = aggregate.state == ExecutionState::Running
                && aggregate.active_attempt_id.as_ref() == Some(&request.attempt_id)
                && aggregate.attempts.len() == 1;
            Ok(ExecutionResult {
                invocation_result: InvocationResult::success(request, json!({})),
                events: vec![],
                stdout: String::new(),
                stderr: String::new(),
                duration: Duration::ZERO,
                exit_code: Some(0),
            })
        }
    }

    impl Executor for CancellingFailure {
        fn invoke(
            &self,
            _target: &RuntimeTarget,
            request: &InvocationRequest,
            _options: &ExecutionOptions,
        ) -> crate::ExecutorResult<ExecutionResult> {
            *self.invocations.lock().unwrap() += 1;
            let aggregate = self.store.load(&request.execution_id).unwrap().unwrap();
            self.store
                .compare_and_set(
                    &request.execution_id,
                    aggregate.execution_version,
                    ExecutionMutation::RequestCancellation {
                        requested_at: SystemTime::now(),
                    },
                )
                .unwrap();
            Ok(ExecutionResult {
                invocation_result: InvocationResult::failed(
                    request,
                    InvocationError::new("retryable", "try again", true),
                ),
                events: vec![],
                stdout: String::new(),
                stderr: String::new(),
                duration: Duration::ZERO,
                exit_code: Some(1),
            })
        }
    }

    impl Executor for TimedOutExecutor {
        fn invoke(
            &self,
            _target: &RuntimeTarget,
            request: &InvocationRequest,
            _options: &ExecutionOptions,
        ) -> crate::ExecutorResult<ExecutionResult> {
            Err(ExecutorError::RuntimeTimedOut {
                attempt: request.attempt(),
            })
        }
    }

    fn test_action() -> ActionDefinition {
        ActionDefinition {
            runtime: RuntimeKind::Python,
            kind: ActionKind::Api(ApiAction {
                method: "POST".to_string(),
                path: "/test".to_string(),
                consumes: vec!["application/json".to_string()],
                produces: vec!["application/json".to_string()],
                request_schema: None,
                response_schema: None,
                query_params: Vec::new(),
                authorizer: None,
            }),
            source: "src/test.py".into(),
            entrypoint: "test".to_string(),
            name: Some("test".to_string()),
            policy: ryvus_protocol::ActionExecutionPolicy::default(),
        }
    }

    fn test_runtime_control(store: Arc<dyn ExecutionStateStore>) -> RuntimeControlService {
        RuntimeControlService::new(
            Arc::new(crate::InMemoryRuntimeControlChannel::default()),
            store,
        )
    }
}
