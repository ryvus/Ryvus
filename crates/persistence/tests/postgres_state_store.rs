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
    RetryPolicy, ScheduleId, StateStoreError, TerminalState, TransitionResult,
};
use ryvus_persistence::{migrate, PostgresExecutionStateStore, PostgresScheduleStore};
use ryvus_protocol::{
    ActionDefinition, ActionExecutionPolicy, ActionKind, ApiAction, AttemptId, AttemptOutcome,
    ExecutionAttempt, ExecutionId, InvocationContext, InvocationEvent, InvocationRequest,
    InvocationResult, LogEvent, LogLevel, MetricEvent, RuntimeHostId, RuntimeKind,
    RuntimeSessionId, WorkerId,
};
use ryvus_scheduler::{
    ClaimOccurrenceRequest, ClaimOccurrenceResult, DiscoveredSchedule, ManualTriggerRequest,
    ManualTriggerResult, MemoryScheduleStore, ScheduleAvailability, ScheduleEnablement,
    ScheduleOperationalEventKind, ScheduleQuery, ScheduleStore, ScheduleTriggerKind,
    ScheduleTriggerStatus, SchedulerError, TriggerFailure, TriggerQuery,
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

    eprintln!("phase: schedule concurrency");
    validate_schedule_concurrency(db.url());

    eprintln!("phase: schedule restart");
    validate_schedule_restart(db.url());

    eprintln!("phase: schedule rollback");
    validate_schedule_rollback(db.url());

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
                cursor: None,
                limit: 10,
            })
            .unwrap()
            .items
            .len(),
        1
    );
    assert_eq!(
        store
            .list_triggers(TriggerQuery {
                schedule_id,
                kind: None,
                cursor: None,
                limit: 10,
            })
            .unwrap()
            .items
            .len(),
        1
    );

    validate_schedule_definition_contract(store);
    validate_schedule_pagination_contract(store);
}

fn validate_schedule_definition_contract(store: &dyn ScheduleStore) {
    let factory = ExecutionIdentityFactory;
    let scope = unique_scope("schedule-definition-contract");
    let observed_at = SystemTime::now();
    let schedule = discovered_schedule(&factory, &scope, "definition", "every 10s", 10);
    store
        .reconcile(&scope, std::slice::from_ref(&schedule), observed_at)
        .unwrap();

    let mut renamed = schedule.clone();
    renamed.display_name = "Renamed definition".into();
    store
        .reconcile(
            &scope,
            std::slice::from_ref(&renamed),
            observed_at + Duration::from_secs(1),
        )
        .unwrap();
    assert_eq!(
        store
            .get_schedule(&schedule.schedule_id)
            .unwrap()
            .unwrap()
            .current_revision,
        1
    );

    let changed = changed_schedule(renamed, "every 20s", 20);
    assert_eq!(
        store
            .reconcile(
                &scope,
                std::slice::from_ref(&changed),
                observed_at + Duration::from_secs(2),
            )
            .unwrap()
            .updated,
        1
    );
    assert_eq!(
        store
            .reconcile(
                &scope,
                std::slice::from_ref(&changed),
                observed_at + Duration::from_secs(3),
            )
            .unwrap()
            .updated,
        0
    );
    assert_eq!(
        store.list_revisions(&schedule.schedule_id).unwrap().len(),
        2
    );

    let actor = ActorRef::new("contract-actor").unwrap();
    store
        .disable(
            &schedule.schedule_id,
            &actor,
            observed_at + Duration::from_secs(4),
        )
        .unwrap();
    store
        .reconcile(&scope, &[], observed_at + Duration::from_secs(5))
        .unwrap();
    store
        .reconcile(
            &scope,
            std::slice::from_ref(&changed),
            observed_at + Duration::from_secs(6),
        )
        .unwrap();
    let rediscovered = store.get_schedule(&schedule.schedule_id).unwrap().unwrap();
    assert_eq!(rediscovered.availability, ScheduleAvailability::Available);
    assert_eq!(rediscovered.enablement, ScheduleEnablement::Disabled);
    assert!(store
        .list_triggers(TriggerQuery {
            schedule_id: schedule.schedule_id.clone(),
            kind: None,
            cursor: None,
            limit: 10,
        })
        .unwrap()
        .items
        .is_empty());

    let enabled_at = observed_at + Duration::from_secs(60);
    let enabled = store
        .enable(&schedule.schedule_id, &actor, enabled_at)
        .unwrap();
    assert_eq!(enabled.enablement, ScheduleEnablement::Enabled);
    assert_eq!(
        enabled.next_trigger_at,
        Some(enabled_at + Duration::from_secs(20))
    );
    let events = store
        .list_operational_events(&schedule.schedule_id, 10)
        .unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].kind, ScheduleOperationalEventKind::Enabled);
    assert_eq!(events[1].kind, ScheduleOperationalEventKind::Disabled);

    let request = manual_request(
        &factory,
        &scope,
        &schedule.schedule_id,
        enabled_at,
        Some("definition-key"),
        "same-input",
    );
    assert!(matches!(
        store.create_manual_trigger(request.clone()).unwrap(),
        ManualTriggerResult::Created(_)
    ));
    let matching = ManualTriggerRequest {
        trigger_id: factory.random_trigger(),
        execution_id: factory.random_execution(),
        ..request.clone()
    };
    assert!(matches!(
        store.create_manual_trigger(matching).unwrap(),
        ManualTriggerResult::Existing(_)
    ));
    let conflicting = ManualTriggerRequest {
        trigger_id: factory.random_trigger(),
        execution_id: factory.random_execution(),
        immutable_request_fingerprint: "different-input".into(),
        ..request
    };
    assert!(matches!(
        store.create_manual_trigger(conflicting),
        Err(SchedulerError::Conflict(_))
    ));

    let due_at = enabled.next_trigger_at.unwrap();
    let due = store.list_due(&scope, due_at, 10).unwrap().remove(0);
    let trigger_id = factory
        .scheduled_trigger(
            &scope,
            &schedule.schedule_id,
            due.schedule.current_revision,
            due.scheduled_for,
        )
        .unwrap();
    let execution_id = factory
        .scheduled_execution(
            &scope,
            &schedule.schedule_id,
            due.schedule.current_revision,
            due.scheduled_for,
        )
        .unwrap();
    let ClaimOccurrenceResult::Claimed(claimed) = store
        .claim_occurrence(ClaimOccurrenceRequest {
            execution_scope_id: scope,
            schedule_id: schedule.schedule_id,
            schedule_version: due.schedule.version,
            schedule_revision: due.schedule.current_revision,
            trigger_id: trigger_id.clone(),
            execution_id: Some(execution_id.clone()),
            scheduled_for: due.scheduled_for,
            observed_at: due_at,
            owner: "contract".into(),
            lease: Duration::from_secs(30),
        })
        .unwrap()
    else {
        panic!("occurrence should be claimed")
    };
    let missed = store.miss_trigger(&trigger_id, claimed.version).unwrap();
    assert!(matches!(
        store.fail_trigger(
            &trigger_id,
            TriggerFailure {
                code: "failed".into(),
                summary: "failed".into(),
            },
            missed.version,
        ),
        Err(SchedulerError::Conflict(_))
    ));
    assert!(matches!(
        store.link_execution(&trigger_id, &execution_id, missed.version),
        Err(SchedulerError::Conflict(_))
    ));
    assert_eq!(
        store.get_trigger(&trigger_id).unwrap().unwrap().status,
        ScheduleTriggerStatus::Missed
    );
}

fn validate_schedule_pagination_contract(store: &dyn ScheduleStore) {
    let factory = ExecutionIdentityFactory;
    let scope = unique_scope("schedule-pagination-contract");
    let observed_at = SystemTime::now();
    let schedules = ["charlie", "alpha", "bravo"]
        .map(|key| discovered_schedule(&factory, &scope, key, "every 10s", 10));
    store.reconcile(&scope, &schedules, observed_at).unwrap();

    let other_scope = unique_scope("schedule-pagination-other");
    let other = discovered_schedule(&factory, &other_scope, "alpha", "every 10s", 10);
    store
        .reconcile(&other_scope, std::slice::from_ref(&other), observed_at)
        .unwrap();
    assert!(matches!(
        store.list_schedules(ScheduleQuery {
            execution_scope_id: Some(scope.clone()),
            cursor: Some(other.schedule_id),
            limit: 10,
        }),
        Err(SchedulerError::InvalidCursor(_))
    ));

    let first = store
        .list_schedules(ScheduleQuery {
            execution_scope_id: Some(scope.clone()),
            cursor: None,
            limit: 2,
        })
        .unwrap();
    assert_eq!(
        first
            .items
            .iter()
            .map(|schedule| schedule.stable_schedule_key.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha", "bravo"]
    );
    let second = store
        .list_schedules(ScheduleQuery {
            execution_scope_id: Some(scope.clone()),
            cursor: first.next_cursor,
            limit: 2,
        })
        .unwrap();
    assert_eq!(
        second
            .items
            .iter()
            .map(|schedule| schedule.stable_schedule_key.as_str())
            .collect::<Vec<_>>(),
        vec!["charlie"]
    );
    assert!(second.next_cursor.is_none());

    let schedule = &schedules[0];
    let due_at = observed_at + Duration::from_secs(10);
    let due = store
        .list_due(&scope, due_at, 10)
        .unwrap()
        .into_iter()
        .find(|due| due.schedule.schedule_id == schedule.schedule_id)
        .unwrap();
    let scheduled_trigger_id = factory
        .scheduled_trigger(&scope, &schedule.schedule_id, 1, due.scheduled_for)
        .unwrap();
    store
        .claim_occurrence(ClaimOccurrenceRequest {
            execution_scope_id: scope.clone(),
            schedule_id: schedule.schedule_id.clone(),
            schedule_version: due.schedule.version,
            schedule_revision: 1,
            trigger_id: scheduled_trigger_id.clone(),
            execution_id: None,
            scheduled_for: due.scheduled_for,
            observed_at: due_at,
            owner: "contract".into(),
            lease: Duration::from_secs(30),
        })
        .unwrap();
    let manual_at = observed_at + Duration::from_secs(30);
    let manual_ids = [0, 1].map(|index| {
        let request = manual_request(
            &factory,
            &scope,
            &schedule.schedule_id,
            manual_at,
            None,
            &format!("pagination-{index}"),
        );
        let trigger_id = request.trigger_id.clone();
        store.create_manual_trigger(request).unwrap();
        trigger_id
    });

    let first = store
        .list_triggers(TriggerQuery {
            schedule_id: schedule.schedule_id.clone(),
            kind: None,
            cursor: None,
            limit: 2,
        })
        .unwrap();
    let second = store
        .list_triggers(TriggerQuery {
            schedule_id: schedule.schedule_id.clone(),
            kind: None,
            cursor: first.next_cursor.clone(),
            limit: 2,
        })
        .unwrap();
    assert_eq!(first.items.len(), 2);
    assert_eq!(second.items.len(), 1);
    assert!(second.next_cursor.is_none());
    let mut expected_manual_ids = manual_ids.to_vec();
    expected_manual_ids.sort_by(|left, right| right.as_ref().cmp(left.as_ref()));
    assert_eq!(
        first
            .items
            .iter()
            .map(|trigger| trigger.trigger_id.clone())
            .collect::<Vec<_>>(),
        expected_manual_ids
    );
    assert_eq!(second.items[0].trigger_id, scheduled_trigger_id);

    let manual_page = store
        .list_triggers(TriggerQuery {
            schedule_id: schedule.schedule_id.clone(),
            kind: Some(ScheduleTriggerKind::Manual),
            cursor: None,
            limit: 1,
        })
        .unwrap();
    let last_manual_page = store
        .list_triggers(TriggerQuery {
            schedule_id: schedule.schedule_id.clone(),
            kind: Some(ScheduleTriggerKind::Manual),
            cursor: manual_page.next_cursor,
            limit: 1,
        })
        .unwrap();
    assert_eq!(manual_page.items.len(), 1);
    assert_eq!(last_manual_page.items.len(), 1);
    assert!(last_manual_page.next_cursor.is_none());
    assert!(matches!(
        store.list_triggers(TriggerQuery {
            schedule_id: schedule.schedule_id.clone(),
            kind: Some(ScheduleTriggerKind::Manual),
            cursor: Some(scheduled_trigger_id),
            limit: 10,
        }),
        Err(SchedulerError::InvalidCursor(_))
    ));
}

fn validate_schedule_concurrency(url: &str) {
    let factory = ExecutionIdentityFactory;
    let scope = unique_scope("schedule-concurrency");
    let observed_at = SystemTime::now();
    let schedule = discovered_schedule(&factory, &scope, "race", "every 10s", 10);

    let created = run_race(
        {
            let url = url.to_owned();
            let scope = scope.clone();
            let schedule = schedule.clone();
            move |barrier| {
                let store = PostgresScheduleStore::connect(&url).unwrap();
                barrier.wait();
                store.reconcile(&scope, &[schedule], observed_at)
            }
        },
        {
            let url = url.to_owned();
            let scope = scope.clone();
            let schedule = schedule.clone();
            move |barrier| {
                let store = PostgresScheduleStore::connect(&url).unwrap();
                barrier.wait();
                store.reconcile(&scope, &[schedule], observed_at)
            }
        },
    );
    assert_eq!(
        created
            .into_iter()
            .map(|result| result.unwrap().created)
            .sum::<usize>(),
        1
    );
    let store = PostgresScheduleStore::connect(url).unwrap();
    assert_eq!(
        store.list_revisions(&schedule.schedule_id).unwrap().len(),
        1
    );

    let changed = changed_schedule(schedule.clone(), "every 20s", 20);
    let changed_at = observed_at + Duration::from_secs(1);
    let updated = run_race(
        {
            let url = url.to_owned();
            let scope = scope.clone();
            let changed = changed.clone();
            move |barrier| {
                let store = PostgresScheduleStore::connect(&url).unwrap();
                barrier.wait();
                store.reconcile(&scope, &[changed], changed_at)
            }
        },
        {
            let url = url.to_owned();
            let scope = scope.clone();
            let changed = changed.clone();
            move |barrier| {
                let store = PostgresScheduleStore::connect(&url).unwrap();
                barrier.wait();
                store.reconcile(&scope, &[changed], changed_at)
            }
        },
    );
    assert_eq!(
        updated
            .into_iter()
            .map(|result| result.unwrap().updated)
            .sum::<usize>(),
        1
    );
    assert_eq!(
        store.list_revisions(&schedule.schedule_id).unwrap().len(),
        2
    );

    let actor = ActorRef::new("race-actor").unwrap();
    let mut rediscovered = changed.clone();
    rediscovered.display_name = "Concurrent discovery".into();
    let raced = run_race(
        {
            let url = url.to_owned();
            let schedule_id = schedule.schedule_id.clone();
            let actor = actor.clone();
            move |barrier| {
                let store = PostgresScheduleStore::connect(&url).unwrap();
                barrier.wait();
                store
                    .disable(&schedule_id, &actor, changed_at + Duration::from_secs(1))
                    .map(|_| ())
            }
        },
        {
            let url = url.to_owned();
            let scope = scope.clone();
            move |barrier| {
                let store = PostgresScheduleStore::connect(&url).unwrap();
                barrier.wait();
                store
                    .reconcile(&scope, &[rediscovered], changed_at + Duration::from_secs(1))
                    .map(|_| ())
            }
        },
    );
    for result in raced {
        result.unwrap();
    }
    assert_eq!(
        store
            .get_schedule(&schedule.schedule_id)
            .unwrap()
            .unwrap()
            .enablement,
        ScheduleEnablement::Disabled
    );

    let enabled_at = changed_at + Duration::from_secs(2);
    let enabled = store
        .enable(&schedule.schedule_id, &actor, enabled_at)
        .unwrap();
    let due_at = enabled.next_trigger_at.unwrap();
    let due = store.list_due(&scope, due_at, 10).unwrap().remove(0);
    let trigger_id = factory
        .scheduled_trigger(
            &scope,
            &schedule.schedule_id,
            due.schedule.current_revision,
            due.scheduled_for,
        )
        .unwrap();
    let execution_id = factory
        .scheduled_execution(
            &scope,
            &schedule.schedule_id,
            due.schedule.current_revision,
            due.scheduled_for,
        )
        .unwrap();
    let claim = ClaimOccurrenceRequest {
        execution_scope_id: scope.clone(),
        schedule_id: schedule.schedule_id.clone(),
        schedule_version: due.schedule.version,
        schedule_revision: due.schedule.current_revision,
        trigger_id,
        execution_id: Some(execution_id),
        scheduled_for: due.scheduled_for,
        observed_at: due_at,
        owner: "race-left".into(),
        lease: Duration::from_secs(30),
    };
    let claims = run_race(
        {
            let url = url.to_owned();
            let claim = claim.clone();
            move |barrier| {
                let store = PostgresScheduleStore::connect(&url).unwrap();
                barrier.wait();
                store.claim_occurrence(claim)
            }
        },
        {
            let url = url.to_owned();
            let mut claim = claim;
            claim.owner = "race-right".into();
            move |barrier| {
                let store = PostgresScheduleStore::connect(&url).unwrap();
                barrier.wait();
                store.claim_occurrence(claim)
            }
        },
    );
    let claims = claims.map(Result::unwrap);
    assert_eq!(
        claims
            .iter()
            .filter(|result| matches!(result, ClaimOccurrenceResult::Claimed(_)))
            .count(),
        1
    );
    assert_eq!(
        claims
            .iter()
            .filter(|result| matches!(result, ClaimOccurrenceResult::Busy))
            .count(),
        1
    );
    assert_eq!(
        store
            .list_triggers(TriggerQuery {
                schedule_id: schedule.schedule_id.clone(),
                kind: Some(ScheduleTriggerKind::Scheduled),
                cursor: None,
                limit: 10,
            })
            .unwrap()
            .items
            .len(),
        1
    );

    let matching_left = manual_request(
        &factory,
        &scope,
        &schedule.schedule_id,
        due_at,
        Some("matching-race-key"),
        "same-input",
    );
    let matching_right = ManualTriggerRequest {
        trigger_id: factory.random_trigger(),
        execution_id: factory.random_execution(),
        ..matching_left.clone()
    };
    let matching = run_race(
        {
            let url = url.to_owned();
            move |barrier| {
                let store = PostgresScheduleStore::connect(&url).unwrap();
                barrier.wait();
                store.create_manual_trigger(matching_left)
            }
        },
        {
            let url = url.to_owned();
            move |barrier| {
                let store = PostgresScheduleStore::connect(&url).unwrap();
                barrier.wait();
                store.create_manual_trigger(matching_right)
            }
        },
    )
    .map(Result::unwrap);
    assert_eq!(
        matching
            .iter()
            .filter(|result| matches!(result, ManualTriggerResult::Created(_)))
            .count(),
        1
    );
    assert_eq!(
        matching
            .iter()
            .filter(|result| matches!(result, ManualTriggerResult::Existing(_)))
            .count(),
        1
    );
    assert_eq!(
        store
            .list_triggers(TriggerQuery {
                schedule_id: schedule.schedule_id.clone(),
                kind: Some(ScheduleTriggerKind::Manual),
                cursor: None,
                limit: 10,
            })
            .unwrap()
            .items
            .len(),
        1
    );

    let conflicting_left = manual_request(
        &factory,
        &scope,
        &schedule.schedule_id,
        due_at,
        Some("conflicting-race-key"),
        "left-input",
    );
    let conflicting_right = ManualTriggerRequest {
        trigger_id: factory.random_trigger(),
        execution_id: factory.random_execution(),
        immutable_request_fingerprint: "right-input".into(),
        ..conflicting_left.clone()
    };
    let conflicting_trigger_ids = [
        conflicting_left.trigger_id.clone(),
        conflicting_right.trigger_id.clone(),
    ];
    let conflicting = run_race(
        {
            let url = url.to_owned();
            move |barrier| {
                let store = PostgresScheduleStore::connect(&url).unwrap();
                barrier.wait();
                store.create_manual_trigger(conflicting_left)
            }
        },
        {
            let url = url.to_owned();
            move |barrier| {
                let store = PostgresScheduleStore::connect(&url).unwrap();
                barrier.wait();
                store.create_manual_trigger(conflicting_right)
            }
        },
    );
    assert_eq!(
        conflicting
            .iter()
            .filter(|result| matches!(result, Ok(ManualTriggerResult::Created(_))))
            .count(),
        1
    );
    assert_eq!(
        conflicting
            .iter()
            .filter(|result| matches!(result, Err(SchedulerError::Conflict(_))))
            .count(),
        1
    );
    assert_eq!(
        conflicting_trigger_ids
            .iter()
            .filter(|trigger_id| store.get_trigger(trigger_id).unwrap().is_some())
            .count(),
        1
    );
    assert_eq!(
        store
            .list_triggers(TriggerQuery {
                schedule_id: schedule.schedule_id.clone(),
                kind: Some(ScheduleTriggerKind::Manual),
                cursor: None,
                limit: 10,
            })
            .unwrap()
            .items
            .len(),
        2
    );
}

fn validate_schedule_restart(url: &str) {
    let factory = ExecutionIdentityFactory;
    let scope = unique_scope("schedule-restart");
    let observed_at = SystemTime::now();
    let schedule = discovered_schedule(&factory, &scope, "restart", "every 10s", 10);
    let actor = ActorRef::new("restart-actor").unwrap();
    let (trigger_id, execution_id, schedule_before_restart) = {
        let store = PostgresScheduleStore::connect(url).unwrap();
        store
            .reconcile(&scope, std::slice::from_ref(&schedule), observed_at)
            .unwrap();
        store
            .disable(
                &schedule.schedule_id,
                &actor,
                observed_at + Duration::from_secs(1),
            )
            .unwrap();
        let enabled = store
            .enable(
                &schedule.schedule_id,
                &actor,
                observed_at + Duration::from_secs(2),
            )
            .unwrap();
        let due_at = enabled.next_trigger_at.unwrap();
        let due = store.list_due(&scope, due_at, 10).unwrap().remove(0);
        let trigger_id = factory
            .scheduled_trigger(&scope, &schedule.schedule_id, 1, due.scheduled_for)
            .unwrap();
        let execution_id = factory
            .scheduled_execution(&scope, &schedule.schedule_id, 1, due.scheduled_for)
            .unwrap();
        assert!(matches!(
            store
                .claim_occurrence(ClaimOccurrenceRequest {
                    execution_scope_id: scope.clone(),
                    schedule_id: schedule.schedule_id.clone(),
                    schedule_version: due.schedule.version,
                    schedule_revision: 1,
                    trigger_id: trigger_id.clone(),
                    execution_id: Some(execution_id.clone()),
                    scheduled_for: due.scheduled_for,
                    observed_at: due_at,
                    owner: "restart-first".into(),
                    lease: Duration::from_secs(1),
                })
                .unwrap(),
            ClaimOccurrenceResult::Claimed(_)
        ));
        (trigger_id, execution_id, enabled)
    };

    {
        let store = PostgresScheduleStore::connect(url).unwrap();
        let recovered = store
            .recover_incomplete(
                &scope,
                "restart-second",
                schedule_before_restart.next_trigger_at.unwrap() + Duration::from_secs(2),
                Duration::from_secs(30),
                10,
            )
            .unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].trigger.trigger_id, trigger_id);
        assert_eq!(
            recovered[0].trigger.execution_id,
            Some(execution_id.clone())
        );
        store
            .link_execution(&trigger_id, &execution_id, recovered[0].trigger.version)
            .unwrap();
    }

    let store = PostgresScheduleStore::connect(url).unwrap();
    assert_eq!(
        store
            .list_triggers(TriggerQuery {
                schedule_id: schedule.schedule_id.clone(),
                kind: None,
                cursor: None,
                limit: 10,
            })
            .unwrap()
            .items
            .len(),
        1
    );
    let persisted_trigger = store.get_trigger(&trigger_id).unwrap().unwrap();
    assert_eq!(persisted_trigger.execution_id, Some(execution_id));
    assert_eq!(
        persisted_trigger.status,
        ScheduleTriggerStatus::ExecutionCreated
    );
    let persisted_schedule = store.get_schedule(&schedule.schedule_id).unwrap().unwrap();
    assert_eq!(persisted_schedule.current_revision, 1);
    assert_eq!(persisted_schedule.enablement, ScheduleEnablement::Enabled);
    assert_eq!(persisted_schedule, schedule_before_restart);
    assert_eq!(
        store.list_revisions(&schedule.schedule_id).unwrap().len(),
        1
    );
    let events = store
        .list_operational_events(&schedule.schedule_id, 10)
        .unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].kind, ScheduleOperationalEventKind::Enabled);
    assert_eq!(events[1].kind, ScheduleOperationalEventKind::Disabled);
}

fn validate_schedule_rollback(url: &str) {
    let factory = ExecutionIdentityFactory;
    let scope = unique_scope("schedule-rollback");
    let observed_at = SystemTime::now();
    let schedule = discovered_schedule(&factory, &scope, "rollback", "every 10s", 10);
    let store = PostgresScheduleStore::connect(url).unwrap();
    store
        .reconcile(&scope, std::slice::from_ref(&schedule), observed_at)
        .unwrap();
    let due_at = observed_at + Duration::from_secs(10);
    let due = store.list_due(&scope, due_at, 10).unwrap().remove(0);
    let trigger_id = factory
        .scheduled_trigger(&scope, &schedule.schedule_id, 1, due.scheduled_for)
        .unwrap();
    let execution_id = factory
        .scheduled_execution(&scope, &schedule.schedule_id, 1, due.scheduled_for)
        .unwrap();
    let ClaimOccurrenceResult::Claimed(claimed) = store
        .claim_occurrence(ClaimOccurrenceRequest {
            execution_scope_id: scope,
            schedule_id: schedule.schedule_id.clone(),
            schedule_version: due.schedule.version,
            schedule_revision: 1,
            trigger_id: trigger_id.clone(),
            execution_id: Some(execution_id.clone()),
            scheduled_for: due.scheduled_for,
            observed_at: due_at,
            owner: "rollback".into(),
            lease: Duration::from_secs(30),
        })
        .unwrap()
    else {
        panic!("occurrence should be claimed")
    };
    let schedule_before = store.get_schedule(&schedule.schedule_id).unwrap().unwrap();
    let triggers_before = store
        .list_triggers(TriggerQuery {
            schedule_id: schedule.schedule_id.clone(),
            kind: None,
            cursor: None,
            limit: 10,
        })
        .unwrap();
    let events_before = store
        .list_operational_events(&schedule.schedule_id, 10)
        .unwrap();

    assert!(matches!(
        store.link_execution(&trigger_id, &execution_id, claimed.version + 1),
        Err(SchedulerError::Conflict(_))
    ));

    assert_eq!(
        store.get_schedule(&schedule.schedule_id).unwrap(),
        Some(schedule_before)
    );
    assert_eq!(
        store
            .list_triggers(TriggerQuery {
                schedule_id: schedule.schedule_id.clone(),
                kind: None,
                cursor: None,
                limit: 10,
            })
            .unwrap(),
        triggers_before
    );
    assert_eq!(
        store
            .list_operational_events(&schedule.schedule_id, 10)
            .unwrap(),
        events_before
    );
}

fn run_race<T: Send + 'static>(
    left: impl FnOnce(Arc<Barrier>) -> T + Send + 'static,
    right: impl FnOnce(Arc<Barrier>) -> T + Send + 'static,
) -> [T; 2] {
    let barrier = Arc::new(Barrier::new(3));
    let left_barrier = Arc::clone(&barrier);
    let right_barrier = Arc::clone(&barrier);
    let left = thread::spawn(move || left(left_barrier));
    let right = thread::spawn(move || right(right_barrier));
    barrier.wait();
    [left.join().unwrap(), right.join().unwrap()]
}

fn unique_scope(prefix: &str) -> ExecutionScopeId {
    ExecutionScopeId::new(format!("{prefix}-{}", Uuid::new_v4())).unwrap()
}

fn discovered_schedule(
    factory: &ExecutionIdentityFactory,
    scope: &ExecutionScopeId,
    key: &str,
    expression: &str,
    interval_seconds: u64,
) -> DiscoveredSchedule {
    let action = ActionDefinition {
        runtime: RuntimeKind::Python,
        kind: ActionKind::Schedule(ryvus_protocol::ScheduleAction {
            key: key.into(),
            expression: expression.into(),
        }),
        source: format!("src/{key}.py").into(),
        entrypoint: key.into(),
        name: Some(key.into()),
        policy: ActionExecutionPolicy::default(),
    };
    DiscoveredSchedule {
        schedule_id: factory.schedule_id(scope, key),
        stable_schedule_key: key.into(),
        display_name: key.into(),
        action_id: key.into(),
        action_revision: action_revision(&action).unwrap(),
        action,
        expression: expression.into(),
        interval: Duration::from_secs(interval_seconds),
    }
}

fn changed_schedule(
    mut schedule: DiscoveredSchedule,
    expression: &str,
    interval_seconds: u64,
) -> DiscoveredSchedule {
    schedule.expression = expression.into();
    schedule.interval = Duration::from_secs(interval_seconds);
    let ActionKind::Schedule(action) = &mut schedule.action.kind else {
        panic!("schedule fixture must contain a schedule action")
    };
    action.expression = expression.into();
    schedule.action_revision = action_revision(&schedule.action).unwrap();
    schedule
}

fn manual_request(
    factory: &ExecutionIdentityFactory,
    scope: &ExecutionScopeId,
    schedule_id: &ScheduleId,
    requested_at: SystemTime,
    idempotency_key_hash: Option<&str>,
    fingerprint: &str,
) -> ManualTriggerRequest {
    ManualTriggerRequest {
        execution_scope_id: scope.clone(),
        schedule_id: schedule_id.clone(),
        trigger_id: factory.random_trigger(),
        execution_id: factory.random_execution(),
        actor: ActorRef::new("contract-actor").unwrap(),
        requested_at,
        claim_owner: "contract".into(),
        claim_expires_at: requested_at + Duration::from_secs(30),
        idempotency_key_hash: idempotency_key_hash.map(str::to_owned),
        immutable_request_fingerprint: fingerprint.into(),
    }
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
    assert_eq!(versions, vec![1, 2, 3]);

    drop(client);
    validate_log_removal_migration(url);

    let mut client = Client::connect(url, NoTls).unwrap();
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

fn validate_log_removal_migration(url: &str) {
    let store = PostgresExecutionStateStore::connect(url).unwrap();
    let (mixed_execution, mixed_attempt, mixed_result) = completed_execution(&store);
    let (absent_execution, absent_attempt, absent_result) = completed_execution(&store);
    let (null_execution, null_attempt, null_result) = completed_execution(&store);
    drop(store);

    let log = serde_json::to_value(InvocationEvent::Log(LogEvent {
        execution_id: mixed_attempt.execution_id.clone(),
        attempt_id: mixed_attempt.attempt_id.clone(),
        attempt_number: mixed_attempt.attempt_number,
        timestamp_unix_nanos: None,
        trace_id: None,
        span_id: None,
        level: LogLevel::Info,
        message: "legacy durable log".into(),
        fields: json!({ "legacy": true }),
    }))
    .unwrap();
    let mut mixed_legacy = serde_json::to_value(&mixed_result).unwrap();
    let metric = mixed_legacy["events"][0].clone();
    mixed_legacy["events"] = json!([
        log,
        metric,
        null,
        { "message": "missing type" },
        { "type": null },
        log
    ]);
    let mut mixed_expected = mixed_legacy.clone();
    mixed_expected["events"] = json!([
        metric,
        null,
        { "message": "missing type" },
        { "type": null }
    ]);

    let mut absent_legacy = serde_json::to_value(absent_result).unwrap();
    absent_legacy.as_object_mut().unwrap().remove("events");
    let mut null_legacy = serde_json::to_value(null_result).unwrap();
    null_legacy["events"] = serde_json::Value::Null;

    let fixtures = [
        (&mixed_attempt.attempt_id, &mixed_legacy),
        (&absent_attempt.attempt_id, &absent_legacy),
        (&null_attempt.attempt_id, &null_legacy),
    ];
    let mut client = Client::connect(url, NoTls).unwrap();
    client
        .execute("DELETE FROM ryvus_schema_migrations WHERE version = 3", &[])
        .unwrap();
    for (attempt_id, result) in fixtures {
        client
            .execute(
                "UPDATE ryvus_attempts SET result = $1 WHERE attempt_id = $2",
                &[result, &attempt_id.as_ref()],
            )
            .unwrap();
    }
    let lifecycle_before = migration_lifecycle(
        &mut client,
        [
            &mixed_attempt.attempt_id,
            &absent_attempt.attempt_id,
            &null_attempt.attempt_id,
        ],
    );
    drop(client);

    migrate(url).unwrap();
    let mut client = Client::connect(url, NoTls).unwrap();
    let first_results = migration_results(
        &mut client,
        [
            &mixed_attempt.attempt_id,
            &absent_attempt.attempt_id,
            &null_attempt.attempt_id,
        ],
    );
    assert_eq!(
        first_results,
        vec![mixed_expected, absent_legacy, null_legacy]
    );
    assert_eq!(
        migration_lifecycle(
            &mut client,
            [
                &mixed_attempt.attempt_id,
                &absent_attempt.attempt_id,
                &null_attempt.attempt_id,
            ]
        ),
        lifecycle_before
    );
    drop(client);

    migrate(url).unwrap();
    let mut client = Client::connect(url, NoTls).unwrap();
    assert_eq!(
        migration_results(
            &mut client,
            [
                &mixed_attempt.attempt_id,
                &absent_attempt.attempt_id,
                &null_attempt.attempt_id,
            ]
        ),
        first_results
    );
    for execution_id in [
        mixed_execution.execution_id,
        absent_execution.execution_id,
        null_execution.execution_id,
    ] {
        client
            .execute(
                "DELETE FROM ryvus_executions WHERE execution_id = $1",
                &[&execution_id.as_ref()],
            )
            .unwrap();
    }
}

fn completed_execution(
    store: &dyn ExecutionStateStore,
) -> (ExecutionAggregate, ExecutionAttempt, ExecutionResult) {
    let created = store.create(new_execution()).unwrap();
    let first = attempt(&created.execution_id, 1);
    let running = applied(
        store
            .compare_and_set(
                &created.execution_id,
                created.execution_version,
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
    (finished, first.attempt, result)
}

fn migration_results(client: &mut Client, attempt_ids: [&AttemptId; 3]) -> Vec<serde_json::Value> {
    attempt_ids
        .into_iter()
        .map(|attempt_id| {
            client
                .query_one(
                    "SELECT result FROM ryvus_attempts WHERE attempt_id = $1",
                    &[&attempt_id.as_ref()],
                )
                .unwrap()
                .get(0)
        })
        .collect()
}

fn migration_lifecycle(
    client: &mut Client,
    attempt_ids: [&AttemptId; 3],
) -> Vec<(String, Option<String>, Option<i64>, Option<i64>)> {
    attempt_ids
        .into_iter()
        .map(|attempt_id| {
            let row = client
                .query_one(
                    "SELECT state, outcome, started_at_unix_ns, finished_at_unix_ns \
                     FROM ryvus_attempts WHERE attempt_id = $1",
                    &[&attempt_id.as_ref()],
                )
                .unwrap();
            (row.get(0), row.get(1), row.get(2), row.get(3))
        })
        .collect()
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
    validate_log_rejection_contract(store);
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

fn validate_log_rejection_contract(store: &dyn ExecutionStateStore) {
    let created = store.create(new_execution()).unwrap();
    let first = attempt(&created.execution_id, 1);
    let running = applied(
        store
            .compare_and_set(
                &created.execution_id,
                created.execution_version,
                ExecutionMutation::StartAttempt {
                    attempt: first.clone(),
                },
            )
            .unwrap(),
    );
    let request = InvocationRequest::with_attempt(
        json!({}),
        InvocationContext::default(),
        first.attempt.clone(),
    );
    let result = ExecutionResult {
        invocation_result: InvocationResult::success(&request, json!({})),
        events: vec![InvocationEvent::Log(LogEvent {
            execution_id: first.attempt.execution_id.clone(),
            attempt_id: first.attempt.attempt_id.clone(),
            attempt_number: first.attempt.attempt_number,
            timestamp_unix_nanos: None,
            trace_id: None,
            span_id: None,
            level: LogLevel::Info,
            message: "must not persist".into(),
            fields: json!({}),
        })],
        stdout: String::new(),
        stderr: String::new(),
        duration: Duration::ZERO,
        exit_code: Some(0),
    };
    assert!(matches!(
        store.compare_and_set(
            &created.execution_id,
            running.execution_version,
            ExecutionMutation::FinishAttempt {
                attempt_id: first.attempt.attempt_id.clone(),
                outcome: AttemptOutcome::Succeeded,
                result: Some(result),
                retry: None,
                terminal: Some(TerminalState::new(
                    ExecutionState::Succeeded,
                    Some(first.attempt.attempt_id),
                )),
            },
        ),
        Err(StateStoreError::InvalidMutation(_))
    ));
    assert_eq!(store.load(&created.execution_id).unwrap(), Some(running));
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
        events: vec![InvocationEvent::Metric(MetricEvent {
            execution_id: attempt.execution_id.clone(),
            attempt_id: attempt.attempt_id.clone(),
            attempt_number: attempt.attempt_number,
            name: "jobs".into(),
            value: 1.0,
            unit: "count".into(),
        })],
        stdout: String::new(),
        stderr: String::new(),
        duration: Duration::from_millis(7),
        exit_code: Some(0),
    }
}
