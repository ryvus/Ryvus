use std::{
    env,
    sync::{Arc, Barrier},
    thread,
    time::{Duration, SystemTime},
};

use postgres::{error::SqlState, Client, NoTls};
use ryvus_execution::{
    action_revision, ActorRef, AttemptOwnership, AttemptRecord, ExecutionAggregate,
    ExecutionIdentityFactory, ExecutionMutation, ExecutionPolicy, ExecutionResult,
    ExecutionScopeId, ExecutionState, ExecutionStateStore, MemoryExecutionStateStore, NewExecution,
    RetryPolicy, StateStoreError, TerminalState, TransitionResult,
};
use ryvus_persistence::{migrate, PostgresExecutionStateStore, PostgresScheduleStore};
use ryvus_protocol::{
    ActionDefinition, ActionExecutionPolicy, ActionKind, ApiAction, AttemptId, AttemptOutcome,
    ExecutionAttempt, ExecutionId, InvocationContext, InvocationEvent, InvocationRequest,
    InvocationResult, LogEvent, LogLevel, RuntimeHostId, RuntimeKind, RuntimeSessionId, WorkerId,
};
use ryvus_scheduler::{
    ClaimOccurrenceRequest, ClaimOccurrenceResult, DiscoveredSchedule, MemoryScheduleStore,
    ScheduleAvailability, ScheduleEnablement, ScheduleQuery, ScheduleStore, TriggerQuery,
};
use serde_json::json;
use url::Url;
use uuid::Uuid;

const ADMIN_URL_ENV: &str = "RYVUS_POSTGRES_TEST_ADMIN_URL";
const TEST_DATABASE_PREFIX: &str = "ryvus_test_";

struct TestDatabase {
    admin_url: String,
    database_url: String,
    name: String,
}

impl TestDatabase {
    fn create() -> Result<Self, String> {
        let admin_url = env::var(ADMIN_URL_ENV)
            .map_err(|_| format!("{ADMIN_URL_ENV} is required for postgres-integration tests"))?;
        let name = format!("{TEST_DATABASE_PREFIX}{}", Uuid::new_v4().simple());
        let quoted_name = quote_database_name(&name)?;
        let database_url = target_database_url(&admin_url, &name)?;
        let mut admin = Client::connect(&admin_url, NoTls)
            .map_err(|error| format!("connect to PostgreSQL administrator database: {error}"))?;
        admin
            .batch_execute(&format!("CREATE DATABASE {quoted_name}"))
            .map_err(|error| format!("create PostgreSQL test database '{name}': {error}"))?;
        eprintln!("created PostgreSQL integration database {name}");
        Ok(Self {
            admin_url,
            database_url,
            name,
        })
    }

    fn url(&self) -> &str {
        &self.database_url
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        let cleanup = (|| -> Result<(), String> {
            let quoted_name = quote_database_name(&self.name)?;
            let mut admin = Client::connect(&self.admin_url, NoTls)
                .map_err(|error| format!("connect for cleanup: {error}"))?;
            admin
                .execute(
                    "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
                     WHERE datname = $1 AND pid <> pg_backend_pid()",
                    &[&self.name],
                )
                .map_err(|error| format!("terminate test database connections: {error}"))?;
            admin
                .batch_execute(&format!("DROP DATABASE IF EXISTS {quoted_name}"))
                .map_err(|error| format!("drop test database: {error}"))?;
            Ok(())
        })();
        match cleanup {
            Ok(()) => eprintln!("dropped PostgreSQL integration database {}", self.name),
            Err(error) => eprintln!(
                "failed to clean PostgreSQL integration database {}: {error}",
                self.name
            ),
        }
    }
}

fn quote_database_name(name: &str) -> Result<String, String> {
    if name.len() > 63
        || !name.starts_with(TEST_DATABASE_PREFIX)
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err("generated PostgreSQL test database name is invalid".into());
    }
    Ok(format!("\"{name}\""))
}

fn target_database_url(admin_url: &str, database_name: &str) -> Result<String, String> {
    quote_database_name(database_name)?;
    let mut url = Url::parse(admin_url)
        .map_err(|error| format!("parse {ADMIN_URL_ENV} as a URL: {error}"))?;
    if !matches!(url.scheme(), "postgres" | "postgresql") {
        return Err(format!(
            "{ADMIN_URL_ENV} must use postgres or postgresql scheme"
        ));
    }
    url.set_path(&format!("/{database_name}"));
    Ok(url.into())
}

#[test]
fn postgres_integration_suite() {
    let db = TestDatabase::create().unwrap_or_else(|error| panic!("{error}"));
    validate_url_construction();

    eprintln!("phase: migration validation");
    validate_migrations(db.url());

    eprintln!("phase: provider contract (memory)");
    run_provider_contract(&MemoryExecutionStateStore::default());
    eprintln!("phase: provider contract (PostgreSQL)");
    {
        let store = PostgresExecutionStateStore::connect(db.url()).unwrap();
        run_provider_contract(&store);
    }

    eprintln!("phase: schedule provider contract (memory)");
    run_schedule_provider_contract(&MemoryScheduleStore::default());
    eprintln!("phase: schedule provider contract (PostgreSQL)");
    {
        let store = PostgresScheduleStore::connect(db.url()).unwrap();
        run_schedule_provider_contract(&store);
    }

    eprintln!("phase: Tokio runtime compatibility");
    validate_tokio_runtime_compatibility(db.url());

    eprintln!("phase: restart validation");
    validate_restart(db.url());

    eprintln!("phase: CAS validation");
    validate_terminal_races(db.url());
    validate_retry_cancellation_race(db.url());
    validate_ownership_race(db.url());

    eprintln!("phase: rollback validation");
    validate_transaction_rollback(db.url());
}

fn run_schedule_provider_contract(store: &dyn ScheduleStore) {
    let factory = ExecutionIdentityFactory;
    let scope = ExecutionScopeId::new(format!("scope-{}", Uuid::new_v4())).unwrap();
    let observed_at = SystemTime::now();
    let action = ActionDefinition {
        runtime: RuntimeKind::Python,
        kind: ActionKind::Schedule(ryvus_protocol::ScheduleAction {
            key: "postgres-restock".into(),
            expression: "every 10s".into(),
        }),
        source: "src/restock.py".into(),
        entrypoint: "restock".into(),
        name: Some("restock".into()),
        policy: ActionExecutionPolicy::default(),
    };
    let schedule_id = factory.schedule_id(&scope, "postgres-restock");
    let discovered = DiscoveredSchedule {
        schedule_id: schedule_id.clone(),
        stable_schedule_key: "postgres-restock".into(),
        display_name: "restock".into(),
        action_id: "restock".into(),
        action_revision: action_revision(&action).unwrap(),
        action,
        expression: "every 10s".into(),
        interval: Duration::from_secs(10),
    };
    assert_eq!(
        store
            .reconcile(&scope, std::slice::from_ref(&discovered), observed_at)
            .unwrap()
            .created,
        1
    );
    assert_eq!(
        store
            .reconcile(&scope, std::slice::from_ref(&discovered), observed_at)
            .unwrap()
            .updated,
        0
    );
    let actor = ActorRef::new("contract-actor").unwrap();
    assert_eq!(
        store
            .disable(&schedule_id, &actor, observed_at)
            .unwrap()
            .enablement,
        ScheduleEnablement::Disabled
    );
    store
        .reconcile(&scope, std::slice::from_ref(&discovered), observed_at)
        .unwrap();
    assert_eq!(
        store
            .get_schedule(&schedule_id)
            .unwrap()
            .unwrap()
            .enablement,
        ScheduleEnablement::Disabled
    );

    let mut changed = discovered.clone();
    changed.expression = "every 20s".into();
    changed.interval = Duration::from_secs(20);
    changed.action.kind = ActionKind::Schedule(ryvus_protocol::ScheduleAction {
        key: "postgres-restock".into(),
        expression: changed.expression.clone(),
    });
    changed.action_revision = action_revision(&changed.action).unwrap();
    assert_eq!(
        store
            .reconcile(&scope, std::slice::from_ref(&changed), observed_at)
            .unwrap()
            .updated,
        1
    );
    assert_eq!(store.list_revisions(&schedule_id).unwrap().len(), 2);
    assert_eq!(
        store
            .reconcile(&scope, &[], observed_at)
            .unwrap()
            .unavailable,
        1
    );
    let unavailable = store.get_schedule(&schedule_id).unwrap().unwrap();
    assert_eq!(unavailable.availability, ScheduleAvailability::Unavailable);
    assert_eq!(unavailable.enablement, ScheduleEnablement::Disabled);
    assert!(store.list_due(&scope, observed_at, 10).unwrap().is_empty());
    store
        .reconcile(&scope, std::slice::from_ref(&changed), observed_at)
        .unwrap();
    let rediscovered = store.get_schedule(&schedule_id).unwrap().unwrap();
    assert_eq!(rediscovered.availability, ScheduleAvailability::Available);
    assert_eq!(rediscovered.enablement, ScheduleEnablement::Disabled);
    assert_eq!(rediscovered.current_revision, 2);
    store.enable(&schedule_id, &actor, observed_at).unwrap();

    let due_at = observed_at + Duration::from_secs(20);
    let due = store.list_due(&scope, due_at, 10).unwrap().remove(0);
    let trigger_id = factory
        .scheduled_trigger(&scope, &schedule_id, 2, due.scheduled_for)
        .unwrap();
    let execution_id = factory
        .scheduled_execution(&scope, &schedule_id, 2, due.scheduled_for)
        .unwrap();
    let claim = ClaimOccurrenceRequest {
        execution_scope_id: scope.clone(),
        schedule_id: schedule_id.clone(),
        schedule_version: due.schedule.version,
        schedule_revision: 2,
        trigger_id: trigger_id.clone(),
        execution_id: Some(execution_id),
        scheduled_for: due.scheduled_for,
        observed_at: due_at,
        owner: "contract".into(),
        lease: Duration::from_secs(30),
    };
    assert!(matches!(
        store.claim_occurrence(claim.clone()).unwrap(),
        ClaimOccurrenceResult::Claimed(_)
    ));
    assert!(matches!(
        store.claim_occurrence(claim).unwrap(),
        ClaimOccurrenceResult::Busy
    ));
    assert_eq!(
        store
            .list_schedules(ScheduleQuery {
                execution_scope_id: Some(scope),
                limit: 10,
            })
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .list_triggers(TriggerQuery {
                schedule_id,
                kind: None,
                limit: 10,
            })
            .unwrap()
            .len(),
        1
    );
}

fn validate_tokio_runtime_compatibility(url: &str) {
    let url = url.to_owned();
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(async move {
            let store = PostgresExecutionStateStore::connect(&url).unwrap();
            let created = store.create(new_execution()).unwrap();
            assert_eq!(store.load(&created.execution_id).unwrap(), Some(created));
            drop(store);
        });
}

fn validate_url_construction() {
    let target = target_database_url(
        "postgres://user:secret@localhost:55432/postgres?application_name=ryvus&sslmode=disable",
        "ryvus_test_a1b2c3",
    )
    .unwrap();
    let parsed = Url::parse(&target).unwrap();
    assert_eq!(parsed.path(), "/ryvus_test_a1b2c3");
    assert_eq!(
        parsed.query(),
        Some("application_name=ryvus&sslmode=disable")
    );
}

fn validate_migrations(url: &str) {
    let barrier = Arc::new(Barrier::new(3));
    let migrations = (0..2)
        .map(|_| {
            let url = url.to_owned();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                migrate(&url)
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    for migration in migrations {
        migration.join().unwrap().unwrap();
    }
    migrate(url).unwrap();

    let mut client = Client::connect(url, NoTls).unwrap();
    let versions = client
        .query(
            "SELECT version FROM ryvus_schema_migrations ORDER BY version",
            &[],
        )
        .unwrap()
        .into_iter()
        .map(|row| row.get::<_, i64>(0))
        .collect::<Vec<_>>();
    assert_eq!(versions, vec![1, 2]);

    client
        .batch_execute(
            "ALTER TABLE ryvus_schema_migrations \
                 RENAME TO ryvus_schema_migrations_valid; \
             CREATE TABLE ryvus_schema_migrations (broken TEXT NOT NULL)",
        )
        .unwrap();
    drop(client);
    assert!(matches!(
        migrate(url),
        Err(StateStoreError::BackendCode { code, .. })
            if code == SqlState::UNDEFINED_COLUMN.code()
    ));
    let mut client = Client::connect(url, NoTls).unwrap();
    client
        .batch_execute(
            "DROP TABLE ryvus_schema_migrations; \
             ALTER TABLE ryvus_schema_migrations_valid \
                 RENAME TO ryvus_schema_migrations",
        )
        .unwrap();
    let execution_count = client
        .query_one("SELECT COUNT(*) FROM ryvus_executions", &[])
        .unwrap()
        .get::<_, i64>(0);
    assert_eq!(execution_count, 0);
    drop(client);
    migrate(url).unwrap();
}

fn run_provider_contract(store: &dyn ExecutionStateStore) {
    let new = new_execution();
    let expected_revision = new.action_revision.clone();
    let created = store.create(new.clone()).unwrap();
    assert_eq!(created.execution_version, 0);
    assert_eq!(created.action_revision, expected_revision);
    assert_eq!(
        store.load(&created.execution_id).unwrap(),
        Some(created.clone())
    );
    assert!(matches!(
        store.create(new),
        Err(StateStoreError::AlreadyExists { .. })
    ));

    let first = attempt_with_id(&created.execution_id, AttemptId::new(), 1);
    let running = applied(
        store
            .compare_and_set(
                &created.execution_id,
                0,
                ExecutionMutation::StartAttempt {
                    attempt: first.clone(),
                },
            )
            .unwrap(),
    );
    assert_eq!(running.execution_version, 1);
    assert!(matches!(
        store
            .compare_and_set(
                &created.execution_id,
                0,
                ExecutionMutation::StartAttempt {
                    attempt: attempt(&created.execution_id, 2),
                },
            )
            .unwrap(),
        TransitionResult::Conflict { current_version: 1 }
    ));
    let after_conflict = store.load(&created.execution_id).unwrap().unwrap();
    assert_eq!(after_conflict.execution_version, running.execution_version);
    assert_eq!(after_conflict.attempts, running.attempts);

    let first_ownership = ownership(&first, "session-1");
    let assigned = applied(
        store
            .compare_and_set(
                &created.execution_id,
                running.execution_version,
                ExecutionMutation::AssignOwnership {
                    attempt_id: first.attempt.attempt_id.clone(),
                    ownership: first_ownership.clone(),
                },
            )
            .unwrap(),
    );
    assert!(matches!(
        store
            .compare_and_set(
                &created.execution_id,
                assigned.execution_version,
                ExecutionMutation::AssignOwnership {
                    attempt_id: first.attempt.attempt_id.clone(),
                    ownership: first_ownership,
                },
            )
            .unwrap(),
        TransitionResult::Unchanged { aggregate }
            if aggregate.execution_version == assigned.execution_version
    ));
    let replacement = ownership(&first, "session-2");
    let replaced = applied(
        store
            .compare_and_set(
                &created.execution_id,
                assigned.execution_version,
                ExecutionMutation::AssignOwnership {
                    attempt_id: first.attempt.attempt_id.clone(),
                    ownership: replacement.clone(),
                },
            )
            .unwrap(),
    );
    assert_eq!(replaced.execution_version, assigned.execution_version + 1);
    assert_eq!(replaced.attempts[0].ownership, Some(replacement));

    let cancelled = applied(
        store
            .compare_and_set(
                &created.execution_id,
                replaced.execution_version,
                ExecutionMutation::RequestCancellation {
                    requested_at: SystemTime::now(),
                },
            )
            .unwrap(),
    );
    assert!(matches!(
        store
            .compare_and_set(
                &created.execution_id,
                cancelled.execution_version,
                ExecutionMutation::RequestCancellation {
                    requested_at: SystemTime::now(),
                },
            )
            .unwrap(),
        TransitionResult::Unchanged { aggregate }
            if aggregate.execution_version == cancelled.execution_version
    ));
    assert!(store
        .reconcilable_cancellations()
        .unwrap()
        .iter()
        .any(|aggregate| aggregate.execution_id == created.execution_id));
    assert!(store
        .active_executions()
        .unwrap()
        .iter()
        .any(|aggregate| aggregate.execution_id == created.execution_id));

    let terminal = applied(
        store
            .compare_and_set(
                &created.execution_id,
                cancelled.execution_version,
                terminal_mutation(&first, AttemptOutcome::Cancelled, ExecutionState::Cancelled),
            )
            .unwrap(),
    );
    assert_eq!(terminal.state, ExecutionState::Cancelled);
    assert!(store
        .compare_and_set(
            &created.execution_id,
            terminal.execution_version,
            ExecutionMutation::RequestCancellation {
                requested_at: SystemTime::now(),
            },
        )
        .is_err());
    assert!(store
        .compare_and_set(
            &created.execution_id,
            terminal.execution_version,
            terminal_mutation(&first, AttemptOutcome::Cancelled, ExecutionState::Cancelled),
        )
        .is_err());
    assert!(!store
        .reconcilable_cancellations()
        .unwrap()
        .iter()
        .any(|aggregate| aggregate.execution_id == created.execution_id));

    validate_retry_contract(store);
    validate_structured_result_contract(store);
}

fn validate_retry_contract(store: &dyn ExecutionStateStore) {
    let created = store.create(new_execution()).unwrap();
    let first = attempt(&created.execution_id, 1);
    let running = applied(
        store
            .compare_and_set(
                &created.execution_id,
                0,
                ExecutionMutation::StartAttempt {
                    attempt: first.clone(),
                },
            )
            .unwrap(),
    );
    let retry = attempt(&created.execution_id, 2);
    let pending = applied(
        store
            .compare_and_set(
                &created.execution_id,
                running.execution_version,
                ExecutionMutation::FinishAttempt {
                    attempt_id: first.attempt.attempt_id.clone(),
                    outcome: AttemptOutcome::Failed,
                    result: None,
                    retry: Some(retry.clone()),
                    terminal: None,
                },
            )
            .unwrap(),
    );
    assert_eq!(pending.state, ExecutionState::Pending);
    assert_eq!(pending.attempts.len(), 2);
    assert_eq!(pending.attempts[0].outcome, Some(AttemptOutcome::Failed));
    assert_eq!(pending.attempts[1], retry);
    assert_eq!(store.load(&created.execution_id).unwrap(), Some(pending));
}

fn validate_structured_result_contract(store: &dyn ExecutionStateStore) {
    let created = store.create(new_execution()).unwrap();
    let first = attempt(&created.execution_id, 1);
    let running = applied(
        store
            .compare_and_set(
                &created.execution_id,
                0,
                ExecutionMutation::StartAttempt {
                    attempt: first.clone(),
                },
            )
            .unwrap(),
    );
    let result = structured_result(&first.attempt);
    let finished = applied(
        store
            .compare_and_set(
                &created.execution_id,
                running.execution_version,
                ExecutionMutation::FinishAttempt {
                    attempt_id: first.attempt.attempt_id.clone(),
                    outcome: AttemptOutcome::Succeeded,
                    result: Some(result.clone()),
                    retry: None,
                    terminal: Some(TerminalState::new(
                        ExecutionState::Succeeded,
                        Some(first.attempt.attempt_id.clone()),
                    )),
                },
            )
            .unwrap(),
    );
    let reloaded = store.load(&created.execution_id).unwrap().unwrap();
    assert_eq!(reloaded, finished);
    assert_eq!(reloaded.attempts[0].result, Some(result));
}

fn validate_restart(url: &str) {
    let aggregate = {
        let store = PostgresExecutionStateStore::connect(url).unwrap();
        let created = store.create(new_execution()).unwrap();
        let first = attempt(&created.execution_id, 1);
        let running = applied(
            store
                .compare_and_set(
                    &created.execution_id,
                    0,
                    ExecutionMutation::StartAttempt {
                        attempt: first.clone(),
                    },
                )
                .unwrap(),
        );
        applied(
            store
                .compare_and_set(
                    &created.execution_id,
                    running.execution_version,
                    ExecutionMutation::AssignOwnership {
                        attempt_id: first.attempt.attempt_id.clone(),
                        ownership: ownership(&first, "restart-session"),
                    },
                )
                .unwrap(),
        )
    };
    let restarted = PostgresExecutionStateStore::connect(url).unwrap();
    assert_eq!(
        restarted.load(&aggregate.execution_id).unwrap(),
        Some(aggregate)
    );
}

fn validate_terminal_races(url: &str) {
    run_terminal_race(
        url,
        (AttemptOutcome::Succeeded, ExecutionState::Succeeded),
        (AttemptOutcome::Cancelled, ExecutionState::Cancelled),
    );
    run_terminal_race(
        url,
        (AttemptOutcome::TimedOut, ExecutionState::TimedOut),
        (AttemptOutcome::Succeeded, ExecutionState::Succeeded),
    );
}

fn run_terminal_race(
    url: &str,
    left: (AttemptOutcome, ExecutionState),
    right: (AttemptOutcome, ExecutionState),
) {
    let seed = PostgresExecutionStateStore::connect(url).unwrap();
    let created = seed.create(new_execution()).unwrap();
    let first = attempt(&created.execution_id, 1);
    let running = applied(
        seed.compare_and_set(
            &created.execution_id,
            0,
            ExecutionMutation::StartAttempt {
                attempt: first.clone(),
            },
        )
        .unwrap(),
    );
    drop(seed);

    let barrier = Arc::new(Barrier::new(3));
    let spawn = |(outcome, state)| {
        let url = url.to_owned();
        let barrier = Arc::clone(&barrier);
        let execution_id = created.execution_id.clone();
        let attempt = first.clone();
        thread::spawn(move || {
            let store = PostgresExecutionStateStore::connect(&url).unwrap();
            barrier.wait();
            store
                .compare_and_set(
                    &execution_id,
                    running.execution_version,
                    terminal_mutation(&attempt, outcome, state),
                )
                .unwrap()
        })
    };
    let left = spawn(left);
    let right = spawn(right);
    barrier.wait();
    assert_one_applied_one_conflict([left.join().unwrap(), right.join().unwrap()]);

    let store = PostgresExecutionStateStore::connect(url).unwrap();
    let final_aggregate = store.load(&created.execution_id).unwrap().unwrap();
    assert!(final_aggregate.terminal_state.is_some());
    assert!(store
        .compare_and_set(
            &created.execution_id,
            final_aggregate.execution_version,
            ExecutionMutation::RequestCancellation {
                requested_at: SystemTime::now(),
            },
        )
        .is_err());
}

fn validate_retry_cancellation_race(url: &str) {
    let seed = PostgresExecutionStateStore::connect(url).unwrap();
    let created = seed.create(new_execution()).unwrap();
    let first = attempt(&created.execution_id, 1);
    let running = applied(
        seed.compare_and_set(
            &created.execution_id,
            0,
            ExecutionMutation::StartAttempt {
                attempt: first.clone(),
            },
        )
        .unwrap(),
    );
    drop(seed);

    let barrier = Arc::new(Barrier::new(3));
    let retry = {
        let url = url.to_owned();
        let barrier = Arc::clone(&barrier);
        let execution_id = created.execution_id.clone();
        let first = first.clone();
        thread::spawn(move || {
            let store = PostgresExecutionStateStore::connect(&url).unwrap();
            barrier.wait();
            store
                .compare_and_set(
                    &execution_id,
                    running.execution_version,
                    ExecutionMutation::FinishAttempt {
                        attempt_id: first.attempt.attempt_id,
                        outcome: AttemptOutcome::Failed,
                        result: None,
                        retry: Some(attempt(&execution_id, 2)),
                        terminal: None,
                    },
                )
                .unwrap()
        })
    };
    let cancellation = {
        let url = url.to_owned();
        let barrier = Arc::clone(&barrier);
        let execution_id = created.execution_id.clone();
        thread::spawn(move || {
            let store = PostgresExecutionStateStore::connect(&url).unwrap();
            barrier.wait();
            store
                .compare_and_set(
                    &execution_id,
                    running.execution_version,
                    ExecutionMutation::RequestCancellation {
                        requested_at: SystemTime::now(),
                    },
                )
                .unwrap()
        })
    };
    barrier.wait();
    assert_one_applied_one_conflict([retry.join().unwrap(), cancellation.join().unwrap()]);
    let aggregate = PostgresExecutionStateStore::connect(url)
        .unwrap()
        .load(&created.execution_id)
        .unwrap()
        .unwrap();
    assert!(
        (aggregate.cancellation_intent.is_some() && aggregate.attempts.len() == 1)
            || (aggregate.cancellation_intent.is_none() && aggregate.attempts.len() == 2)
    );
}

fn validate_ownership_race(url: &str) {
    let seed = PostgresExecutionStateStore::connect(url).unwrap();
    let created = seed.create(new_execution()).unwrap();
    let first = attempt(&created.execution_id, 1);
    let running = applied(
        seed.compare_and_set(
            &created.execution_id,
            0,
            ExecutionMutation::StartAttempt {
                attempt: first.clone(),
            },
        )
        .unwrap(),
    );
    let assigned = applied(
        seed.compare_and_set(
            &created.execution_id,
            running.execution_version,
            ExecutionMutation::AssignOwnership {
                attempt_id: first.attempt.attempt_id.clone(),
                ownership: ownership(&first, "initial-session"),
            },
        )
        .unwrap(),
    );
    drop(seed);

    let barrier = Arc::new(Barrier::new(3));
    let spawn = |session: &'static str| {
        let url = url.to_owned();
        let barrier = Arc::clone(&barrier);
        let execution_id = created.execution_id.clone();
        let first = first.clone();
        thread::spawn(move || {
            let store = PostgresExecutionStateStore::connect(&url).unwrap();
            barrier.wait();
            store
                .compare_and_set(
                    &execution_id,
                    assigned.execution_version,
                    ExecutionMutation::AssignOwnership {
                        attempt_id: first.attempt.attempt_id.clone(),
                        ownership: ownership(&first, session),
                    },
                )
                .unwrap()
        })
    };
    let left = spawn("replacement-a");
    let right = spawn("replacement-b");
    barrier.wait();
    assert_one_applied_one_conflict([left.join().unwrap(), right.join().unwrap()]);
}

fn validate_transaction_rollback(url: &str) {
    let store = PostgresExecutionStateStore::connect(url).unwrap();
    let shared_attempt_id = AttemptId::new();

    let first_execution = store.create(new_execution()).unwrap();
    applied(
        store
            .compare_and_set(
                &first_execution.execution_id,
                0,
                ExecutionMutation::StartAttempt {
                    attempt: attempt_with_id(
                        &first_execution.execution_id,
                        shared_attempt_id.clone(),
                        1,
                    ),
                },
            )
            .unwrap(),
    );

    let second_execution = store.create(new_execution()).unwrap();
    let before = store.load(&second_execution.execution_id).unwrap().unwrap();
    assert!(matches!(
        store.compare_and_set(
            &second_execution.execution_id,
            before.execution_version,
            ExecutionMutation::StartAttempt {
                attempt: attempt_with_id(
                    &second_execution.execution_id,
                    shared_attempt_id,
                    1,
                ),
            },
        ),
        Err(StateStoreError::BackendCode { code, .. })
            if code == SqlState::UNIQUE_VIOLATION.code()
    ));
    assert_eq!(
        store.load(&second_execution.execution_id).unwrap(),
        Some(before.clone())
    );
    let recovered = applied(
        store
            .compare_and_set(
                &second_execution.execution_id,
                before.execution_version,
                ExecutionMutation::StartAttempt {
                    attempt: attempt(&second_execution.execution_id, 1),
                },
            )
            .unwrap(),
    );
    assert_eq!(recovered.execution_version, before.execution_version + 1);
    assert_eq!(recovered.attempts.len(), 1);
}

fn assert_one_applied_one_conflict(results: [TransitionResult; 2]) {
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, TransitionResult::Applied { .. }))
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, TransitionResult::Conflict { .. }))
            .count(),
        1
    );
}

fn terminal_mutation(
    attempt: &AttemptRecord,
    outcome: AttemptOutcome,
    state: ExecutionState,
) -> ExecutionMutation {
    ExecutionMutation::FinishAttempt {
        attempt_id: attempt.attempt.attempt_id.clone(),
        outcome,
        result: None,
        retry: None,
        terminal: Some(TerminalState::new(
            state,
            Some(attempt.attempt.attempt_id.clone()),
        )),
    }
}

fn new_execution() -> NewExecution {
    NewExecution {
        action: ActionDefinition {
            runtime: RuntimeKind::Python,
            kind: ActionKind::Api(ApiAction {
                method: "POST".into(),
                path: "/postgres".into(),
                consumes: vec!["application/json".into()],
                produces: vec!["application/json".into()],
                request_schema: None,
                response_schema: None,
                query_params: Vec::new(),
                authorizer: None,
            }),
            source: "src/postgres.py".into(),
            entrypoint: "postgres".into(),
            name: Some("postgres".into()),
            policy: ActionExecutionPolicy::default(),
        },
        action_revision: "postgres-test-revision".into(),
        execution_scope_id: ryvus_execution::ExecutionScopeId::new("test").unwrap(),
        action_id: "postgres".into(),
        trigger: ryvus_execution::ExecutionTrigger::Api,
        creation_fingerprint: "postgres-test-fingerprint".into(),
        data_refs: ryvus_execution::ExecutionDataReferences::default(),
        request: InvocationRequest::new(json!({ "provider": "postgres" })),
        policy: ExecutionPolicy {
            timeout: Duration::from_secs(3),
            retry: RetryPolicy {
                max_attempts: 2,
                initial_delay: Duration::from_millis(10),
                backoff: 2.0,
            },
        },
        created_at: SystemTime::now(),
    }
}

fn attempt(execution_id: &ExecutionId, number: u32) -> AttemptRecord {
    attempt_with_id(execution_id, AttemptId::new(), number)
}

fn attempt_with_id(
    execution_id: &ExecutionId,
    attempt_id: AttemptId,
    number: u32,
) -> AttemptRecord {
    AttemptRecord::pending(
        ExecutionAttempt {
            execution_id: execution_id.clone(),
            attempt_id,
            attempt_number: number,
        },
        10_000 + i64::from(number),
    )
}

fn ownership(attempt: &AttemptRecord, session: &str) -> AttemptOwnership {
    AttemptOwnership {
        execution_id: attempt.attempt.execution_id.clone(),
        attempt_id: attempt.attempt.attempt_id.clone(),
        attempt_number: attempt.attempt.attempt_number,
        runtime_host_id: RuntimeHostId::from("postgres-host"),
        runtime_session_id: RuntimeSessionId::from(session),
        worker_id: WorkerId::from("postgres-worker"),
    }
}

fn applied(result: TransitionResult) -> ExecutionAggregate {
    match result {
        TransitionResult::Applied { aggregate } => aggregate,
        other => panic!("expected applied transition, got {other:?}"),
    }
}

fn structured_result(attempt: &ExecutionAttempt) -> ExecutionResult {
    let request = InvocationRequest::with_attempt(
        json!({ "structured": true }),
        InvocationContext::default(),
        attempt.clone(),
    );
    ExecutionResult {
        invocation_result: InvocationResult::success(&request, json!({ "ok": true })),
        events: vec![InvocationEvent::Log(LogEvent {
            execution_id: attempt.execution_id.clone(),
            attempt_id: attempt.attempt_id.clone(),
            attempt_number: attempt.attempt_number,
            level: LogLevel::Info,
            message: "postgres structured log".into(),
            fields: json!({ "provider": "postgres" }),
        })],
        stdout: String::new(),
        stderr: String::new(),
        duration: Duration::from_millis(7),
        exit_code: Some(0),
    }
}
