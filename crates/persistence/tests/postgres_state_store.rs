use std::{
    sync::{Arc, Barrier},
    thread,
    time::{Duration, SystemTime},
};

use ryvus_execution::{
    AttemptOwnership, AttemptRecord, ExecutionMutation, ExecutionPolicy, ExecutionResult,
    ExecutionState, ExecutionStateStore, NewExecution, RetryPolicy, StateStoreError, TerminalState,
    TransitionResult,
};
use ryvus_persistence::{migrate, PostgresExecutionStateStore};
use ryvus_protocol::{
    ActionDefinition, ActionExecutionPolicy, ActionKind, ApiAction, AttemptId, AttemptOutcome,
    ExecutionAttempt, InvocationContext, InvocationEvent, InvocationRequest, InvocationResult,
    LogEvent, LogLevel, RuntimeHostId, RuntimeKind, RuntimeSessionId, WorkerId,
};
use serde_json::json;

fn database_url() -> Option<String> {
    match std::env::var("DATABASE_URL") {
        Ok(url) if !url.trim().is_empty() => Some(url),
        _ => {
            eprintln!("skipping PostgreSQL execution-state test: DATABASE_URL is not set");
            None
        }
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

fn attempt(execution_id: &ryvus_protocol::ExecutionId, number: u32) -> AttemptRecord {
    AttemptRecord::pending(
        ExecutionAttempt {
            execution_id: execution_id.clone(),
            attempt_id: AttemptId::new(),
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

fn applied(result: TransitionResult) -> ryvus_execution::ExecutionAggregate {
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

#[test]
fn postgres_matches_authoritative_memory_contract() {
    let Some(url) = database_url() else { return };
    migrate(&url).unwrap();
    let store = PostgresExecutionStateStore::connect(&url).unwrap();
    let new = new_execution();
    let created = store.create(new.clone()).unwrap();
    assert_eq!(
        store.load(&created.execution_id).unwrap(),
        Some(created.clone())
    );
    assert!(matches!(
        store.create(new),
        Err(StateStoreError::AlreadyExists { .. })
    ));

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
    let assigned = applied(
        store
            .compare_and_set(
                &created.execution_id,
                running.execution_version,
                ExecutionMutation::AssignOwnership {
                    attempt_id: first.attempt.attempt_id.clone(),
                    ownership: ownership(&first, "session-1"),
                },
            )
            .unwrap(),
    );
    assert!(matches!(
        store.compare_and_set(
            &created.execution_id,
            assigned.execution_version,
            ExecutionMutation::AssignOwnership {
                attempt_id: first.attempt.attempt_id.clone(),
                ownership: ownership(&first, "session-1"),
            },
        ).unwrap(),
        TransitionResult::Unchanged { aggregate }
            if aggregate.execution_version == assigned.execution_version
    ));
    let replaced = applied(
        store
            .compare_and_set(
                &created.execution_id,
                assigned.execution_version,
                ExecutionMutation::AssignOwnership {
                    attempt_id: first.attempt.attempt_id.clone(),
                    ownership: ownership(&first, "session-2"),
                },
            )
            .unwrap(),
    );
    let reloaded = PostgresExecutionStateStore::connect(&url)
        .unwrap()
        .load(&created.execution_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        reloaded.attempts[0].ownership,
        Some(ownership(&first, "session-2"))
    );
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
                    requested_at: SystemTime::now()
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
    assert!(matches!(
        store.compare_and_set(
            &created.execution_id,
            replaced.execution_version,
            ExecutionMutation::FinishExecution {
                terminal: TerminalState::new(ExecutionState::Cancelled, None),
            },
        ).unwrap(),
        TransitionResult::Conflict { current_version }
            if current_version == cancelled.execution_version
    ));
    let terminal = applied(
        store
            .compare_and_set(
                &created.execution_id,
                cancelled.execution_version,
                ExecutionMutation::FinishAttempt {
                    attempt_id: first.attempt.attempt_id.clone(),
                    outcome: AttemptOutcome::Cancelled,
                    result: None,
                    retry: None,
                    terminal: Some(TerminalState::new(
                        ExecutionState::Cancelled,
                        Some(first.attempt.attempt_id.clone()),
                    )),
                },
            )
            .unwrap(),
    );
    assert_eq!(terminal.state, ExecutionState::Cancelled);
    assert!(!store
        .reconcilable_cancellations()
        .unwrap()
        .iter()
        .any(|aggregate| aggregate.execution_id == created.execution_id));
    assert!(!store
        .active_executions()
        .unwrap()
        .iter()
        .any(|aggregate| aggregate.execution_id == created.execution_id));
    assert!(store
        .compare_and_set(
            &created.execution_id,
            terminal.execution_version,
            ExecutionMutation::RequestCancellation {
                requested_at: SystemTime::now()
            },
        )
        .is_err());

    let inconsistent = store.create(new_execution()).unwrap();
    let inconsistent_attempt = attempt(&inconsistent.execution_id, 1);
    let inconsistent_running = applied(
        store
            .compare_and_set(
                &inconsistent.execution_id,
                0,
                ExecutionMutation::StartAttempt {
                    attempt: inconsistent_attempt.clone(),
                },
            )
            .unwrap(),
    );
    assert!(store
        .compare_and_set(
            &inconsistent.execution_id,
            inconsistent_running.execution_version,
            ExecutionMutation::FinishAttempt {
                attempt_id: inconsistent_attempt.attempt.attempt_id.clone(),
                outcome: AttemptOutcome::Succeeded,
                result: None,
                retry: None,
                terminal: Some(TerminalState::new(
                    ExecutionState::Cancelled,
                    Some(inconsistent_attempt.attempt.attempt_id.clone()),
                )),
            },
        )
        .is_err());
    assert_eq!(
        store
            .load(&inconsistent.execution_id)
            .unwrap()
            .unwrap()
            .execution_version,
        inconsistent_running.execution_version
    );
    applied(
        store
            .compare_and_set(
                &inconsistent.execution_id,
                inconsistent_running.execution_version,
                ExecutionMutation::FinishAttempt {
                    attempt_id: inconsistent_attempt.attempt.attempt_id.clone(),
                    outcome: AttemptOutcome::Succeeded,
                    result: None,
                    retry: None,
                    terminal: Some(TerminalState::new(
                        ExecutionState::Succeeded,
                        Some(inconsistent_attempt.attempt.attempt_id),
                    )),
                },
            )
            .unwrap(),
    );

    let result_execution = store.create(new_execution()).unwrap();
    let result_attempt = attempt(&result_execution.execution_id, 1);
    let running = applied(
        store
            .compare_and_set(
                &result_execution.execution_id,
                0,
                ExecutionMutation::StartAttempt {
                    attempt: result_attempt.clone(),
                },
            )
            .unwrap(),
    );
    let result = structured_result(&result_attempt.attempt);
    let finished = applied(
        store
            .compare_and_set(
                &result_execution.execution_id,
                running.execution_version,
                ExecutionMutation::FinishAttempt {
                    attempt_id: result_attempt.attempt.attempt_id.clone(),
                    outcome: AttemptOutcome::Succeeded,
                    result: Some(result.clone()),
                    retry: None,
                    terminal: Some(TerminalState::new(
                        ExecutionState::Succeeded,
                        Some(result_attempt.attempt.attempt_id.clone()),
                    )),
                },
            )
            .unwrap(),
    );
    let reloaded = PostgresExecutionStateStore::connect(&url)
        .unwrap()
        .load(&result_execution.execution_id)
        .unwrap()
        .unwrap();
    assert_eq!(reloaded, finished);
    assert_eq!(reloaded.attempts[0].result, Some(result));
}

#[test]
fn postgres_rejects_non_finite_policy_without_creating_a_row() {
    let Some(url) = database_url() else { return };
    migrate(&url).unwrap();
    let store = PostgresExecutionStateStore::connect(&url).unwrap();
    for backoff in [f64::NAN, f64::INFINITY] {
        let mut execution = new_execution();
        execution.policy.retry.backoff = backoff;
        let execution_id = execution.request.execution_id.clone();
        assert!(store.create(execution).is_err());
        assert!(store.load(&execution_id).unwrap().is_none());

        let mut execution = new_execution();
        execution.action.policy.retry.backoff = backoff;
        let execution_id = execution.request.execution_id.clone();
        assert!(store.create(execution).is_err());
        assert!(store.load(&execution_id).unwrap().is_none());
    }
}

#[test]
fn postgres_persists_atomic_retry_across_connections() {
    let Some(url) = database_url() else { return };
    migrate(&url).unwrap();
    let first_store = PostgresExecutionStateStore::connect(&url).unwrap();
    let created = first_store.create(new_execution()).unwrap();
    let first = attempt(&created.execution_id, 1);
    let running = applied(
        first_store
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
        first_store
            .compare_and_set(
                &created.execution_id,
                running.execution_version,
                ExecutionMutation::FinishAttempt {
                    attempt_id: first.attempt.attempt_id.clone(),
                    outcome: AttemptOutcome::Failed,
                    result: None,
                    retry: Some(retry),
                    terminal: None,
                },
            )
            .unwrap(),
    );

    let second_store = PostgresExecutionStateStore::connect(&url).unwrap();
    assert_eq!(
        second_store.load(&created.execution_id).unwrap(),
        Some(pending)
    );
}

#[test]
fn postgres_terminal_cas_has_exactly_one_winner_across_connections() {
    let Some(url) = database_url() else { return };
    migrate(&url).unwrap();
    let seed = PostgresExecutionStateStore::connect(&url).unwrap();
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
    let barrier = Arc::new(Barrier::new(3));

    let spawn = |outcome: AttemptOutcome, state: ExecutionState| {
        let url = url.clone();
        let barrier = barrier.clone();
        let execution_id = created.execution_id.clone();
        let attempt_id = first.attempt.attempt_id.clone();
        thread::spawn(move || {
            let store = PostgresExecutionStateStore::connect(&url).unwrap();
            barrier.wait();
            store
                .compare_and_set(
                    &execution_id,
                    running.execution_version,
                    ExecutionMutation::FinishAttempt {
                        attempt_id: attempt_id.clone(),
                        outcome,
                        result: None,
                        retry: None,
                        terminal: Some(TerminalState::new(state, Some(attempt_id))),
                    },
                )
                .unwrap()
        })
    };
    let success = spawn(AttemptOutcome::Succeeded, ExecutionState::Succeeded);
    let timeout = spawn(AttemptOutcome::TimedOut, ExecutionState::TimedOut);
    barrier.wait();
    let results = [success.join().unwrap(), timeout.join().unwrap()];
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

#[test]
fn postgres_rejects_corrupt_cross_row_authoritative_facts() {
    let Some(url) = database_url() else { return };
    migrate(&url).unwrap();
    let store = PostgresExecutionStateStore::connect(&url).unwrap();
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
    let assigned = applied(
        store
            .compare_and_set(
                &created.execution_id,
                running.execution_version,
                ExecutionMutation::AssignOwnership {
                    attempt_id: first.attempt.attempt_id.clone(),
                    ownership: ownership(&first, "corruption-session"),
                },
            )
            .unwrap(),
    );

    let mut client = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    let invalid_ownership = json!({
        "execution_id": created.execution_id,
        "attempt_id": AttemptId::new(),
        "attempt_number": 99,
        "runtime_host_id": "wrong-host",
        "runtime_session_id": "wrong-session",
        "worker_id": "wrong-worker"
    });
    client
        .execute(
            "UPDATE ryvus_attempts SET ownership = $2 WHERE attempt_id = $1",
            &[&first.attempt.attempt_id.as_ref(), &invalid_ownership],
        )
        .unwrap();
    assert!(matches!(
        store.load(&created.execution_id),
        Err(StateStoreError::Backend(message)) if message.contains("aggregate invariants failed")
    ));
    let valid_ownership = serde_json::to_value(ownership(&first, "corruption-session")).unwrap();
    client
        .execute(
            "UPDATE ryvus_attempts SET ownership = $2 WHERE attempt_id = $1",
            &[&first.attempt.attempt_id.as_ref(), &valid_ownership],
        )
        .unwrap();

    let terminal = applied(
        store
            .compare_and_set(
                &created.execution_id,
                assigned.execution_version,
                ExecutionMutation::FinishAttempt {
                    attempt_id: first.attempt.attempt_id.clone(),
                    outcome: AttemptOutcome::Succeeded,
                    result: Some(structured_result(&first.attempt)),
                    retry: None,
                    terminal: Some(TerminalState::new(
                        ExecutionState::Succeeded,
                        Some(first.attempt.attempt_id.clone()),
                    )),
                },
            )
            .unwrap(),
    );
    client
        .execute(
            "UPDATE ryvus_terminal_states SET state = 'failed' WHERE execution_id = $1",
            &[&created.execution_id.as_ref()],
        )
        .unwrap();
    assert!(matches!(
        store.load(&created.execution_id),
        Err(StateStoreError::Backend(message)) if message.contains("aggregate invariants failed")
    ));
    client
        .execute(
            "UPDATE ryvus_terminal_states SET state = 'succeeded' WHERE execution_id = $1",
            &[&created.execution_id.as_ref()],
        )
        .unwrap();
    assert_eq!(store.load(&created.execution_id).unwrap(), Some(terminal));

    let missing_active = store.create(new_execution()).unwrap();
    client
        .execute(
            "UPDATE ryvus_executions SET state = 'running' WHERE execution_id = $1",
            &[&missing_active.execution_id.as_ref()],
        )
        .unwrap();
    assert!(matches!(
        store.load(&missing_active.execution_id),
        Err(StateStoreError::Backend(message)) if message.contains("aggregate invariants failed")
    ));
    client
        .execute(
            "UPDATE ryvus_executions SET state = 'pending' WHERE execution_id = $1",
            &[&missing_active.execution_id.as_ref()],
        )
        .unwrap();
    assert_eq!(
        store.load(&missing_active.execution_id).unwrap(),
        Some(missing_active)
    );
}
