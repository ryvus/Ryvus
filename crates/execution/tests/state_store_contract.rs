use std::time::{Duration, SystemTime};

use ryvus_execution::{
    execution_creation_fingerprint, AttemptOwnership, AttemptRecord, CreateExecutionResult,
    ExecutionHistoryQuery, ExecutionMutation, ExecutionPolicy, ExecutionResult, ExecutionState,
    ExecutionStateStore, MemoryExecutionStateStore, NewExecution, RetryPolicy, StateStoreError,
    TerminalState, TransitionResult,
};
use ryvus_protocol::{
    ActionDefinition, ActionExecutionPolicy, ActionKind, ApiAction, AttemptId, AttemptOutcome,
    ExecutionAttempt, InvocationContext, InvocationRequest, InvocationResult, RuntimeHostId,
    RuntimeKind, RuntimeSessionId, WorkerId,
};
use serde_json::json;

fn new_execution() -> NewExecution {
    let request = InvocationRequest::new(json!({ "message": "hello" }));
    NewExecution {
        action: ActionDefinition {
            runtime: RuntimeKind::Python,
            kind: ActionKind::Api(ApiAction {
                method: "POST".to_string(),
                path: "/hello".to_string(),
                consumes: vec!["application/json".to_string()],
                produces: vec!["application/json".to_string()],
                request_schema: None,
                response_schema: None,
                query_params: Vec::new(),
                authorizer: None,
            }),
            source: "src/hello.py".into(),
            entrypoint: "hello".to_string(),
            name: Some("hello".to_string()),
            policy: ActionExecutionPolicy::default(),
        },
        action_revision: "test-action-revision".into(),
        execution_scope_id: ryvus_execution::ExecutionScopeId::new("test").unwrap(),
        action_id: "hello".into(),
        trigger: ryvus_execution::ExecutionTrigger::Api,
        creation_fingerprint: "test-fingerprint".into(),
        data_refs: ryvus_execution::ExecutionDataReferences::default(),
        request,
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

#[test]
fn memory_store_idempotently_creates_and_lists_execution_history() {
    let store = MemoryExecutionStateStore::default();
    let mut execution = new_execution();
    execution.created_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1);
    execution.creation_fingerprint = execution_creation_fingerprint(
        &execution.execution_scope_id,
        &execution.action_id,
        &execution.action_revision,
        &execution.trigger,
        &execution.request,
        &execution.policy,
        &execution.data_refs,
    )
    .unwrap();

    assert!(matches!(
        store.create_idempotent(execution.clone()).unwrap(),
        CreateExecutionResult::Created(_)
    ));
    assert!(matches!(
        store.create_idempotent(execution.clone()).unwrap(),
        CreateExecutionResult::Existing(_)
    ));

    let mut second_execution = execution.clone();
    second_execution.request = InvocationRequest::new(json!({ "message": "second" }));
    second_execution.created_at = SystemTime::UNIX_EPOCH + Duration::from_secs(2);
    second_execution.creation_fingerprint = execution_creation_fingerprint(
        &second_execution.execution_scope_id,
        &second_execution.action_id,
        &second_execution.action_revision,
        &second_execution.trigger,
        &second_execution.request,
        &second_execution.policy,
        &second_execution.data_refs,
    )
    .unwrap();
    store.create(second_execution.clone()).unwrap();

    let mut third_execution = execution.clone();
    third_execution.request = InvocationRequest::new(json!({ "message": "third" }));
    third_execution.created_at = SystemTime::UNIX_EPOCH + Duration::from_secs(3);
    third_execution.creation_fingerprint = execution_creation_fingerprint(
        &third_execution.execution_scope_id,
        &third_execution.action_id,
        &third_execution.action_revision,
        &third_execution.trigger,
        &third_execution.request,
        &third_execution.policy,
        &third_execution.data_refs,
    )
    .unwrap();
    store.create(third_execution.clone()).unwrap();

    let first = store
        .list_history(ExecutionHistoryQuery {
            execution_scope_id: execution.execution_scope_id.clone(),
            action_id: Some(execution.action_id.clone()),
            action_revision: Some(execution.action_revision.clone()),
            cursor: None,
            limit: 2,
        })
        .unwrap();
    assert_eq!(first.items.len(), 2);
    assert!(first.next_cursor.is_some());

    let second = store
        .list_history(ExecutionHistoryQuery {
            execution_scope_id: execution.execution_scope_id.clone(),
            action_id: Some(execution.action_id.clone()),
            action_revision: Some(execution.action_revision.clone()),
            cursor: first.next_cursor,
            limit: 2,
        })
        .unwrap();
    assert_eq!(second.items.len(), 1);
    assert!(second.next_cursor.is_none());

    let ids = first
        .items
        .into_iter()
        .chain(second.items)
        .map(|aggregate| aggregate.execution_id)
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        vec![
            third_execution.request.execution_id,
            second_execution.request.execution_id,
            execution.request.execution_id.clone(),
        ]
    );

    execution.action_id = "different-action".into();
    execution.creation_fingerprint = "different-fingerprint".into();
    assert!(matches!(
        store.create_idempotent(execution),
        Err(ryvus_execution::StateStoreError::IdentityConflict { .. })
    ));
}

#[test]
fn memory_history_uses_id_tiebreaker_and_rejects_filtered_cursor() {
    let store = MemoryExecutionStateStore::default();
    let created_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1);
    let mut executions = ["first", "second"].map(|message| {
        let mut execution = new_execution();
        execution.request = InvocationRequest::new(json!({ "message": message }));
        execution.created_at = created_at;
        execution.creation_fingerprint = execution_creation_fingerprint(
            &execution.execution_scope_id,
            &execution.action_id,
            &execution.action_revision,
            &execution.trigger,
            &execution.request,
            &execution.policy,
            &execution.data_refs,
        )
        .unwrap();
        store.create(execution.clone()).unwrap();
        execution
    });
    executions.sort_by(|left, right| {
        right
            .request
            .execution_id
            .as_ref()
            .cmp(left.request.execution_id.as_ref())
    });

    let page = store
        .list_history(ExecutionHistoryQuery {
            execution_scope_id: executions[0].execution_scope_id.clone(),
            action_id: Some(executions[0].action_id.clone()),
            action_revision: Some(executions[0].action_revision.clone()),
            cursor: None,
            limit: 10,
        })
        .unwrap();
    assert_eq!(
        page.items
            .iter()
            .map(|execution| execution.execution_id.clone())
            .collect::<Vec<_>>(),
        executions
            .iter()
            .map(|execution| execution.request.execution_id.clone())
            .collect::<Vec<_>>()
    );

    let cursor = executions[0].request.execution_id.clone();
    assert_eq!(
        store.list_history(ExecutionHistoryQuery {
            execution_scope_id: executions[0].execution_scope_id.clone(),
            action_id: Some("different-action".into()),
            action_revision: None,
            cursor: Some(cursor.clone()),
            limit: 10,
        }),
        Err(StateStoreError::InvalidHistoryCursor { cursor })
    );
}

#[test]
fn memory_store_rejects_non_finite_retry_backoff_without_writing() {
    for backoff in [f64::NAN, f64::INFINITY] {
        let store = MemoryExecutionStateStore::default();
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

fn attempt(execution_id: &ryvus_protocol::ExecutionId, number: u32) -> AttemptRecord {
    AttemptRecord::pending(
        ExecutionAttempt {
            execution_id: execution_id.clone(),
            attempt_id: AttemptId::new(),
            attempt_number: number,
        },
        1_000 + i64::from(number),
    )
}

fn ownership(attempt: &AttemptRecord, session: &str) -> AttemptOwnership {
    AttemptOwnership {
        execution_id: attempt.attempt.execution_id.clone(),
        attempt_id: attempt.attempt.attempt_id.clone(),
        attempt_number: attempt.attempt.attempt_number,
        runtime_host_id: RuntimeHostId::from("host-1"),
        runtime_session_id: RuntimeSessionId::from(session),
        worker_id: WorkerId::from("worker-1"),
    }
}

fn applied(result: TransitionResult) -> ryvus_execution::ExecutionAggregate {
    match result {
        TransitionResult::Applied { aggregate } => aggregate,
        other => panic!("expected applied transition, got {other:?}"),
    }
}

fn malformed_attempts(attempt: &AttemptRecord) -> Vec<AttemptRecord> {
    let mut with_ownership = attempt.clone();
    with_ownership.ownership = Some(ownership(attempt, "session-1"));

    let mut with_outcome = attempt.clone();
    with_outcome.outcome = Some(AttemptOutcome::Failed);

    let request = InvocationRequest::with_attempt(
        json!({}),
        InvocationContext::default(),
        attempt.attempt.clone(),
    );
    let mut with_result = attempt.clone();
    with_result.result = Some(ExecutionResult {
        invocation_result: InvocationResult::success(&request, json!({})),
        events: Vec::new(),
        stdout: String::new(),
        stderr: String::new(),
        duration: Duration::ZERO,
        exit_code: Some(0),
    });

    let mut with_started_at = attempt.clone();
    with_started_at.started_at = Some(SystemTime::now());

    let mut with_finished_at = attempt.clone();
    with_finished_at.finished_at = Some(SystemTime::now());

    let mut running = attempt.clone();
    running.state = ExecutionState::Running;

    vec![
        with_ownership,
        with_outcome,
        with_result,
        with_started_at,
        with_finished_at,
        running,
    ]
}

#[test]
fn memory_store_applies_authoritative_transitions_once() {
    let store = MemoryExecutionStateStore::default();
    let created = store.create(new_execution()).unwrap();
    assert_eq!(created.execution_version, 0);
    assert_eq!(created.state, ExecutionState::Pending);

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
    assert_eq!(running.execution_version, 1);
    assert_eq!(running.attempts[0].deadline_unix_ms, first.deadline_unix_ms);
    assert_eq!(
        running.active_attempt_id,
        Some(first.attempt.attempt_id.clone())
    );

    let assigned = applied(
        store
            .compare_and_set(
                &created.execution_id,
                1,
                ExecutionMutation::AssignOwnership {
                    attempt_id: first.attempt.attempt_id.clone(),
                    ownership: ownership(&first, "session-1"),
                },
            )
            .unwrap(),
    );
    assert_eq!(assigned.execution_version, 2);

    let unchanged = store
        .compare_and_set(
            &created.execution_id,
            2,
            ExecutionMutation::AssignOwnership {
                attempt_id: first.attempt.attempt_id.clone(),
                ownership: ownership(&first, "session-1"),
            },
        )
        .unwrap();
    assert!(matches!(
        unchanged,
        TransitionResult::Unchanged { aggregate } if aggregate.execution_version == 2
    ));

    let replaced = applied(
        store
            .compare_and_set(
                &created.execution_id,
                2,
                ExecutionMutation::AssignOwnership {
                    attempt_id: first.attempt.attempt_id.clone(),
                    ownership: ownership(&first, "session-2"),
                },
            )
            .unwrap(),
    );
    assert_eq!(replaced.execution_version, 3);
}

#[test]
fn cancellation_is_idempotent_and_stale_versions_do_not_write() {
    let store = MemoryExecutionStateStore::default();
    let created = store.create(new_execution()).unwrap();
    let requested_at = SystemTime::now();
    let cancelled = applied(
        store
            .compare_and_set(
                &created.execution_id,
                0,
                ExecutionMutation::RequestCancellation { requested_at },
            )
            .unwrap(),
    );
    assert_eq!(cancelled.execution_version, 1);

    assert!(matches!(
        store
            .compare_and_set(
                &created.execution_id,
                1,
                ExecutionMutation::RequestCancellation {
                    requested_at: SystemTime::now(),
                },
            )
            .unwrap(),
        TransitionResult::Unchanged { aggregate } if aggregate.execution_version == 1
    ));

    assert!(matches!(
        store
            .compare_and_set(
                &created.execution_id,
                0,
                ExecutionMutation::FinishExecution {
                    terminal: TerminalState::new(ExecutionState::Cancelled, None),
                },
            )
            .unwrap(),
        TransitionResult::Conflict { current_version: 1 }
    ));
    assert!(store
        .load(&created.execution_id)
        .unwrap()
        .unwrap()
        .terminal_state
        .is_none());
}

#[test]
fn retry_and_terminal_decisions_are_atomic_and_terminal_is_immutable() {
    let store = MemoryExecutionStateStore::default();
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
    assert_eq!(pending.execution_version, 2);
    assert_eq!(pending.state, ExecutionState::Pending);
    assert_eq!(pending.attempts.len(), 2);
    assert_eq!(pending.attempts[1].attempt, retry.attempt);
    assert_eq!(pending.attempts[1].deadline_unix_ms, retry.deadline_unix_ms);

    let retry_running = applied(
        store
            .compare_and_set(
                &created.execution_id,
                2,
                ExecutionMutation::StartAttempt {
                    attempt: retry.clone(),
                },
            )
            .unwrap(),
    );
    let terminal = TerminalState::new(
        ExecutionState::Succeeded,
        Some(retry.attempt.attempt_id.clone()),
    );
    assert_eq!(
        retry_running.attempts[1].deadline_unix_ms,
        retry.deadline_unix_ms
    );
    let finished = applied(
        store
            .compare_and_set(
                &created.execution_id,
                retry_running.execution_version,
                ExecutionMutation::FinishAttempt {
                    attempt_id: retry.attempt.attempt_id.clone(),
                    outcome: AttemptOutcome::Succeeded,
                    result: None::<ExecutionResult>,
                    retry: None,
                    terminal: Some(terminal.clone()),
                },
            )
            .unwrap(),
    );
    assert_eq!(finished.terminal_state, Some(terminal));
    assert!(store
        .compare_and_set(
            &created.execution_id,
            finished.execution_version,
            ExecutionMutation::FinishExecution {
                terminal: TerminalState::new(ExecutionState::Failed, None),
            },
        )
        .is_err());
}

#[test]
fn terminal_state_must_match_attempt_outcome() {
    let store = MemoryExecutionStateStore::default();
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

    assert!(store
        .compare_and_set(
            &created.execution_id,
            running.execution_version,
            ExecutionMutation::FinishAttempt {
                attempt_id: first.attempt.attempt_id.clone(),
                outcome: AttemptOutcome::Succeeded,
                result: None,
                retry: None,
                terminal: Some(TerminalState::new(
                    ExecutionState::Cancelled,
                    Some(first.attempt.attempt_id.clone()),
                )),
            },
        )
        .is_err());
    let unchanged = store.load(&created.execution_id).unwrap().unwrap();
    assert_eq!(unchanged.execution_version, running.execution_version);
    assert!(unchanged.terminal_state.is_none());
}

#[test]
fn reconciliation_returns_only_non_terminal_cancellation_intents() {
    let store = MemoryExecutionStateStore::default();
    let pending = store.create(new_execution()).unwrap();
    let terminal = store.create(new_execution()).unwrap();
    let ordinary = store.create(new_execution()).unwrap();

    applied(
        store
            .compare_and_set(
                &pending.execution_id,
                0,
                ExecutionMutation::RequestCancellation {
                    requested_at: SystemTime::now(),
                },
            )
            .unwrap(),
    );
    let terminal_with_intent = applied(
        store
            .compare_and_set(
                &terminal.execution_id,
                0,
                ExecutionMutation::RequestCancellation {
                    requested_at: SystemTime::now(),
                },
            )
            .unwrap(),
    );
    applied(
        store
            .compare_and_set(
                &terminal.execution_id,
                terminal_with_intent.execution_version,
                ExecutionMutation::FinishExecution {
                    terminal: TerminalState::new(ExecutionState::Cancelled, None),
                },
            )
            .unwrap(),
    );

    let reconcilable = store.reconcilable_cancellations().unwrap();
    assert_eq!(reconcilable.len(), 1);
    assert_eq!(reconcilable[0].execution_id, pending.execution_id);
    assert!(store.load(&ordinary.execution_id).unwrap().is_some());
}

#[test]
fn invalid_structural_mutations_do_not_change_the_aggregate() {
    let store = MemoryExecutionStateStore::default();
    let created = store.create(new_execution()).unwrap();
    let first = attempt(&created.execution_id, 1);
    applied(
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

    let second = attempt(&created.execution_id, 2);
    assert!(store
        .compare_and_set(
            &created.execution_id,
            1,
            ExecutionMutation::StartAttempt {
                attempt: second.clone(),
            },
        )
        .is_err());
    assert!(store
        .compare_and_set(
            &created.execution_id,
            1,
            ExecutionMutation::AssignOwnership {
                attempt_id: second.attempt.attempt_id.clone(),
                ownership: ownership(&second, "session-1"),
            },
        )
        .is_err());
    assert!(store
        .compare_and_set(
            &created.execution_id,
            1,
            ExecutionMutation::FinishAttempt {
                attempt_id: first.attempt.attempt_id.clone(),
                outcome: AttemptOutcome::Failed,
                result: None,
                retry: None,
                terminal: None,
            },
        )
        .is_err());
    assert!(store
        .compare_and_set(
            &created.execution_id,
            1,
            ExecutionMutation::FinishAttempt {
                attempt_id: first.attempt.attempt_id.clone(),
                outcome: AttemptOutcome::Failed,
                result: None,
                retry: Some(second.clone()),
                terminal: Some(TerminalState::new(
                    ExecutionState::Failed,
                    Some(first.attempt.attempt_id.clone()),
                )),
            },
        )
        .is_err());

    let duplicate_number = AttemptRecord::pending(
        ExecutionAttempt {
            execution_id: created.execution_id.clone(),
            attempt_id: AttemptId::new(),
            attempt_number: 1,
        },
        1_001,
    );
    assert!(store
        .compare_and_set(
            &created.execution_id,
            1,
            ExecutionMutation::FinishAttempt {
                attempt_id: first.attempt.attempt_id.clone(),
                outcome: AttemptOutcome::Failed,
                result: None,
                retry: Some(duplicate_number),
                terminal: None,
            },
        )
        .is_err());

    let cancelled = applied(
        store
            .compare_and_set(
                &created.execution_id,
                1,
                ExecutionMutation::RequestCancellation {
                    requested_at: SystemTime::now(),
                },
            )
            .unwrap(),
    );
    assert!(store
        .compare_and_set(
            &created.execution_id,
            cancelled.execution_version,
            ExecutionMutation::FinishAttempt {
                attempt_id: first.attempt.attempt_id.clone(),
                outcome: AttemptOutcome::Failed,
                result: None,
                retry: Some(second),
                terminal: None,
            },
        )
        .is_err());

    let unchanged = store.load(&created.execution_id).unwrap().unwrap();
    assert_eq!(unchanged.execution_version, cancelled.execution_version);
    assert_eq!(unchanged.active_attempt_id, Some(first.attempt.attempt_id));
    assert_eq!(unchanged.attempts.len(), 1);
}

#[test]
fn malformed_pending_records_are_rejected_for_start_and_retry_without_writes() {
    for malformed in malformed_attempts(&attempt(
        &ryvus_protocol::ExecutionId::from("placeholder"),
        1,
    )) {
        let store = MemoryExecutionStateStore::default();
        let created = store.create(new_execution()).unwrap();
        let mut malformed = malformed;
        malformed.attempt.execution_id = created.execution_id.clone();

        assert!(store
            .compare_and_set(
                &created.execution_id,
                0,
                ExecutionMutation::StartAttempt { attempt: malformed },
            )
            .is_err());
        let unchanged = store.load(&created.execution_id).unwrap().unwrap();
        assert_eq!(unchanged.execution_version, 0);
        assert!(unchanged.attempts.is_empty());
    }

    for malformed in malformed_attempts(&attempt(
        &ryvus_protocol::ExecutionId::from("placeholder"),
        2,
    )) {
        let store = MemoryExecutionStateStore::default();
        let created = store.create(new_execution()).unwrap();
        let first = attempt(&created.execution_id, 1);
        applied(
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
        let mut malformed = malformed;
        malformed.attempt.execution_id = created.execution_id.clone();
        if let Some(ownership) = &mut malformed.ownership {
            ownership.execution_id = created.execution_id.clone();
        }

        assert!(store
            .compare_and_set(
                &created.execution_id,
                1,
                ExecutionMutation::FinishAttempt {
                    attempt_id: first.attempt.attempt_id.clone(),
                    outcome: AttemptOutcome::Failed,
                    result: None,
                    retry: Some(malformed),
                    terminal: None,
                },
            )
            .is_err());
        let unchanged = store.load(&created.execution_id).unwrap().unwrap();
        assert_eq!(unchanged.execution_version, 1);
        assert_eq!(unchanged.attempts.len(), 1);
        assert_eq!(unchanged.active_attempt_id, Some(first.attempt.attempt_id));
    }
}

#[test]
fn attempt_numbers_must_start_at_one_and_increase_consecutively() {
    let store = MemoryExecutionStateStore::default();
    let created = store.create(new_execution()).unwrap();
    assert!(store
        .compare_and_set(
            &created.execution_id,
            0,
            ExecutionMutation::StartAttempt {
                attempt: attempt(&created.execution_id, 2),
            },
        )
        .is_err());
    assert_eq!(
        store
            .load(&created.execution_id)
            .unwrap()
            .unwrap()
            .execution_version,
        0
    );

    let first = attempt(&created.execution_id, 1);
    applied(
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
    assert!(store
        .compare_and_set(
            &created.execution_id,
            1,
            ExecutionMutation::FinishAttempt {
                attempt_id: first.attempt.attempt_id.clone(),
                outcome: AttemptOutcome::Failed,
                result: None,
                retry: Some(attempt(&created.execution_id, 3)),
                terminal: None,
            },
        )
        .is_err());
    let unchanged = store.load(&created.execution_id).unwrap().unwrap();
    assert_eq!(unchanged.execution_version, 1);
    assert_eq!(unchanged.attempts.len(), 1);
    assert_eq!(unchanged.active_attempt_id, Some(first.attempt.attempt_id));
}

#[test]
fn finished_attempts_clear_authoritative_ownership() {
    let store = MemoryExecutionStateStore::default();
    let created = store.create(new_execution()).unwrap();
    assert_eq!(created.action_revision, "test-action-revision");
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
    let retry = attempt(&created.execution_id, 2);
    let pending = applied(
        store
            .compare_and_set(
                &created.execution_id,
                assigned.execution_version,
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
    assert!(pending.attempts[0].ownership.is_none());

    let running = applied(
        store
            .compare_and_set(
                &created.execution_id,
                pending.execution_version,
                ExecutionMutation::StartAttempt {
                    attempt: retry.clone(),
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
                    attempt_id: retry.attempt.attempt_id.clone(),
                    ownership: ownership(&retry, "session-2"),
                },
            )
            .unwrap(),
    );
    let terminal = applied(
        store
            .compare_and_set(
                &created.execution_id,
                assigned.execution_version,
                ExecutionMutation::FinishAttempt {
                    attempt_id: retry.attempt.attempt_id.clone(),
                    outcome: AttemptOutcome::Succeeded,
                    result: None,
                    retry: None,
                    terminal: Some(TerminalState::new(
                        ExecutionState::Succeeded,
                        Some(retry.attempt.attempt_id.clone()),
                    )),
                },
            )
            .unwrap(),
    );
    assert!(terminal.attempts[1].ownership.is_none());
}
