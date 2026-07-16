use std::time::{Duration, UNIX_EPOCH};

use ryvus_execution::{action_revision, ActorRef, ExecutionIdentityFactory, ExecutionScopeId};
use ryvus_protocol::{
    ActionDefinition, ActionExecutionPolicy, ActionKind, RuntimeKind, ScheduleAction,
};
use ryvus_scheduler::{
    ClaimOccurrenceRequest, ClaimOccurrenceResult, DiscoveredSchedule, MemoryScheduleStore,
    ScheduleAvailability, ScheduleEnablement, ScheduleQuery, ScheduleStore, ScheduleTriggerStatus,
    TriggerQuery,
};

#[test]
fn memory_store_preserves_definition_and_operational_history() {
    let store = MemoryScheduleStore::default();
    let factory = ExecutionIdentityFactory;
    let scope = ExecutionScopeId::new("local-project").unwrap();
    let observed_at = UNIX_EPOCH + Duration::from_secs(100);
    let schedule = discovered(&factory, &scope, "every 10s");

    let first = store
        .reconcile(&scope, std::slice::from_ref(&schedule), observed_at)
        .unwrap();
    let repeated = store
        .reconcile(&scope, std::slice::from_ref(&schedule), observed_at)
        .unwrap();
    assert_eq!(first.created, 1);
    assert_eq!(repeated.updated, 0);
    assert_eq!(
        store.list_revisions(&schedule.schedule_id).unwrap().len(),
        1
    );

    let actor = ActorRef::new("local-user").unwrap();
    let disabled = store
        .disable(&schedule.schedule_id, &actor, observed_at)
        .unwrap();
    assert_eq!(disabled.enablement, ScheduleEnablement::Disabled);
    store
        .reconcile(&scope, std::slice::from_ref(&schedule), observed_at)
        .unwrap();
    assert_eq!(
        store
            .get_schedule(&schedule.schedule_id)
            .unwrap()
            .unwrap()
            .enablement,
        ScheduleEnablement::Disabled
    );

    store.reconcile(&scope, &[], observed_at).unwrap();
    assert_eq!(
        store
            .get_schedule(&schedule.schedule_id)
            .unwrap()
            .unwrap()
            .availability,
        ScheduleAvailability::Unavailable
    );
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
}

#[test]
fn memory_store_claims_one_deterministic_occurrence() {
    let store = MemoryScheduleStore::default();
    let factory = ExecutionIdentityFactory;
    let scope = ExecutionScopeId::new("local-project").unwrap();
    let observed_at = UNIX_EPOCH + Duration::from_secs(100);
    let schedule = discovered(&factory, &scope, "every 10s");
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
    let request = ClaimOccurrenceRequest {
        execution_scope_id: scope,
        schedule_id: schedule.schedule_id.clone(),
        schedule_version: due.schedule.version,
        schedule_revision: 1,
        trigger_id: trigger_id.clone(),
        execution_id: Some(execution_id),
        scheduled_for: due.scheduled_for,
        observed_at: due_at,
        owner: "scheduler-1".into(),
        lease: Duration::from_secs(30),
    };

    assert!(matches!(
        store.claim_occurrence(request.clone()).unwrap(),
        ClaimOccurrenceResult::Claimed(_)
    ));
    assert!(matches!(
        store.claim_occurrence(request.clone()).unwrap(),
        ClaimOccurrenceResult::Busy
    ));
    let mut expired_request = request;
    expired_request.observed_at = due_at + Duration::from_secs(31);
    expired_request.owner = "scheduler-2".into();
    let reclaimed = store.claim_occurrence(expired_request).unwrap();
    let ClaimOccurrenceResult::Claimed(reclaimed) = reclaimed else {
        panic!("expired occurrence should be reclaimed");
    };
    assert_eq!(reclaimed.claim_owner.as_deref(), Some("scheduler-2"));
    let triggers = store
        .list_triggers(TriggerQuery {
            schedule_id: schedule.schedule_id,
            kind: None,
            limit: 10,
        })
        .unwrap();
    assert_eq!(triggers.len(), 1);
    assert_eq!(triggers[0].trigger_id, trigger_id);
    assert_eq!(triggers[0].status, ScheduleTriggerStatus::Claimed);
}

fn discovered(
    factory: &ExecutionIdentityFactory,
    scope: &ExecutionScopeId,
    expression: &str,
) -> DiscoveredSchedule {
    let action = ActionDefinition {
        runtime: RuntimeKind::Python,
        kind: ActionKind::Schedule(ScheduleAction {
            key: "inventory-restock".into(),
            expression: expression.into(),
        }),
        source: "src/restock.py".into(),
        entrypoint: "restock".into(),
        name: Some("Restock".into()),
        policy: ActionExecutionPolicy::default(),
    };
    DiscoveredSchedule {
        schedule_id: factory.schedule_id(scope, "inventory-restock"),
        stable_schedule_key: "inventory-restock".into(),
        display_name: "Restock".into(),
        action_id: "restock".into(),
        action_revision: action_revision(&action).unwrap(),
        action,
        expression: expression.into(),
        interval: Duration::from_secs(10),
    }
}
