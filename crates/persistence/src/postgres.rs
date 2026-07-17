use std::{
    sync::{
        mpsc::{self, Sender},
        Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use postgres::{Client, NoTls, Row, Transaction};
use ryvus_execution::{
    aggregate_from_new, apply_mutation, validate_execution_aggregate, validate_new_execution,
    AttemptRecord, CancellationIntent, CreateExecutionResult, ExecutionAggregate,
    ExecutionHistoryPage, ExecutionHistoryQuery, ExecutionMutation, ExecutionState,
    ExecutionStateStore, NewExecution, StateStoreError, StateStoreResult, TerminalState,
    TransitionResult,
};
use ryvus_protocol::{AttemptId, AttemptOutcome, ExecutionAttempt, ExecutionId};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;

const MIGRATION_LOCK_ID: i64 = 7_823_981_045_710_001;
const MIGRATIONS: &[(i64, &str)] = &[
    (1, include_str!("../migrations/0001_execution_state.sql")),
    (2, include_str!("../migrations/0002_execution_history.sql")),
];

pub struct PostgresExecutionStateStore {
    commands: Option<Sender<ClientCommand>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

type ClientCommand = Box<dyn FnOnce(&mut Client) + Send>;

impl PostgresExecutionStateStore {
    pub fn connect(database_url: &str) -> StateStoreResult<Self> {
        let database_url = database_url.to_owned();
        let (commands, receiver) = mpsc::channel::<ClientCommand>();
        let (startup_sender, startup_receiver) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("ryvus-postgres-store".into())
            .spawn(move || {
                let mut client = match Client::connect(&database_url, NoTls) {
                    Ok(client) => client,
                    Err(error) => {
                        let _ = startup_sender.send(Err(backend("connect to PostgreSQL", error)));
                        return;
                    }
                };
                if startup_sender.send(Ok(())).is_err() {
                    return;
                }
                while let Ok(command) = receiver.recv() {
                    command(&mut client);
                }
            })
            .map_err(|error| {
                StateStoreError::Backend(format!("start PostgreSQL store worker: {error}"))
            })?;
        if let Err(error) = startup_receiver
            .recv()
            .map_err(|_| StateStoreError::Backend("PostgreSQL store worker stopped".into()))?
        {
            let _ = worker.join();
            return Err(error);
        }
        Ok(Self {
            commands: Some(commands),
            worker: Mutex::new(Some(worker)),
        })
    }

    fn run<T>(
        &self,
        operation: impl FnOnce(&mut Client) -> StateStoreResult<T> + Send + 'static,
    ) -> StateStoreResult<T>
    where
        T: Send + 'static,
    {
        let (result_sender, result_receiver) = mpsc::sync_channel(1);
        let command = Box::new(move |client: &mut Client| {
            let _ = result_sender.send(operation(client));
        });
        self.commands
            .as_ref()
            .ok_or_else(|| StateStoreError::Backend("PostgreSQL store is closed".into()))?
            .send(command)
            .map_err(|_| StateStoreError::Backend("PostgreSQL store worker stopped".into()))?;
        result_receiver
            .recv()
            .map_err(|_| StateStoreError::Backend("PostgreSQL store worker stopped".into()))?
    }
}

impl Drop for PostgresExecutionStateStore {
    fn drop(&mut self) {
        self.commands.take();
        let worker = self
            .worker
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(worker) = worker {
            let _ = worker.join();
        }
    }
}

fn transaction<T>(
    client: &mut Client,
    operation: impl FnOnce(&mut Transaction<'_>) -> StateStoreResult<T>,
) -> StateStoreResult<T> {
    let mut transaction = client
        .transaction()
        .map_err(|error| backend("begin PostgreSQL transaction", error))?;
    let value = operation(&mut transaction)?;
    transaction
        .commit()
        .map_err(|error| backend("commit PostgreSQL transaction", error))?;
    Ok(value)
}

impl ExecutionStateStore for PostgresExecutionStateStore {
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
        let execution_id = execution.request.execution_id.clone();
        let aggregate = aggregate_from_new(execution);

        self.run(move |client| {
            transaction(client, |transaction| {
                if insert_execution(transaction, &aggregate)? == 1 {
                    return Ok(CreateExecutionResult::Created(aggregate.clone()));
                }
                let existing = load_aggregate(transaction, &execution_id, "FOR SHARE")?
                    .ok_or_else(|| StateStoreError::NotFound {
                        execution_id: execution_id.clone(),
                    })?;
                if existing.creation_fingerprint == aggregate.creation_fingerprint {
                    Ok(CreateExecutionResult::Existing(existing))
                } else {
                    Err(StateStoreError::IdentityConflict {
                        execution_id: execution_id.clone(),
                    })
                }
            })
        })
    }

    fn load(&self, execution_id: &ExecutionId) -> StateStoreResult<Option<ExecutionAggregate>> {
        let execution_id = execution_id.clone();
        self.run(move |client| {
            transaction(client, |transaction| {
                load_aggregate(transaction, &execution_id, "FOR SHARE")
            })
        })
    }

    fn compare_and_set(
        &self,
        execution_id: &ExecutionId,
        expected_version: u64,
        mutation: ExecutionMutation,
    ) -> StateStoreResult<TransitionResult> {
        let execution_id = execution_id.clone();
        self.run(move |client| {
            transaction(client, |transaction| {
                let current = load_aggregate(transaction, &execution_id, "FOR UPDATE")?
                    .ok_or_else(|| StateStoreError::NotFound {
                        execution_id: execution_id.clone(),
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
        })
    }

    fn reconcilable_cancellations(&self) -> StateStoreResult<Vec<ExecutionAggregate>> {
        let mut aggregates = self.run(|client| {
            query_aggregates(
                client,
                "SELECT e.execution_id FROM ryvus_executions e \
             JOIN ryvus_cancellation_intents c USING (execution_id) \
             LEFT JOIN ryvus_terminal_states t USING (execution_id) \
             WHERE t.execution_id IS NULL ORDER BY e.execution_id",
            )
        })?;
        aggregates.retain(|aggregate| {
            aggregate.cancellation_intent.is_some() && aggregate.terminal_state.is_none()
        });
        Ok(aggregates)
    }

    fn active_executions(&self) -> StateStoreResult<Vec<ExecutionAggregate>> {
        let mut aggregates = self.run(|client| {
            query_aggregates(
                client,
                "SELECT execution_id FROM ryvus_executions \
             WHERE active_attempt_id IS NOT NULL ORDER BY execution_id",
            )
        })?;
        aggregates.retain(|aggregate| aggregate.active_attempt_id.is_some());
        Ok(aggregates)
    }

    fn list_history(&self, query: ExecutionHistoryQuery) -> StateStoreResult<ExecutionHistoryPage> {
        self.run(move |client| {
            transaction(client, |transaction| {
                let limit = query.limit.clamp(1, 100);
                let fetch_limit = i64::try_from(limit + 1).map_err(|_| {
                    StateStoreError::InvalidMutation("history limit overflow".into())
                })?;
                let (cursor_created_at, cursor_id) = if let Some(cursor) = query.cursor {
                    let cursor_id = cursor.to_string();
                    let row = transaction
                        .query_opt(
                            "SELECT created_at_unix_ns FROM ryvus_executions \
                             WHERE execution_id = $1 AND execution_scope_id = $2 \
                               AND ($3::TEXT IS NULL OR action_id = $3) \
                               AND ($4::TEXT IS NULL OR action_revision = $4)",
                            &[
                                &cursor_id,
                                &query.execution_scope_id.as_ref(),
                                &query.action_id,
                                &query.action_revision,
                            ],
                        )
                        .map_err(|error| backend("validate execution history cursor", error))?
                        .ok_or(StateStoreError::InvalidHistoryCursor { cursor })?;
                    (Some(row.get::<_, i64>(0)), Some(cursor_id))
                } else {
                    (None, None)
                };
                let rows = transaction
                    .query(
                        "SELECT execution_id FROM ryvus_executions \
                         WHERE execution_scope_id = $1 \
                           AND ($2::TEXT IS NULL OR action_id = $2) \
                           AND ($3::TEXT IS NULL OR action_revision = $3) \
                           AND ($4::BIGINT IS NULL OR (created_at_unix_ns, execution_id) < ($4, $5)) \
                         ORDER BY created_at_unix_ns DESC, execution_id DESC LIMIT $6",
                        &[
                            &query.execution_scope_id.as_ref(),
                            &query.action_id,
                            &query.action_revision,
                            &cursor_created_at,
                            &cursor_id,
                            &fetch_limit,
                        ],
                    )
                    .map_err(|error| backend("query execution history", error))?;
                let mut aggregates = Vec::with_capacity(rows.len());
                for row in rows {
                    let execution_id = ExecutionId::from(row.get::<_, String>(0));
                    aggregates.push(
                        load_aggregate(transaction, &execution_id, "FOR SHARE")?
                            .ok_or_else(|| corrupt("history execution disappeared"))?,
                    );
                }
                let has_more = aggregates.len() > limit;
                aggregates.truncate(limit);
                let next_cursor = has_more
                    .then(|| aggregates.last().map(|item| item.execution_id.clone()))
                    .flatten();
                Ok(ExecutionHistoryPage {
                    items: aggregates,
                    next_cursor,
                })
            })
        })
    }
}

fn query_aggregates(
    client: &mut Client,
    sql: &'static str,
) -> StateStoreResult<Vec<ExecutionAggregate>> {
    transaction(client, |transaction| {
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
) -> StateStoreResult<u64> {
    let action = json(&aggregate.action, "action")?;
    let request = json(&aggregate.request, "invocation request")?;
    let policy = json(&aggregate.policy, "execution policy")?;
    let trigger = json(&aggregate.trigger, "execution trigger")?;
    let data_refs = json(&aggregate.data_refs, "execution data references")?;
    let version = to_i64(aggregate.execution_version, "execution_version")?;
    let created_at = system_time_to_i64(aggregate.created_at)?;
    let updated_at = system_time_to_i64(aggregate.updated_at)?;
    transaction
        .execute(
            "INSERT INTO ryvus_executions (\
                 execution_id, action, action_revision, execution_scope_id, action_id, trigger, \
                 creation_fingerprint, data_refs, invocation_request, policy, state, \
                 active_attempt_id, created_at_unix_ns, updated_at_unix_ns, execution_version\
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15) \
             ON CONFLICT (execution_id) DO NOTHING",
            &[
                &aggregate.execution_id.as_ref(),
                &action,
                &aggregate.action_revision,
                &aggregate.execution_scope_id.as_ref(),
                &aggregate.action_id,
                &trigger,
                &aggregate.creation_fingerprint,
                &data_refs,
                &request,
                &policy,
                &state_name(aggregate.state),
                &aggregate.active_attempt_id.as_ref().map(AsRef::as_ref),
                &created_at,
                &updated_at,
                &version,
            ],
        )
        .map_err(|error| backend("insert execution", error))
}

fn update_execution(
    transaction: &mut Transaction<'_>,
    aggregate: &ExecutionAggregate,
    expected_version: u64,
) -> StateStoreResult<u64> {
    let action = json(&aggregate.action, "action")?;
    let request = json(&aggregate.request, "invocation request")?;
    let policy = json(&aggregate.policy, "execution policy")?;
    let data_refs = json(&aggregate.data_refs, "execution data references")?;
    let version = to_i64(aggregate.execution_version, "execution_version")?;
    let expected = to_i64(expected_version, "expected execution_version")?;
    let updated_at = system_time_to_i64(aggregate.updated_at)?;
    transaction
        .execute(
            "UPDATE ryvus_executions SET \
                 action = $2, action_revision = $3, invocation_request = $4, policy = $5, \
                 state = $6, active_attempt_id = $7, updated_at_unix_ns = $8, \
                 execution_version = $9, data_refs = $10 \
                 WHERE execution_id = $1 AND execution_version = $11",
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
                &data_refs,
                &expected,
            ],
        )
        .map_err(|error| backend("update execution", error))
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
    let data_refs = json(&attempt.data_refs, "attempt data references")?;
    let attempt_number = i64::from(attempt.attempt.attempt_number);
    let started_at = optional_system_time_to_i64(attempt.started_at)?;
    let finished_at = optional_system_time_to_i64(attempt.finished_at)?;
    transaction
        .execute(
            "INSERT INTO ryvus_attempts (\
                 execution_id, attempt_id, attempt_number, deadline_unix_ms, state, ownership, \
                 outcome, result, data_refs, started_at_unix_ns, finished_at_unix_ns\
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
            &[
                &attempt.attempt.execution_id.as_ref(),
                &attempt.attempt.attempt_id.as_ref(),
                &attempt_number,
                &attempt.deadline_unix_ms,
                &state_name(attempt.state),
                &ownership,
                &attempt.outcome.map(outcome_name),
                &result,
                &data_refs,
                &started_at,
                &finished_at,
            ],
        )
        .map_err(|error| backend("insert attempt", error))?;
    Ok(())
}

fn load_aggregate(
    transaction: &mut Transaction<'_>,
    execution_id: &ExecutionId,
    lock: &str,
) -> StateStoreResult<Option<ExecutionAggregate>> {
    let sql = format!(
        "SELECT execution_id, action, action_revision, execution_scope_id, action_id, trigger, \
             creation_fingerprint, data_refs, invocation_request, policy, state, active_attempt_id, \
             created_at_unix_ns, updated_at_unix_ns, execution_version \
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
                 outcome, result, data_refs, started_at_unix_ns, finished_at_unix_ns \
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
        execution_scope_id: ryvus_execution::ExecutionScopeId::new(
            row.get::<_, String>("execution_scope_id"),
        )
        .map_err(|error| corrupt(format!("invalid execution scope: {error}")))?,
        action_id: row.get("action_id"),
        trigger: from_json(row.get("trigger"), "execution trigger")?,
        creation_fingerprint: row.get("creation_fingerprint"),
        data_refs: from_json(row.get("data_refs"), "execution data references")?,
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
        data_refs: from_json(row.get("data_refs"), "attempt data references")?,
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

fn backend(operation: &str, error: postgres::Error) -> StateStoreError {
    match error.as_db_error() {
        Some(database_error) => StateStoreError::BackendCode {
            code: database_error.code().code().into(),
            message: format!("{operation}: {error}"),
        },
        None => StateStoreError::Backend(format!("{operation}: {error}")),
    }
}

fn corrupt(message: impl Into<String>) -> StateStoreError {
    StateStoreError::Backend(format!(
        "corrupt PostgreSQL execution state: {}",
        message.into()
    ))
}
