use std::{
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use postgres::{error::SqlState, Client, NoTls, Row, Transaction};
use ryvus_execution::{
    apply_mutation, validate_execution_aggregate, validate_new_execution, AttemptRecord,
    CancellationIntent, ExecutionAggregate, ExecutionMutation, ExecutionState, ExecutionStateStore,
    NewExecution, StateStoreError, StateStoreResult, TerminalState, TransitionResult,
};
use ryvus_protocol::{AttemptId, AttemptOutcome, ExecutionAttempt, ExecutionId};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;

const MIGRATION_LOCK_ID: i64 = 7_823_981_045_710_001;
const MIGRATIONS: &[(i64, &str)] = &[(1, include_str!("../migrations/0001_execution_state.sql"))];

pub struct PostgresExecutionStateStore {
    client: Mutex<Client>,
}

impl PostgresExecutionStateStore {
    pub fn connect(database_url: &str) -> StateStoreResult<Self> {
        let client = Client::connect(database_url, NoTls)
            .map_err(|error| backend("connect to PostgreSQL", error))?;
        Ok(Self {
            client: Mutex::new(client),
        })
    }

    fn transaction<T>(
        &self,
        operation: impl FnOnce(&mut Transaction<'_>) -> StateStoreResult<T>,
    ) -> StateStoreResult<T> {
        let mut client = self
            .client
            .lock()
            .map_err(|_| StateStoreError::Backend("PostgreSQL client lock is poisoned".into()))?;
        let mut transaction = client
            .transaction()
            .map_err(|error| backend("begin PostgreSQL transaction", error))?;
        let value = operation(&mut transaction)?;
        transaction
            .commit()
            .map_err(|error| backend("commit PostgreSQL transaction", error))?;
        Ok(value)
    }
}

impl ExecutionStateStore for PostgresExecutionStateStore {
    fn create(&self, execution: NewExecution) -> StateStoreResult<ExecutionAggregate> {
        validate_new_execution(&execution)?;
        let execution_id = execution.request.execution_id.clone();
        let aggregate = ExecutionAggregate {
            execution_id: execution_id.clone(),
            action: execution.action,
            action_revision: execution.action_revision,
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
        };

        self.transaction(
            |transaction| match insert_execution(transaction, &aggregate) {
                Ok(()) => Ok(aggregate.clone()),
                Err(StateStoreError::Backend(message))
                    if message.starts_with("unique violation:") =>
                {
                    Err(StateStoreError::AlreadyExists {
                        execution_id: execution_id.clone(),
                    })
                }
                Err(error) => Err(error),
            },
        )
    }

    fn load(&self, execution_id: &ExecutionId) -> StateStoreResult<Option<ExecutionAggregate>> {
        self.transaction(|transaction| load_aggregate(transaction, execution_id, "FOR SHARE"))
    }

    fn compare_and_set(
        &self,
        execution_id: &ExecutionId,
        expected_version: u64,
        mutation: ExecutionMutation,
    ) -> StateStoreResult<TransitionResult> {
        self.transaction(|transaction| {
            let current =
                load_aggregate(transaction, execution_id, "FOR UPDATE")?.ok_or_else(|| {
                    StateStoreError::NotFound {
                        execution_id: execution_id.clone(),
                    }
                })?;
            if current.execution_version != expected_version {
                return Ok(TransitionResult::Conflict {
                    current_version: current.execution_version,
                });
            }

            let transition = apply_mutation(&current, mutation)?;
            let TransitionResult::Applied { aggregate } = &transition else {
                return Ok(transition);
            };
            let updated = update_execution(transaction, aggregate, expected_version)?;
            if updated == 0 {
                let version = transaction
                    .query_one(
                        "SELECT execution_version FROM ryvus_executions WHERE execution_id = $1",
                        &[&execution_id.as_ref()],
                    )
                    .map_err(|error| backend("reload conflicting execution version", error))?
                    .get::<_, i64>(0);
                return Ok(TransitionResult::Conflict {
                    current_version: from_i64(version, "execution_version")?,
                });
            }
            replace_children(transaction, aggregate)?;
            Ok(transition)
        })
    }

    fn reconcilable_cancellations(&self) -> StateStoreResult<Vec<ExecutionAggregate>> {
        let mut aggregates = self.query_aggregates(
            "SELECT e.execution_id FROM ryvus_executions e \
             JOIN ryvus_cancellation_intents c USING (execution_id) \
             LEFT JOIN ryvus_terminal_states t USING (execution_id) \
             WHERE t.execution_id IS NULL ORDER BY e.execution_id",
        )?;
        aggregates.retain(|aggregate| {
            aggregate.cancellation_intent.is_some() && aggregate.terminal_state.is_none()
        });
        Ok(aggregates)
    }

    fn active_executions(&self) -> StateStoreResult<Vec<ExecutionAggregate>> {
        let mut aggregates = self.query_aggregates(
            "SELECT execution_id FROM ryvus_executions \
             WHERE active_attempt_id IS NOT NULL ORDER BY execution_id",
        )?;
        aggregates.retain(|aggregate| aggregate.active_attempt_id.is_some());
        Ok(aggregates)
    }
}

impl PostgresExecutionStateStore {
    fn query_aggregates(&self, sql: &str) -> StateStoreResult<Vec<ExecutionAggregate>> {
        self.transaction(|transaction| {
            let ids = transaction
                .query(sql, &[])
                .map_err(|error| backend("query execution aggregates", error))?
                .into_iter()
                .map(|row| ExecutionId::from(row.get::<_, String>(0)))
                .collect::<Vec<_>>();
            let mut aggregates = Vec::with_capacity(ids.len());
            for execution_id in ids {
                let aggregate = load_aggregate(transaction, &execution_id, "FOR SHARE")?
                    .ok_or_else(|| corrupt("execution disappeared during consistent read"))?;
                aggregates.push(aggregate);
            }
            Ok(aggregates)
        })
    }
}

pub fn migrate(database_url: &str) -> StateStoreResult<()> {
    let mut client = Client::connect(database_url, NoTls)
        .map_err(|error| backend("connect to PostgreSQL for migration", error))?;
    client
        .query_one("SELECT pg_advisory_lock($1)", &[&MIGRATION_LOCK_ID])
        .map_err(|error| backend("acquire migration advisory lock", error))?;

    let result = apply_migrations(&mut client);
    let unlock = client
        .query_one("SELECT pg_advisory_unlock($1)", &[&MIGRATION_LOCK_ID])
        .map_err(|error| backend("release migration advisory lock", error));
    match (result, unlock) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(_)) => Ok(()),
    }
}

fn apply_migrations(client: &mut Client) -> StateStoreResult<()> {
    client
        .batch_execute(
            "CREATE TABLE IF NOT EXISTS ryvus_schema_migrations (\
                 version BIGINT PRIMARY KEY, \
                 applied_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP\
             )",
        )
        .map_err(|error| backend("create migration history", error))?;

    for (version, sql) in MIGRATIONS {
        let applied = client
            .query_opt(
                "SELECT version FROM ryvus_schema_migrations WHERE version = $1",
                &[version],
            )
            .map_err(|error| backend("read migration history", error))?
            .is_some();
        if applied {
            continue;
        }
        let mut transaction = client
            .transaction()
            .map_err(|error| backend("begin migration transaction", error))?;
        transaction
            .batch_execute(sql)
            .map_err(|error| backend("apply execution state migration", error))?;
        transaction
            .execute(
                "INSERT INTO ryvus_schema_migrations (version) VALUES ($1)",
                &[version],
            )
            .map_err(|error| backend("record execution state migration", error))?;
        transaction
            .commit()
            .map_err(|error| backend("commit execution state migration", error))?;
    }
    Ok(())
}

fn insert_execution(
    transaction: &mut Transaction<'_>,
    aggregate: &ExecutionAggregate,
) -> StateStoreResult<()> {
    let action = json(&aggregate.action, "action")?;
    let request = json(&aggregate.request, "invocation request")?;
    let policy = json(&aggregate.policy, "execution policy")?;
    let version = to_i64(aggregate.execution_version, "execution_version")?;
    let created_at = system_time_to_i64(aggregate.created_at)?;
    let updated_at = system_time_to_i64(aggregate.updated_at)?;
    transaction
        .execute(
            "INSERT INTO ryvus_executions (\
                 execution_id, action, action_revision, invocation_request, policy, state, \
                 active_attempt_id, created_at_unix_ns, updated_at_unix_ns, execution_version\
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            &[
                &aggregate.execution_id.as_ref(),
                &action,
                &aggregate.action_revision,
                &request,
                &policy,
                &state_name(aggregate.state),
                &aggregate.active_attempt_id.as_ref().map(AsRef::as_ref),
                &created_at,
                &updated_at,
                &version,
            ],
        )
        .map_err(|error| database_write_error("insert execution", error))?;
    Ok(())
}

fn update_execution(
    transaction: &mut Transaction<'_>,
    aggregate: &ExecutionAggregate,
    expected_version: u64,
) -> StateStoreResult<u64> {
    let action = json(&aggregate.action, "action")?;
    let request = json(&aggregate.request, "invocation request")?;
    let policy = json(&aggregate.policy, "execution policy")?;
    let version = to_i64(aggregate.execution_version, "execution_version")?;
    let expected = to_i64(expected_version, "expected execution_version")?;
    let updated_at = system_time_to_i64(aggregate.updated_at)?;
    transaction
        .execute(
            "UPDATE ryvus_executions SET \
                 action = $2, action_revision = $3, invocation_request = $4, policy = $5, \
                 state = $6, active_attempt_id = $7, updated_at_unix_ns = $8, \
                 execution_version = $9 WHERE execution_id = $1 AND execution_version = $10",
            &[
                &aggregate.execution_id.as_ref(),
                &action,
                &aggregate.action_revision,
                &request,
                &policy,
                &state_name(aggregate.state),
                &aggregate.active_attempt_id.as_ref().map(AsRef::as_ref),
                &updated_at,
                &version,
                &expected,
            ],
        )
        .map_err(|error| database_write_error("update execution", error))
}

fn replace_children(
    transaction: &mut Transaction<'_>,
    aggregate: &ExecutionAggregate,
) -> StateStoreResult<()> {
    transaction
        .execute(
            "DELETE FROM ryvus_terminal_states WHERE execution_id = $1",
            &[&aggregate.execution_id.as_ref()],
        )
        .map_err(|error| backend("replace terminal state", error))?;
    transaction
        .execute(
            "DELETE FROM ryvus_attempts WHERE execution_id = $1",
            &[&aggregate.execution_id.as_ref()],
        )
        .map_err(|error| backend("replace attempts", error))?;
    for attempt in &aggregate.attempts {
        insert_attempt(transaction, attempt)?;
    }

    transaction
        .execute(
            "DELETE FROM ryvus_cancellation_intents WHERE execution_id = $1",
            &[&aggregate.execution_id.as_ref()],
        )
        .map_err(|error| backend("replace cancellation intent", error))?;
    if let Some(intent) = &aggregate.cancellation_intent {
        let requested_at = system_time_to_i64(intent.requested_at)?;
        transaction
            .execute(
                "INSERT INTO ryvus_cancellation_intents (execution_id, requested_at_unix_ns) \
                 VALUES ($1, $2)",
                &[&intent.execution_id.as_ref(), &requested_at],
            )
            .map_err(|error| backend("insert cancellation intent", error))?;
    }

    if let Some(terminal) = &aggregate.terminal_state {
        let accepted_at = system_time_to_i64(terminal.accepted_at)?;
        transaction
            .execute(
                "INSERT INTO ryvus_terminal_states \
                     (execution_id, state, attempt_id, accepted_at_unix_ns) \
                 VALUES ($1, $2, $3, $4)",
                &[
                    &aggregate.execution_id.as_ref(),
                    &state_name(terminal.state),
                    &terminal.attempt_id.as_ref().map(AsRef::as_ref),
                    &accepted_at,
                ],
            )
            .map_err(|error| backend("insert terminal state", error))?;
    }
    Ok(())
}

fn insert_attempt(
    transaction: &mut Transaction<'_>,
    attempt: &AttemptRecord,
) -> StateStoreResult<()> {
    let ownership = optional_json(attempt.ownership.as_ref(), "attempt ownership")?;
    let result = optional_json(attempt.result.as_ref(), "execution result")?;
    let attempt_number = i64::from(attempt.attempt.attempt_number);
    let started_at = optional_system_time_to_i64(attempt.started_at)?;
    let finished_at = optional_system_time_to_i64(attempt.finished_at)?;
    transaction
        .execute(
            "INSERT INTO ryvus_attempts (\
                 execution_id, attempt_id, attempt_number, deadline_unix_ms, state, ownership, \
                 outcome, result, started_at_unix_ns, finished_at_unix_ns\
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            &[
                &attempt.attempt.execution_id.as_ref(),
                &attempt.attempt.attempt_id.as_ref(),
                &attempt_number,
                &attempt.deadline_unix_ms,
                &state_name(attempt.state),
                &ownership,
                &attempt.outcome.map(outcome_name),
                &result,
                &started_at,
                &finished_at,
            ],
        )
        .map_err(|error| database_write_error("insert attempt", error))?;
    Ok(())
}

fn load_aggregate(
    transaction: &mut Transaction<'_>,
    execution_id: &ExecutionId,
    lock: &str,
) -> StateStoreResult<Option<ExecutionAggregate>> {
    let sql = format!(
        "SELECT execution_id, action, action_revision, invocation_request, policy, state, \
             active_attempt_id, created_at_unix_ns, updated_at_unix_ns, execution_version \
         FROM ryvus_executions WHERE execution_id = $1 {lock}"
    );
    let Some(row) = transaction
        .query_opt(&sql, &[&execution_id.as_ref()])
        .map_err(|error| backend("load execution", error))?
    else {
        return Ok(None);
    };

    let attempts = transaction
        .query(
            "SELECT execution_id, attempt_id, attempt_number, deadline_unix_ms, state, ownership, \
                 outcome, result, started_at_unix_ns, finished_at_unix_ns \
             FROM ryvus_attempts WHERE execution_id = $1 ORDER BY attempt_number",
            &[&execution_id.as_ref()],
        )
        .map_err(|error| backend("load attempts", error))?
        .iter()
        .map(attempt_from_row)
        .collect::<StateStoreResult<Vec<_>>>()?;
    let cancellation_intent = transaction
        .query_opt(
            "SELECT requested_at_unix_ns FROM ryvus_cancellation_intents WHERE execution_id = $1",
            &[&execution_id.as_ref()],
        )
        .map_err(|error| backend("load cancellation intent", error))?
        .map(|row| {
            Ok(CancellationIntent {
                execution_id: execution_id.clone(),
                requested_at: system_time_from_i64(row.get(0))?,
            })
        })
        .transpose()?;
    let terminal_state = transaction
        .query_opt(
            "SELECT state, attempt_id, accepted_at_unix_ns \
             FROM ryvus_terminal_states WHERE execution_id = $1",
            &[&execution_id.as_ref()],
        )
        .map_err(|error| backend("load terminal state", error))?
        .map(terminal_from_row)
        .transpose()?;

    let aggregate = ExecutionAggregate {
        execution_id: ExecutionId::from(row.get::<_, String>("execution_id")),
        action: from_json(row.get("action"), "action")?,
        action_revision: row.get("action_revision"),
        request: from_json(row.get("invocation_request"), "invocation request")?,
        policy: from_json(row.get("policy"), "execution policy")?,
        state: parse_state(row.get("state"))?,
        active_attempt_id: row
            .get::<_, Option<String>>("active_attempt_id")
            .map(AttemptId::from),
        attempts,
        cancellation_intent,
        terminal_state,
        created_at: system_time_from_i64(row.get("created_at_unix_ns"))?,
        updated_at: system_time_from_i64(row.get("updated_at_unix_ns"))?,
        execution_version: from_i64(row.get("execution_version"), "execution_version")?,
    };
    validate_execution_aggregate(&aggregate)
        .map_err(|error| corrupt(format!("aggregate invariants failed: {error}")))?;
    Ok(Some(aggregate))
}

fn attempt_from_row(row: &Row) -> StateStoreResult<AttemptRecord> {
    let attempt_number = row.get::<_, i64>("attempt_number");
    Ok(AttemptRecord {
        attempt: ExecutionAttempt {
            execution_id: ExecutionId::from(row.get::<_, String>("execution_id")),
            attempt_id: AttemptId::from(row.get::<_, String>("attempt_id")),
            attempt_number: u32::try_from(attempt_number)
                .map_err(|_| corrupt("attempt_number is outside u32 range"))?,
        },
        deadline_unix_ms: row.get("deadline_unix_ms"),
        state: parse_state(row.get("state"))?,
        ownership: optional_from_json(row.get("ownership"), "attempt ownership")?,
        outcome: row
            .get::<_, Option<String>>("outcome")
            .map(|value| parse_outcome(&value))
            .transpose()?,
        result: optional_from_json(row.get("result"), "execution result")?,
        started_at: optional_system_time_from_i64(row.get("started_at_unix_ns"))?,
        finished_at: optional_system_time_from_i64(row.get("finished_at_unix_ns"))?,
    })
}

fn terminal_from_row(row: Row) -> StateStoreResult<TerminalState> {
    Ok(TerminalState {
        state: parse_state(row.get("state"))?,
        attempt_id: row
            .get::<_, Option<String>>("attempt_id")
            .map(AttemptId::from),
        accepted_at: system_time_from_i64(row.get("accepted_at_unix_ns"))?,
    })
}

fn state_name(state: ExecutionState) -> &'static str {
    match state {
        ExecutionState::Pending => "pending",
        ExecutionState::Running => "running",
        ExecutionState::CancellationRequested => "cancellation_requested",
        ExecutionState::Succeeded => "succeeded",
        ExecutionState::Failed => "failed",
        ExecutionState::Cancelled => "cancelled",
        ExecutionState::TimedOut => "timed_out",
    }
}

fn parse_state(value: String) -> StateStoreResult<ExecutionState> {
    match value.as_str() {
        "pending" => Ok(ExecutionState::Pending),
        "running" => Ok(ExecutionState::Running),
        "cancellation_requested" => Ok(ExecutionState::CancellationRequested),
        "succeeded" => Ok(ExecutionState::Succeeded),
        "failed" => Ok(ExecutionState::Failed),
        "cancelled" => Ok(ExecutionState::Cancelled),
        "timed_out" => Ok(ExecutionState::TimedOut),
        _ => Err(corrupt(format!("unknown execution state '{value}'"))),
    }
}

fn outcome_name(outcome: AttemptOutcome) -> &'static str {
    match outcome {
        AttemptOutcome::Succeeded => "succeeded",
        AttemptOutcome::Failed => "failed",
        AttemptOutcome::Cancelled => "cancelled",
        AttemptOutcome::TimedOut => "timed_out",
        AttemptOutcome::InfrastructureFailed => "infrastructure_failed",
    }
}

fn parse_outcome(value: &str) -> StateStoreResult<AttemptOutcome> {
    match value {
        "succeeded" => Ok(AttemptOutcome::Succeeded),
        "failed" => Ok(AttemptOutcome::Failed),
        "cancelled" => Ok(AttemptOutcome::Cancelled),
        "timed_out" => Ok(AttemptOutcome::TimedOut),
        "infrastructure_failed" => Ok(AttemptOutcome::InfrastructureFailed),
        _ => Err(corrupt(format!("unknown attempt outcome '{value}'"))),
    }
}

fn json(value: &impl Serialize, field: &str) -> StateStoreResult<Value> {
    serde_json::to_value(value).map_err(|error| corrupt(format!("serialize {field}: {error}")))
}

fn optional_json<T: Serialize>(value: Option<&T>, field: &str) -> StateStoreResult<Option<Value>> {
    value.map(|value| json(value, field)).transpose()
}

fn from_json<T: DeserializeOwned>(value: Value, field: &str) -> StateStoreResult<T> {
    serde_json::from_value(value).map_err(|error| corrupt(format!("deserialize {field}: {error}")))
}

fn optional_from_json<T: DeserializeOwned>(
    value: Option<Value>,
    field: &str,
) -> StateStoreResult<Option<T>> {
    value.map(|value| from_json(value, field)).transpose()
}

fn system_time_to_i64(value: SystemTime) -> StateStoreResult<i64> {
    match value.duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_nanos())
            .map_err(|_| corrupt("SystemTime is outside supported nanosecond range")),
        Err(error) => {
            let nanos = i64::try_from(error.duration().as_nanos())
                .map_err(|_| corrupt("SystemTime is outside supported nanosecond range"))?;
            nanos
                .checked_neg()
                .ok_or_else(|| corrupt("SystemTime is outside supported nanosecond range"))
        }
    }
}

fn optional_system_time_to_i64(value: Option<SystemTime>) -> StateStoreResult<Option<i64>> {
    value.map(system_time_to_i64).transpose()
}

fn system_time_from_i64(value: i64) -> StateStoreResult<SystemTime> {
    if value >= 0 {
        UNIX_EPOCH
            .checked_add(Duration::from_nanos(value as u64))
            .ok_or_else(|| corrupt("stored timestamp is outside SystemTime range"))
    } else {
        UNIX_EPOCH
            .checked_sub(Duration::from_nanos(value.unsigned_abs()))
            .ok_or_else(|| corrupt("stored timestamp is outside SystemTime range"))
    }
}

fn optional_system_time_from_i64(value: Option<i64>) -> StateStoreResult<Option<SystemTime>> {
    value.map(system_time_from_i64).transpose()
}

fn to_i64(value: u64, field: &str) -> StateStoreResult<i64> {
    i64::try_from(value).map_err(|_| corrupt(format!("{field} is outside PostgreSQL BIGINT range")))
}

fn from_i64(value: i64, field: &str) -> StateStoreResult<u64> {
    u64::try_from(value).map_err(|_| corrupt(format!("{field} cannot be negative")))
}

fn database_write_error(operation: &str, error: postgres::Error) -> StateStoreError {
    if error
        .as_db_error()
        .is_some_and(|error| error.code() == &SqlState::UNIQUE_VIOLATION)
    {
        StateStoreError::Backend(format!("unique violation: {operation}"))
    } else {
        backend(operation, error)
    }
}

fn backend(operation: &str, error: postgres::Error) -> StateStoreError {
    StateStoreError::Backend(format!("{operation}: {error}"))
}

fn corrupt(message: impl Into<String>) -> StateStoreError {
    StateStoreError::Backend(format!(
        "corrupt PostgreSQL execution state: {}",
        message.into()
    ))
}
