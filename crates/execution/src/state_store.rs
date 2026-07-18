use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
    time::SystemTime,
};

use ryvus_protocol::{
    ActionDefinition, AttemptId, AttemptOutcome, ExecutionAttempt, ExecutionId, InvocationEvent,
    InvocationRequest,
};
use serde::Serialize;
use thiserror::Error;

use crate::{
    AttemptOwnership, ExecutionDataReferences, ExecutionPolicy, ExecutionResult, ExecutionScopeId,
    ExecutionState, ExecutionTrigger,
};

#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct ExecutionAggregate {
    pub execution_id: ExecutionId,
    pub action: ActionDefinition,
    pub action_revision: String,
    pub execution_scope_id: ExecutionScopeId,
    pub action_id: String,
    pub trigger: ExecutionTrigger,
    pub creation_fingerprint: String,
    pub data_refs: ExecutionDataReferences,
    pub request: InvocationRequest,
    pub policy: ExecutionPolicy,
    pub state: ExecutionState,
    pub active_attempt_id: Option<AttemptId>,
    pub attempts: Vec<AttemptRecord>,
    pub cancellation_intent: Option<CancellationIntent>,
    pub terminal_state: Option<TerminalState>,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
    pub execution_version: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct AttemptRecord {
    pub attempt: ExecutionAttempt,
    pub deadline_unix_ms: i64,
    pub state: ExecutionState,
    pub ownership: Option<AttemptOwnership>,
    pub outcome: Option<AttemptOutcome>,
    pub result: Option<ExecutionResult>,
    pub data_refs: ExecutionDataReferences,
    pub started_at: Option<SystemTime>,
    pub finished_at: Option<SystemTime>,
}

impl AttemptRecord {
    pub fn pending(attempt: ExecutionAttempt, deadline_unix_ms: i64) -> Self {
        Self {
            attempt,
            deadline_unix_ms,
            state: ExecutionState::Pending,
            ownership: None,
            outcome: None,
            result: None,
            data_refs: ExecutionDataReferences::default(),
            started_at: None,
            finished_at: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct CancellationIntent {
    pub execution_id: ExecutionId,
    pub requested_at: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct TerminalState {
    pub state: ExecutionState,
    pub attempt_id: Option<AttemptId>,
    pub accepted_at: SystemTime,
}

impl TerminalState {
    pub fn new(state: ExecutionState, attempt_id: Option<AttemptId>) -> Self {
        Self {
            state,
            attempt_id,
            accepted_at: SystemTime::now(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewExecution {
    pub action: ActionDefinition,
    pub action_revision: String,
    pub execution_scope_id: ExecutionScopeId,
    pub action_id: String,
    pub trigger: ExecutionTrigger,
    pub creation_fingerprint: String,
    pub data_refs: ExecutionDataReferences,
    pub request: InvocationRequest,
    pub policy: ExecutionPolicy,
    pub created_at: SystemTime,
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum ExecutionMutation {
    StartAttempt {
        attempt: AttemptRecord,
    },
    AssignOwnership {
        attempt_id: AttemptId,
        ownership: AttemptOwnership,
    },
    ClearOwnership {
        attempt_id: AttemptId,
        expected_ownership: AttemptOwnership,
    },
    RequestCancellation {
        requested_at: SystemTime,
    },
    FinishAttempt {
        attempt_id: AttemptId,
        outcome: AttemptOutcome,
        result: Option<ExecutionResult>,
        retry: Option<AttemptRecord>,
        terminal: Option<TerminalState>,
    },
    FinishExecution {
        terminal: TerminalState,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum TransitionResult {
    Applied { aggregate: ExecutionAggregate },
    Unchanged { aggregate: ExecutionAggregate },
    Conflict { current_version: u64 },
}

#[derive(Debug, Clone, PartialEq)]
pub enum CreateExecutionResult {
    Created(ExecutionAggregate),
    Existing(ExecutionAggregate),
}

#[derive(Debug, Clone)]
pub struct ExecutionHistoryQuery {
    pub execution_scope_id: ExecutionScopeId,
    pub action_id: Option<String>,
    pub action_revision: Option<String>,
    pub cursor: Option<ExecutionId>,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct ExecutionHistoryPage {
    pub items: Vec<ExecutionAggregate>,
    pub next_cursor: Option<ExecutionId>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum StateStoreError {
    #[error("execution '{execution_id}' already exists")]
    AlreadyExists { execution_id: ExecutionId },
    #[error("execution identity '{execution_id}' conflicts with immutable creation data")]
    IdentityConflict { execution_id: ExecutionId },
    #[error("execution '{execution_id}' was not found")]
    NotFound { execution_id: ExecutionId },
    #[error("invalid execution history cursor '{cursor}'")]
    InvalidHistoryCursor { cursor: ExecutionId },
    #[error("invalid execution mutation: {0}")]
    InvalidMutation(String),
    #[error("execution state store lock is poisoned")]
    LockPoisoned,
    #[error("execution state store backend error: {0}")]
    Backend(String),
    #[error("execution state store backend error [{code}]: {message}")]
    BackendCode { code: String, message: String },
}

pub type StateStoreResult<T> = Result<T, StateStoreError>;

#[cfg_attr(test, mockall::automock)]
pub trait ExecutionStateStore: Send + Sync {
    fn create(&self, execution: NewExecution) -> StateStoreResult<ExecutionAggregate>;
    fn create_idempotent(&self, execution: NewExecution)
        -> StateStoreResult<CreateExecutionResult>;
    fn load(&self, execution_id: &ExecutionId) -> StateStoreResult<Option<ExecutionAggregate>>;
    fn compare_and_set(
        &self,
        execution_id: &ExecutionId,
        expected_version: u64,
        mutation: ExecutionMutation,
    ) -> StateStoreResult<TransitionResult>;
    fn reconcilable_cancellations(&self) -> StateStoreResult<Vec<ExecutionAggregate>>;
    fn active_executions(&self) -> StateStoreResult<Vec<ExecutionAggregate>>;
    fn list_history(&self, query: ExecutionHistoryQuery) -> StateStoreResult<ExecutionHistoryPage>;
}

#[derive(Default)]
pub struct MemoryExecutionStateStore {
    executions: Mutex<HashMap<ExecutionId, ExecutionAggregate>>,
}

impl ExecutionStateStore for MemoryExecutionStateStore {
    fn create(&self, execution: NewExecution) -> StateStoreResult<ExecutionAggregate> {
        let execution_id = execution.request.execution_id.clone();
        match self.create_idempotent(execution)? {
            CreateExecutionResult::Created(aggregate) => Ok(aggregate),
            CreateExecutionResult::Existing(_) => {
                Err(StateStoreError::AlreadyExists { execution_id })
            }
        }
    }

    fn create_idempotent(
        &self,
        execution: NewExecution,
    ) -> StateStoreResult<CreateExecutionResult> {
        validate_new_execution(&execution)?;
        let mut executions = self
            .executions
            .lock()
            .map_err(|_| StateStoreError::LockPoisoned)?;
        let execution_id = execution.request.execution_id.clone();
        if let Some(existing) = executions.get(&execution_id) {
            return if existing.creation_fingerprint == execution.creation_fingerprint {
                Ok(CreateExecutionResult::Existing(existing.clone()))
            } else {
                Err(StateStoreError::IdentityConflict { execution_id })
            };
        }

        let aggregate = aggregate_from_new(execution);
        executions.insert(execution_id, aggregate.clone());
        Ok(CreateExecutionResult::Created(aggregate))
    }

    fn load(&self, execution_id: &ExecutionId) -> StateStoreResult<Option<ExecutionAggregate>> {
        Ok(self
            .executions
            .lock()
            .map_err(|_| StateStoreError::LockPoisoned)?
            .get(execution_id)
            .cloned())
    }

    fn compare_and_set(
        &self,
        execution_id: &ExecutionId,
        expected_version: u64,
        mutation: ExecutionMutation,
    ) -> StateStoreResult<TransitionResult> {
        let mut executions = self
            .executions
            .lock()
            .map_err(|_| StateStoreError::LockPoisoned)?;
        let current = executions
            .get(execution_id)
            .ok_or_else(|| StateStoreError::NotFound {
                execution_id: execution_id.clone(),
            })?;
        if current.execution_version != expected_version {
            return Ok(TransitionResult::Conflict {
                current_version: current.execution_version,
            });
        }

        let result = apply_mutation(current, mutation)?;
        if let TransitionResult::Applied { aggregate } = &result {
            executions.insert(execution_id.clone(), aggregate.clone());
        }
        Ok(result)
    }

    fn reconcilable_cancellations(&self) -> StateStoreResult<Vec<ExecutionAggregate>> {
        Ok(self
            .executions
            .lock()
            .map_err(|_| StateStoreError::LockPoisoned)?
            .values()
            .filter(|aggregate| {
                aggregate.cancellation_intent.is_some() && aggregate.terminal_state.is_none()
            })
            .cloned()
            .collect())
    }

    fn active_executions(&self) -> StateStoreResult<Vec<ExecutionAggregate>> {
        Ok(self
            .executions
            .lock()
            .map_err(|_| StateStoreError::LockPoisoned)?
            .values()
            .filter(|aggregate| aggregate.active_attempt_id.is_some())
            .cloned()
            .collect())
    }

    fn list_history(&self, query: ExecutionHistoryQuery) -> StateStoreResult<ExecutionHistoryPage> {
        let mut executions = self
            .executions
            .lock()
            .map_err(|_| StateStoreError::LockPoisoned)?
            .values()
            .filter(|aggregate| {
                aggregate.execution_scope_id == query.execution_scope_id
                    && query
                        .action_id
                        .as_ref()
                        .is_none_or(|action_id| action_id == &aggregate.action_id)
                    && query
                        .action_revision
                        .as_ref()
                        .is_none_or(|revision| revision == &aggregate.action_revision)
            })
            .cloned()
            .collect::<Vec<_>>();
        executions.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| right.execution_id.as_ref().cmp(left.execution_id.as_ref()))
        });
        if let Some(cursor) = query.cursor {
            let position = executions
                .iter()
                .position(|aggregate| aggregate.execution_id == cursor)
                .ok_or(StateStoreError::InvalidHistoryCursor { cursor })?;
            executions.drain(..=position);
        }
        let limit = query.limit.clamp(1, 100);
        executions.truncate(limit + 1);
        let has_more = executions.len() > limit;
        executions.truncate(limit);
        let next_cursor = has_more
            .then(|| executions.last().map(|item| item.execution_id.clone()))
            .flatten();
        Ok(ExecutionHistoryPage {
            items: executions,
            next_cursor,
        })
    }
}

pub fn aggregate_from_new(execution: NewExecution) -> ExecutionAggregate {
    ExecutionAggregate {
        execution_id: execution.request.execution_id.clone(),
        action: execution.action,
        action_revision: execution.action_revision,
        execution_scope_id: execution.execution_scope_id,
        action_id: execution.action_id,
        trigger: execution.trigger,
        creation_fingerprint: execution.creation_fingerprint,
        data_refs: execution.data_refs,
        request: execution.request,
        policy: execution.policy,
        state: ExecutionState::Pending,
        active_attempt_id: None,
        attempts: Vec::new(),
        cancellation_intent: None,
        terminal_state: None,
        created_at: execution.created_at,
        updated_at: execution.created_at,
        execution_version: 0,
    }
}

pub fn apply_mutation(
    current: &ExecutionAggregate,
    mutation: ExecutionMutation,
) -> StateStoreResult<TransitionResult> {
    let mut next = current.clone();
    let changed = apply(&mut next, mutation)?;
    if !changed {
        return Ok(TransitionResult::Unchanged {
            aggregate: current.clone(),
        });
    }

    validate_execution_aggregate(&next)?;

    next.execution_version = current
        .execution_version
        .checked_add(1)
        .ok_or_else(|| invalid("execution version overflow"))?;
    next.updated_at = SystemTime::now();
    Ok(TransitionResult::Applied { aggregate: next })
}

pub fn validate_new_execution(execution: &NewExecution) -> StateStoreResult<()> {
    if execution.action_id.trim().is_empty() {
        return Err(invalid("action id must not be empty"));
    }
    if execution.creation_fingerprint.trim().is_empty() {
        return Err(invalid("creation fingerprint must not be empty"));
    }
    if execution.action_revision.trim().is_empty() {
        return Err(invalid("action revision must not be empty"));
    }
    if execution.policy.timeout.is_zero() {
        return Err(invalid("execution timeout must be greater than zero"));
    }
    if execution.policy.retry.max_attempts == 0 {
        return Err(invalid("retry max_attempts must be greater than zero"));
    }
    if !execution.policy.retry.backoff.is_finite() || execution.policy.retry.backoff <= 0.0 {
        return Err(invalid(
            "retry backoff must be finite and greater than zero",
        ));
    }
    if execution.action.policy.retry.max_attempts == 0 {
        return Err(invalid(
            "action retry max_attempts must be greater than zero",
        ));
    }
    if !execution.action.policy.retry.backoff.is_finite()
        || execution.action.policy.retry.backoff <= 0.0
    {
        return Err(invalid(
            "action retry backoff must be finite and greater than zero",
        ));
    }
    Ok(())
}

pub fn validate_execution_result(result: &ExecutionResult) -> StateStoreResult<()> {
    if result
        .events
        .iter()
        .any(|event| matches!(event, InvocationEvent::Log(_)))
    {
        return Err(invalid("execution results must not contain log events"));
    }
    Ok(())
}

pub fn action_revision(action: &ActionDefinition) -> StateStoreResult<String> {
    let definition = serde_json::to_vec(action)
        .map_err(|error| invalid(format!("serialize action revision: {error}")))?;
    let hash = definition
        .iter()
        .fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
    Ok(format!("action-definition-v1:{hash:016x}"))
}

pub fn execution_creation_fingerprint(
    execution_scope_id: &ExecutionScopeId,
    action_id: &str,
    action_revision: &str,
    trigger: &ExecutionTrigger,
    request: &InvocationRequest,
    policy: &ExecutionPolicy,
    data_refs: &ExecutionDataReferences,
) -> StateStoreResult<String> {
    #[derive(Serialize)]
    struct ImmutableCreation<'a> {
        execution_scope_id: &'a ExecutionScopeId,
        execution_id: &'a ExecutionId,
        action_id: &'a str,
        action_revision: &'a str,
        trigger: &'a ExecutionTrigger,
        protocol_version: &'a str,
        event: &'a serde_json::Value,
        context: &'a ryvus_protocol::InvocationContext,
        policy: &'a ExecutionPolicy,
        input_ref: &'a Option<crate::ExecutionDataRef>,
    }

    let value = ImmutableCreation {
        execution_scope_id,
        execution_id: &request.execution_id,
        action_id,
        action_revision,
        trigger,
        protocol_version: &request.protocol_version,
        event: &request.event,
        context: &request.context,
        policy,
        input_ref: &data_refs.input_ref,
    };
    let bytes = serde_json::to_vec(&value)
        .map_err(|error| invalid(format!("serialize execution creation fingerprint: {error}")))?;
    let hash = bytes.iter().fold(0xcbf29ce484222325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    });
    Ok(format!("execution-creation-v1:{hash:016x}"))
}

pub fn validate_execution_aggregate(aggregate: &ExecutionAggregate) -> StateStoreResult<()> {
    if aggregate.action_id.trim().is_empty() || aggregate.creation_fingerprint.trim().is_empty() {
        return Err(invalid("execution identity metadata must not be empty"));
    }
    if aggregate.request.execution_id != aggregate.execution_id {
        return Err(invalid("invocation request belongs to another execution"));
    }
    if aggregate
        .cancellation_intent
        .as_ref()
        .is_some_and(|intent| intent.execution_id != aggregate.execution_id)
    {
        return Err(invalid("cancellation intent belongs to another execution"));
    }

    let mut attempt_ids = HashSet::new();
    for (index, record) in aggregate.attempts.iter().enumerate() {
        let expected_number =
            u32::try_from(index + 1).map_err(|_| invalid("attempt number is outside u32 range"))?;
        if record.attempt.execution_id != aggregate.execution_id
            || record.attempt.attempt_number != expected_number
            || !attempt_ids.insert(record.attempt.attempt_id.clone())
        {
            return Err(invalid("attempt identity or sequence is inconsistent"));
        }
        if let Some(ownership) = &record.ownership {
            if ownership.execution_id != aggregate.execution_id
                || ownership.attempt_id != record.attempt.attempt_id
                || ownership.attempt_number != record.attempt.attempt_number
            {
                return Err(invalid("attempt ownership identity is inconsistent"));
            }
            if record.state != ExecutionState::Running {
                return Err(invalid("only a running attempt may retain ownership"));
            }
        }
        if let Some(result) = &record.result {
            validate_execution_result(result)?;
            if result.invocation_result.attempt() != record.attempt
                || result
                    .events
                    .iter()
                    .any(|event| event.attempt() != record.attempt)
            {
                return Err(invalid("execution result identity is inconsistent"));
            }
        }

        match record.state {
            ExecutionState::Pending => {
                if record.started_at.is_some()
                    || record.finished_at.is_some()
                    || record.outcome.is_some()
                    || record.result.is_some()
                    || record.ownership.is_some()
                {
                    return Err(invalid(
                        "pending attempt contains authoritative runtime facts",
                    ));
                }
            }
            ExecutionState::Running => {
                if record.started_at.is_none()
                    || record.finished_at.is_some()
                    || record.outcome.is_some()
                    || record.result.is_some()
                {
                    return Err(invalid("running attempt lifecycle is inconsistent"));
                }
            }
            ExecutionState::Succeeded
            | ExecutionState::Failed
            | ExecutionState::Cancelled
            | ExecutionState::TimedOut => {
                let Some(outcome) = record.outcome else {
                    return Err(invalid("finished attempt is missing its outcome"));
                };
                if record.started_at.is_none()
                    || record.finished_at.is_none()
                    || outcome_state(outcome) != record.state
                {
                    return Err(invalid("finished attempt lifecycle is inconsistent"));
                }
            }
            ExecutionState::CancellationRequested => {
                return Err(invalid("attempt cannot use cancellation-requested state"));
            }
        }
    }

    let running_attempts = aggregate
        .attempts
        .iter()
        .filter(|attempt| attempt.state == ExecutionState::Running)
        .collect::<Vec<_>>();
    match &aggregate.active_attempt_id {
        Some(active_attempt_id) => {
            if aggregate.state != ExecutionState::Running
                || running_attempts.len() != 1
                || running_attempts[0].attempt.attempt_id != *active_attempt_id
            {
                return Err(invalid(
                    "active attempt and execution state are inconsistent",
                ));
            }
        }
        None if !running_attempts.is_empty() => {
            return Err(invalid(
                "running attempt is not the authoritative active attempt",
            ));
        }
        None => {}
    }
    if aggregate.state == ExecutionState::Running && aggregate.active_attempt_id.is_none() {
        return Err(invalid(
            "running execution is missing its authoritative active attempt",
        ));
    }

    match &aggregate.terminal_state {
        Some(terminal) => {
            validate_terminal(terminal)?;
            if terminal.state != aggregate.state || aggregate.active_attempt_id.is_some() {
                return Err(invalid("execution and terminal state are inconsistent"));
            }
            if let Some(attempt_id) = &terminal.attempt_id {
                let attempt = aggregate
                    .attempts
                    .iter()
                    .find(|attempt| &attempt.attempt.attempt_id == attempt_id)
                    .ok_or_else(|| invalid("terminal attempt is missing"))?;
                if attempt.state != terminal.state {
                    return Err(invalid("terminal attempt and execution outcome disagree"));
                }
            }
        }
        None if matches!(
            aggregate.state,
            ExecutionState::Succeeded
                | ExecutionState::Failed
                | ExecutionState::Cancelled
                | ExecutionState::TimedOut
        ) =>
        {
            return Err(invalid("terminal execution is missing terminal state"))
        }
        None => {}
    }
    Ok(())
}

fn apply(
    aggregate: &mut ExecutionAggregate,
    mutation: ExecutionMutation,
) -> StateStoreResult<bool> {
    match mutation {
        ExecutionMutation::StartAttempt { mut attempt } => {
            ensure_mutable(aggregate)?;
            if aggregate.cancellation_intent.is_some() {
                return Err(invalid(
                    "cannot start an attempt after cancellation was requested",
                ));
            }
            if aggregate.active_attempt_id.is_some() {
                return Err(invalid("execution already has an active attempt"));
            }
            validate_pending_attempt(&attempt)?;
            validate_attempt_identity(aggregate, &attempt.attempt)?;

            if let Some(existing) = aggregate
                .attempts
                .iter_mut()
                .find(|existing| existing.attempt.attempt_id == attempt.attempt.attempt_id)
            {
                if existing != &attempt {
                    return Err(invalid("started attempt does not match its pending record"));
                }
                existing.state = ExecutionState::Running;
                existing.started_at = Some(SystemTime::now());
                aggregate.active_attempt_id = Some(existing.attempt.attempt_id.clone());
            } else {
                validate_new_attempt(aggregate, &attempt.attempt)?;
                attempt.state = ExecutionState::Running;
                attempt.started_at = Some(SystemTime::now());
                aggregate.active_attempt_id = Some(attempt.attempt.attempt_id.clone());
                aggregate.attempts.push(attempt);
            }
            aggregate.state = ExecutionState::Running;
            Ok(true)
        }
        ExecutionMutation::AssignOwnership {
            attempt_id,
            ownership,
        } => {
            ensure_mutable(aggregate)?;
            if aggregate.active_attempt_id.as_ref() != Some(&attempt_id) {
                return Err(invalid("ownership must belong to the active attempt"));
            }
            let attempt = aggregate
                .attempts
                .iter_mut()
                .find(|attempt| attempt.attempt.attempt_id == attempt_id)
                .ok_or_else(|| invalid("active attempt record was not found"))?;
            if ownership.execution_id != aggregate.execution_id
                || ownership.attempt_id != attempt_id
                || ownership.attempt_number != attempt.attempt.attempt_number
            {
                return Err(invalid(
                    "ownership identity does not match the active attempt",
                ));
            }
            if attempt.ownership.as_ref() == Some(&ownership) {
                return Ok(false);
            }
            attempt.ownership = Some(ownership);
            Ok(true)
        }
        ExecutionMutation::ClearOwnership {
            attempt_id,
            expected_ownership,
        } => {
            ensure_mutable(aggregate)?;
            let attempt = aggregate
                .attempts
                .iter_mut()
                .find(|attempt| attempt.attempt.attempt_id == attempt_id)
                .ok_or_else(|| invalid("active attempt record was not found"))?;
            if attempt.ownership.as_ref() == Some(&expected_ownership) {
                attempt.ownership = None;
                Ok(true)
            } else {
                Ok(false)
            }
        }
        ExecutionMutation::RequestCancellation { requested_at } => {
            ensure_mutable(aggregate)?;
            if aggregate.cancellation_intent.is_some() {
                return Ok(false);
            }
            aggregate.cancellation_intent = Some(CancellationIntent {
                execution_id: aggregate.execution_id.clone(),
                requested_at,
            });
            Ok(true)
        }
        ExecutionMutation::FinishAttempt {
            attempt_id,
            outcome,
            result,
            retry,
            terminal,
        } => {
            ensure_mutable(aggregate)?;
            if let Some(result) = &result {
                validate_execution_result(result)?;
            }
            if retry.is_some() == terminal.is_some() {
                return Err(invalid(
                    "finishing an attempt requires exactly one retry or terminal state",
                ));
            }
            if aggregate.active_attempt_id.as_ref() != Some(&attempt_id) {
                return Err(invalid("only the active attempt can be finished"));
            }
            if retry.is_some() && aggregate.cancellation_intent.is_some() {
                return Err(invalid("cannot retry after cancellation was requested"));
            }
            if let Some(retry) = &retry {
                validate_pending_attempt(retry)?;
                validate_attempt_identity(aggregate, &retry.attempt)?;
                validate_new_attempt(aggregate, &retry.attempt)?;
            }
            if let Some(terminal) = &terminal {
                validate_terminal(terminal)?;
                if terminal.state != outcome_state(outcome) {
                    return Err(invalid(
                        "terminal state does not match the accepted attempt outcome",
                    ));
                }
                if terminal.attempt_id.as_ref() != Some(&attempt_id) {
                    return Err(invalid(
                        "terminal attempt does not match the active attempt",
                    ));
                }
            }

            let attempt = aggregate
                .attempts
                .iter_mut()
                .find(|attempt| attempt.attempt.attempt_id == attempt_id)
                .ok_or_else(|| invalid("active attempt record was not found"))?;
            if attempt.outcome.is_some() {
                return Err(invalid("attempt outcome is immutable"));
            }
            attempt.state = outcome_state(outcome);
            attempt.outcome = Some(outcome);
            attempt.result = result;
            attempt.finished_at = Some(SystemTime::now());
            attempt.ownership = None;
            aggregate.active_attempt_id = None;

            if let Some(retry) = retry {
                aggregate.attempts.push(retry);
                aggregate.state = ExecutionState::Pending;
            } else if let Some(terminal) = terminal {
                aggregate.state = terminal.state;
                aggregate.terminal_state = Some(terminal);
            }
            Ok(true)
        }
        ExecutionMutation::FinishExecution { terminal } => {
            ensure_mutable(aggregate)?;
            validate_terminal(&terminal)?;
            if aggregate.active_attempt_id.is_some() {
                return Err(invalid("cannot finish an execution with an active attempt"));
            }
            if terminal.attempt_id.is_some() {
                return Err(invalid(
                    "execution without an active attempt cannot name an attempt",
                ));
            }
            aggregate.state = terminal.state;
            aggregate.terminal_state = Some(terminal);
            Ok(true)
        }
    }
}

fn ensure_mutable(aggregate: &ExecutionAggregate) -> StateStoreResult<()> {
    if aggregate.terminal_state.is_some() {
        Err(invalid("terminal execution state is immutable"))
    } else {
        Ok(())
    }
}

fn validate_attempt_identity(
    aggregate: &ExecutionAggregate,
    attempt: &ExecutionAttempt,
) -> StateStoreResult<()> {
    if attempt.execution_id != aggregate.execution_id {
        return Err(invalid("attempt belongs to another execution"));
    }
    if attempt.attempt_number == 0 {
        return Err(invalid("attempt number must be one-based"));
    }
    Ok(())
}

fn validate_pending_attempt(attempt: &AttemptRecord) -> StateStoreResult<()> {
    if attempt.state != ExecutionState::Pending
        || attempt.ownership.is_some()
        || attempt.outcome.is_some()
        || attempt.result.is_some()
        || attempt.started_at.is_some()
        || attempt.finished_at.is_some()
    {
        Err(invalid("new attempt record must be clean and pending"))
    } else {
        Ok(())
    }
}

fn validate_new_attempt(
    aggregate: &ExecutionAggregate,
    attempt: &ExecutionAttempt,
) -> StateStoreResult<()> {
    if aggregate.attempts.iter().any(|existing| {
        existing.attempt.attempt_id == attempt.attempt_id
            || existing.attempt.attempt_number == attempt.attempt_number
    }) {
        return Err(invalid("duplicate attempt identity or number"));
    }

    let expected_number = aggregate
        .attempts
        .iter()
        .map(|existing| existing.attempt.attempt_number)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| invalid("attempt number overflow"))?;
    if attempt.attempt_number != expected_number {
        return Err(invalid(format!("attempt number must be {expected_number}")));
    }
    Ok(())
}

fn validate_terminal(terminal: &TerminalState) -> StateStoreResult<()> {
    if matches!(
        terminal.state,
        ExecutionState::Succeeded
            | ExecutionState::Failed
            | ExecutionState::Cancelled
            | ExecutionState::TimedOut
    ) {
        Ok(())
    } else {
        Err(invalid("terminal state must be final"))
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

fn invalid(message: impl Into<String>) -> StateStoreError {
    StateStoreError::InvalidMutation(message.into())
}
