use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Barrier, Mutex,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use ryvus_execution::{
    ActorRef, ExecutionDataReferences, ExecutionIdentityFactory, ExecutionPolicy, ExecutionScopeId,
    ExecutionSubmission, ExecutionTrigger, ScheduleId,
};
use ryvus_protocol::{
    ActionDefinition, ActionExecutionPolicy, ActionKind, ExecutionAttempt, ExecutionId,
    InvocationContext, InvocationRequest, RuntimeKind, ScheduleAction,
};
use ryvus_scheduler::{
    ClaimOccurrenceRequest, ClaimOccurrenceResult, DurableSchedulerService, MemoryScheduleStore,
    ScheduleAvailability, ScheduleEnablement, ScheduleExecution, ScheduleExecutor, ScheduleQuery,
    ScheduleStore, ScheduleTriggerKind, ScheduleTriggerStatus, SchedulerError, SchedulerResult,
    TriggerQuery,
};
use serde_json::json;

const LEASE: Duration = Duration::from_secs(30);

#[test]
fn repeated_tick_creates_one_trigger_and_execution() {
    let (store, executor, service, scope, discovered_at) = setup("scheduler-1", action());
    let due_at = discovered_at + Duration::from_secs(10);

    assert_eq!(service.tick(due_at, 10).unwrap(), 1);
    assert_eq!(service.tick(due_at, 10).unwrap(), 0);

    let schedule = only_schedule(&store, &scope);
    let triggers = triggers(&store, &schedule.schedule_id);
    assert_eq!(triggers.len(), 1);
    assert_eq!(triggers[0].kind, ScheduleTriggerKind::Scheduled);
    assert_eq!(executor.submit_calls.load(Ordering::Relaxed), 1);
    assert_eq!(executor.submissions.lock().unwrap().len(), 1);
}

#[test]
fn run_now_is_idempotent_and_does_not_advance_interval() {
    let (store, executor, service, scope, discovered_at) = setup("scheduler-1", action());
    let schedule = only_schedule(&store, &scope);
    let before = schedule.next_trigger_at;

    let first = service
        .run_now(
            &schedule.schedule_id,
            json!({ "quantity": 1 }),
            Some("request-1"),
            discovered_at,
        )
        .unwrap();
    let repeated = service
        .run_now(
            &schedule.schedule_id,
            json!({ "quantity": 1 }),
            Some("request-1"),
            discovered_at,
        )
        .unwrap();

    assert_eq!(first.execution_id, repeated.execution_id);
    assert_eq!(executor.submit_calls.load(Ordering::Relaxed), 1);
    assert_eq!(executor.submissions.lock().unwrap().len(), 1);
    let triggers = triggers(&store, &schedule.schedule_id);
    assert_eq!(triggers.len(), 1);
    assert_eq!(triggers[0].kind, ScheduleTriggerKind::Manual);
    assert_eq!(
        store
            .get_schedule(&schedule.schedule_id)
            .unwrap()
            .unwrap()
            .next_trigger_at,
        before
    );
}

#[test]
fn disabled_schedule_does_not_tick_but_can_run_now() {
    let (store, executor, service, scope, discovered_at) = setup("scheduler-1", action());
    let schedule = only_schedule(&store, &scope);
    service
        .disable(&schedule.schedule_id, discovered_at)
        .unwrap();

    assert_eq!(
        service
            .tick(discovered_at + Duration::from_secs(10), 10)
            .unwrap(),
        0
    );
    service
        .run_now(&schedule.schedule_id, json!({}), None, discovered_at)
        .unwrap();

    assert_eq!(executor.submit_calls.load(Ordering::Relaxed), 1);
    assert_eq!(executor.submissions.lock().unwrap().len(), 1);
    let triggers = triggers(&store, &schedule.schedule_id);
    assert_eq!(triggers.len(), 1);
    assert_eq!(triggers[0].kind, ScheduleTriggerKind::Manual);
    assert_eq!(
        store
            .get_schedule(&schedule.schedule_id)
            .unwrap()
            .unwrap()
            .enablement,
        ScheduleEnablement::Disabled
    );
}

#[test]
fn unavailable_schedule_neither_ticks_nor_runs_now() {
    let (store, executor, service, scope, discovered_at) = setup("scheduler-1", action());
    let schedule = only_schedule(&store, &scope);
    service.reconcile(&[], discovered_at).unwrap();

    assert_eq!(
        service
            .tick(discovered_at + Duration::from_secs(10), 10)
            .unwrap(),
        0
    );
    assert!(matches!(
        service.run_now(&schedule.schedule_id, json!({}), None, discovered_at),
        Err(SchedulerError::Conflict(message)) if message == "unavailable schedule cannot run now"
    ));
    assert_eq!(executor.submit_calls.load(Ordering::Relaxed), 0);
    assert!(executor.submissions.lock().unwrap().is_empty());
    assert_eq!(
        store
            .get_schedule(&schedule.schedule_id)
            .unwrap()
            .unwrap()
            .availability,
        ScheduleAvailability::Unavailable
    );
}

#[test]
fn claimed_trigger_before_execution_submission_recovers_once() {
    let (store, executor, _, scope, discovered_at) = setup("scheduler-1", action());
    let boundary = claim_boundary(&store, &scope, discovered_at);
    let recovered_at = discovered_at + Duration::from_secs(41);
    let service = service(&store, &executor, scope, "scheduler-2");

    assert_eq!(service.recover(recovered_at, 10).unwrap(), 1);
    assert_eq!(service.recover(recovered_at, 10).unwrap(), 0);
    assert_eq!(executor.submit_calls.load(Ordering::Relaxed), 1);
    assert_eq!(executor.submissions.lock().unwrap().len(), 1);
    assert_eq!(
        store
            .get_trigger(&boundary.trigger_id)
            .unwrap()
            .unwrap()
            .status,
        ScheduleTriggerStatus::ExecutionCreated
    );
}

#[test]
fn submitted_execution_without_trigger_link_recovers_same_execution_id() {
    let (store, executor, _, scope, discovered_at) = setup("scheduler-1", action());
    let boundary = claim_boundary(&store, &scope, discovered_at);
    let execution_id = boundary.execution_id.clone().unwrap();
    let revision = store
        .list_revisions(&boundary.schedule_id)
        .unwrap()
        .remove(0);
    let seeded = scheduled_submission(&scope, &boundary, &revision);
    executor.submit(&revision.action, seeded).unwrap();
    assert_eq!(executor.submit_calls.load(Ordering::Relaxed), 1);
    let service = service(&store, &executor, scope, "scheduler-2");

    assert_eq!(
        service
            .recover(discovered_at + Duration::from_secs(41), 10)
            .unwrap(),
        1
    );

    assert_eq!(executor.submit_calls.load(Ordering::Relaxed), 2);
    assert_eq!(executor.submissions.lock().unwrap().len(), 1);
    assert_eq!(
        store
            .get_trigger(&boundary.trigger_id)
            .unwrap()
            .unwrap()
            .execution_id,
        Some(execution_id)
    );
}

#[test]
fn linked_trigger_without_schedule_advance_recovers_without_submission() {
    let (store, executor, _, scope, discovered_at) = setup("scheduler-1", action());
    let boundary = claim_boundary(&store, &scope, discovered_at);
    let execution_id = boundary.execution_id.clone().unwrap();
    let linked = store
        .link_execution(&boundary.trigger_id, &execution_id, boundary.version)
        .unwrap();
    let service = service(&store, &executor, scope, "scheduler-2");

    assert_eq!(service.recover(discovered_at, 10).unwrap(), 1);

    assert_eq!(executor.submit_calls.load(Ordering::Relaxed), 0);
    assert!(executor.submissions.lock().unwrap().is_empty());
    let schedule = store.get_schedule(&linked.schedule_id).unwrap().unwrap();
    assert_eq!(schedule.last_scheduled_trigger_at, linked.scheduled_for);
}

#[test]
fn advanced_schedule_makes_repeated_recovery_a_no_op() {
    let (store, executor, _, scope, discovered_at) = setup("scheduler-1", action());
    let boundary = claim_boundary(&store, &scope, discovered_at);
    let execution_id = boundary.execution_id.clone().unwrap();
    let linked = store
        .link_execution(&boundary.trigger_id, &execution_id, boundary.version)
        .unwrap();
    let schedule = store.get_schedule(&linked.schedule_id).unwrap().unwrap();
    store
        .advance_schedule(
            &schedule.schedule_id,
            &linked.trigger_id,
            schedule.version,
            discovered_at + Duration::from_secs(20),
        )
        .unwrap();
    let service = service(&store, &executor, scope, "scheduler-2");

    assert_eq!(service.recover(discovered_at, 10).unwrap(), 0);
    assert_eq!(service.recover(discovered_at, 10).unwrap(), 0);
    assert_eq!(executor.submit_calls.load(Ordering::Relaxed), 0);
    assert!(executor.submissions.lock().unwrap().is_empty());
}

#[test]
fn skip_missed_advances_one_old_occurrence_arithmetically() {
    let store = Arc::new(MemoryScheduleStore::default());
    let executor = Arc::new(IdempotentExecutor::default());
    let scope = ExecutionScopeId::new("local-project").unwrap();
    let observed_at = SystemTime::now() - Duration::from_secs(3_601);
    store
        .reconcile(
            &scope,
            &[discovered(
                &scope,
                action_every("every 1s"),
                Duration::from_secs(1),
            )],
            observed_at,
        )
        .unwrap();
    let service = service(&store, &executor, scope.clone(), "scheduler-1");

    assert_eq!(service.tick(SystemTime::now(), 100).unwrap(), 1);

    let schedule = only_schedule(&store, &scope);
    let triggers = triggers(&store, &schedule.schedule_id);
    assert_eq!(triggers.len(), 1);
    assert_eq!(triggers[0].status, ScheduleTriggerStatus::Missed);
    assert!(schedule
        .next_trigger_at
        .is_some_and(|next| next > SystemTime::now()));
    assert_eq!(executor.submit_calls.load(Ordering::Relaxed), 0);
    assert!(executor.submissions.lock().unwrap().is_empty());
}

#[test]
fn concurrent_memory_schedulers_claim_one_occurrence() {
    let store = Arc::new(MemoryScheduleStore::default());
    let executor = Arc::new(IdempotentExecutor::default());
    let scope = ExecutionScopeId::new("local-project").unwrap();
    let discovered_at = SystemTime::now();
    store
        .reconcile(
            &scope,
            &[discovered(&scope, action(), Duration::from_secs(10))],
            discovered_at,
        )
        .unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let due_at = discovered_at + Duration::from_secs(10);
    let handles = ["scheduler-1", "scheduler-2"].map(|owner| {
        let store = Arc::clone(&store);
        let executor = Arc::clone(&executor);
        let scope = scope.clone();
        let barrier = Arc::clone(&barrier);
        thread::spawn(move || {
            let service = service(&store, &executor, scope, owner);
            barrier.wait();
            service.tick(due_at, 10).unwrap()
        })
    });

    let _ = handles.map(|handle| handle.join().unwrap());
    let schedule = only_schedule(&store, &scope);
    let triggers = triggers(&store, &schedule.schedule_id);
    assert_eq!(triggers.len(), 1);
    assert_eq!(executor.submit_calls.load(Ordering::Relaxed), 1);
    assert_eq!(executor.submissions.lock().unwrap().len(), 1);
    assert_eq!(
        triggers[0].execution_id.as_ref(),
        executor.submissions.lock().unwrap().keys().next()
    );
}

#[derive(Default)]
struct IdempotentExecutor {
    submit_calls: AtomicUsize,
    submissions: Mutex<HashMap<ExecutionId, ExecutionSubmission>>,
}

impl ScheduleExecutor for IdempotentExecutor {
    fn submit(
        &self,
        _action: &ActionDefinition,
        submission: ExecutionSubmission,
    ) -> SchedulerResult<ScheduleExecution> {
        self.submit_calls.fetch_add(1, Ordering::Relaxed);
        let execution_id = submission.request.execution_id.clone();
        self.submissions
            .lock()
            .expect("test executor lock should not be poisoned")
            .entry(execution_id.clone())
            .or_insert_with(|| submission.clone());
        Ok(ScheduleExecution {
            execution_id,
            result: None,
        })
    }
}

fn setup(
    owner: &str,
    action: ActionDefinition,
) -> (
    Arc<MemoryScheduleStore>,
    Arc<IdempotentExecutor>,
    DurableSchedulerService<IdempotentExecutor>,
    ExecutionScopeId,
    SystemTime,
) {
    let store = Arc::new(MemoryScheduleStore::default());
    let executor = Arc::new(IdempotentExecutor::default());
    let scope = ExecutionScopeId::new("local-project").unwrap();
    let service = service(&store, &executor, scope.clone(), owner);
    let discovered_at = SystemTime::now();
    service.reconcile(&[action], discovered_at).unwrap();
    (store, executor, service, scope, discovered_at)
}

fn service(
    store: &Arc<MemoryScheduleStore>,
    executor: &Arc<IdempotentExecutor>,
    scope: ExecutionScopeId,
    owner: &str,
) -> DurableSchedulerService<IdempotentExecutor> {
    let service_store: Arc<dyn ScheduleStore> = store.clone();
    DurableSchedulerService::new(
        service_store,
        Arc::clone(executor),
        scope,
        ActorRef::new("local-user").unwrap(),
        owner,
        LEASE,
    )
}

fn only_schedule(
    store: &MemoryScheduleStore,
    scope: &ExecutionScopeId,
) -> ryvus_scheduler::ScheduleRecord {
    store
        .list_schedules(ScheduleQuery {
            execution_scope_id: Some(scope.clone()),
            cursor: None,
            limit: 10,
        })
        .unwrap()
        .items
        .remove(0)
}

fn triggers(
    store: &MemoryScheduleStore,
    schedule_id: &ScheduleId,
) -> Vec<ryvus_scheduler::ScheduleTriggerRecord> {
    store
        .list_triggers(TriggerQuery {
            schedule_id: schedule_id.clone(),
            kind: None,
            cursor: None,
            limit: 100,
        })
        .unwrap()
        .items
}

fn claim_boundary(
    store: &MemoryScheduleStore,
    scope: &ExecutionScopeId,
    discovered_at: SystemTime,
) -> ryvus_scheduler::ScheduleTriggerRecord {
    let due_at = discovered_at + Duration::from_secs(10);
    let due = store.list_due(scope, due_at, 10).unwrap().remove(0);
    let identities = ExecutionIdentityFactory;
    let trigger_id = identities
        .scheduled_trigger(scope, &due.schedule.schedule_id, 1, due.scheduled_for)
        .unwrap();
    let execution_id = identities
        .scheduled_execution(scope, &due.schedule.schedule_id, 1, due.scheduled_for)
        .unwrap();
    let claimed = store
        .claim_occurrence(ClaimOccurrenceRequest {
            execution_scope_id: scope.clone(),
            schedule_id: due.schedule.schedule_id,
            schedule_version: due.schedule.version,
            schedule_revision: 1,
            trigger_id,
            execution_id: Some(execution_id),
            scheduled_for: due.scheduled_for,
            observed_at: due_at,
            owner: "scheduler-1".into(),
            lease: LEASE,
        })
        .unwrap();
    let ClaimOccurrenceResult::Claimed(trigger) = claimed else {
        panic!("boundary occurrence should be claimed")
    };
    trigger
}

fn scheduled_submission(
    scope: &ExecutionScopeId,
    trigger: &ryvus_scheduler::ScheduleTriggerRecord,
    revision: &ryvus_scheduler::ScheduleRevisionRecord,
) -> ExecutionSubmission {
    let execution_id = trigger.execution_id.clone().unwrap();
    let scheduled_for = trigger.scheduled_for.unwrap();
    ExecutionSubmission {
        scope: scope.clone(),
        action_id: revision.action_id.clone(),
        trigger: ExecutionTrigger::Schedule {
            schedule_id: trigger.schedule_id.clone(),
            schedule_revision: trigger.schedule_revision,
            trigger_id: trigger.trigger_id.clone(),
            scheduled_for,
        },
        request: InvocationRequest::with_attempt(
            json!({
                "trigger": "schedule",
                "scheduled_at": scheduled_for
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis(),
                "expression": revision.schedule_expression,
            }),
            InvocationContext::default(),
            ExecutionAttempt {
                execution_id,
                attempt_id: ryvus_protocol::AttemptId::new(),
                attempt_number: 1,
            },
        ),
        policy: ExecutionPolicy::from_action_policy(&revision.action.policy).unwrap(),
        data_refs: ExecutionDataReferences::default(),
    }
}

fn discovered(
    scope: &ExecutionScopeId,
    action: ActionDefinition,
    interval: Duration,
) -> ryvus_scheduler::DiscoveredSchedule {
    let ActionKind::Schedule(schedule) = &action.kind else {
        unreachable!()
    };
    ryvus_scheduler::DiscoveredSchedule {
        schedule_id: ExecutionIdentityFactory.schedule_id(scope, &schedule.key),
        stable_schedule_key: schedule.key.clone(),
        display_name: action.name.clone().unwrap(),
        action_id: action.name.clone().unwrap(),
        action_revision: ryvus_execution::action_revision(&action).unwrap(),
        expression: schedule.expression.clone(),
        action,
        interval,
    }
}

fn action() -> ActionDefinition {
    action_every("every 10s")
}

fn action_every(expression: &str) -> ActionDefinition {
    ActionDefinition {
        runtime: RuntimeKind::Python,
        kind: ActionKind::Schedule(ScheduleAction {
            key: "inventory-restock".into(),
            expression: expression.into(),
        }),
        source: "src/restock.py".into(),
        entrypoint: "restock".into(),
        name: Some("restock".into()),
        policy: ActionExecutionPolicy::default(),
    }
}
