use std::time::{Duration, UNIX_EPOCH};

use ryvus_execution::{action_revision, ActorRef, ExecutionIdentityFactory, ExecutionScopeId};
use ryvus_protocol::{
    ActionDefinition, ActionExecutionPolicy, ActionKind, RuntimeKind, ScheduleAction,
};
use ryvus_scheduler::{
    ClaimOccurrenceRequest, ClaimOccurrenceResult, DiscoveredSchedule, ManualTriggerRequest,
    MemoryScheduleStore, ScheduleAvailability, ScheduleEnablement, ScheduleQuery, ScheduleStore,
    ScheduleTriggerKind, ScheduleTriggerStatus, SchedulerError, TriggerFailure, TriggerQuery,
};

#[test]
fn display_only_change_keeps_revision() {
    let store = MemoryScheduleStore::default();
    let factory = ExecutionIdentityFactory;
    let scope = ExecutionScopeId::new("local-project").unwrap();
    let observed_at = UNIX_EPOCH + Duration::from_secs(100);
    let schedule = discovered(&factory, &scope, "every 10s");
    store
        .reconcile(&scope, std::slice::from_ref(&schedule), observed_at)
        .unwrap();
    let mut renamed = schedule.clone();
    renamed.display_name = "Renamed restock".into();

    store
        .reconcile(&scope, &[renamed], observed_at + Duration::from_secs(1))
        .unwrap();

    let current = store.get_schedule(&schedule.schedule_id).unwrap().unwrap();
    assert_eq!(current.current_revision, 1);
    assert_eq!(current.display_name, "Renamed restock");
    assert_eq!(
        store.list_revisions(&schedule.schedule_id).unwrap().len(),
        1
    );
}

#[test]
fn expression_and_action_revision_change_increments_once() {
    let store = MemoryScheduleStore::default();
    let factory = ExecutionIdentityFactory;
    let scope = ExecutionScopeId::new("local-project").unwrap();
    let observed_at = UNIX_EPOCH + Duration::from_secs(100);
    let schedule = discovered(&factory, &scope, "every 10s");
    store
        .reconcile(&scope, std::slice::from_ref(&schedule), observed_at)
        .unwrap();
    let mut changed = schedule.clone();
    changed.expression = "every 20s".into();
    changed.interval = Duration::from_secs(20);
    let ActionKind::Schedule(action_schedule) = &mut changed.action.kind else {
        unreachable!()
    };
    action_schedule.expression = changed.expression.clone();
    changed.action_revision = action_revision(&changed.action).unwrap();

    store
        .reconcile(
            &scope,
            std::slice::from_ref(&changed),
            observed_at + Duration::from_secs(1),
        )
        .unwrap();
    store
        .reconcile(
            &scope,
            std::slice::from_ref(&changed),
            observed_at + Duration::from_secs(2),
        )
        .unwrap();

    let current = store.get_schedule(&schedule.schedule_id).unwrap().unwrap();
    assert_eq!(current.current_revision, 2);
    assert_eq!(
        store.list_revisions(&schedule.schedule_id).unwrap().len(),
        2
    );
}

#[test]
fn missing_discovery_marks_unavailable_without_changing_enablement() {
    let store = MemoryScheduleStore::default();
    let factory = ExecutionIdentityFactory;
    let scope = ExecutionScopeId::new("local-project").unwrap();
    let observed_at = UNIX_EPOCH + Duration::from_secs(100);
    let schedule = discovered(&factory, &scope, "every 10s");
    store
        .reconcile(&scope, std::slice::from_ref(&schedule), observed_at)
        .unwrap();
    let actor = ActorRef::new("local-user").unwrap();
    store
        .disable(&schedule.schedule_id, &actor, observed_at)
        .unwrap();
    store.reconcile(&scope, &[], observed_at).unwrap();

    let unavailable = store.get_schedule(&schedule.schedule_id).unwrap().unwrap();
    assert_eq!(unavailable.availability, ScheduleAvailability::Unavailable);
    assert_eq!(unavailable.enablement, ScheduleEnablement::Disabled);
}

#[test]
fn rediscovery_preserves_enablement_and_creates_no_downtime_triggers() {
    let store = MemoryScheduleStore::default();
    let factory = ExecutionIdentityFactory;
    let scope = ExecutionScopeId::new("local-project").unwrap();
    let observed_at = UNIX_EPOCH + Duration::from_secs(100);
    let schedule = discovered(&factory, &scope, "every 10s");
    store
        .reconcile(&scope, std::slice::from_ref(&schedule), observed_at)
        .unwrap();
    let actor = ActorRef::new("local-user").unwrap();
    store
        .disable(&schedule.schedule_id, &actor, observed_at)
        .unwrap();
    store.reconcile(&scope, &[], observed_at).unwrap();
    store
        .reconcile(
            &scope,
            std::slice::from_ref(&schedule),
            observed_at + Duration::from_secs(60),
        )
        .unwrap();

    let rediscovered = store.get_schedule(&schedule.schedule_id).unwrap().unwrap();
    assert_eq!(rediscovered.availability, ScheduleAvailability::Available);
    assert_eq!(rediscovered.enablement, ScheduleEnablement::Disabled);
    assert_eq!(
        store
            .list_triggers(TriggerQuery {
                schedule_id: schedule.schedule_id,
                kind: None,
                cursor: None,
                limit: 10,
            })
            .unwrap()
            .items
            .len(),
        0
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
            cursor: None,
            limit: 10,
        })
        .unwrap();
    assert_eq!(triggers.items.len(), 1);
    assert_eq!(triggers.items[0].trigger_id, trigger_id);
    assert_eq!(triggers.items[0].status, ScheduleTriggerStatus::Claimed);
}

#[test]
fn memory_store_pages_schedules_and_filtered_triggers_without_duplicates() {
    let store = MemoryScheduleStore::default();
    let factory = ExecutionIdentityFactory;
    let scope = ExecutionScopeId::new("local-project").unwrap();
    let observed_at = UNIX_EPOCH + Duration::from_secs(100);
    let schedules =
        ["charlie", "alpha", "bravo"].map(|key| discovered_with_key(&factory, &scope, key));
    store.reconcile(&scope, &schedules, observed_at).unwrap();

    let other_scope = ExecutionScopeId::new("other-project").unwrap();
    let same_key = discovered_with_key(&factory, &other_scope, "alpha");
    store
        .reconcile(&other_scope, std::slice::from_ref(&same_key), observed_at)
        .unwrap();
    let same_key_ids = store
        .list_schedules(ScheduleQuery {
            execution_scope_id: None,
            cursor: None,
            limit: 10,
        })
        .unwrap()
        .items
        .into_iter()
        .filter(|schedule| schedule.stable_schedule_key == "alpha")
        .map(|schedule| schedule.schedule_id)
        .collect::<Vec<_>>();
    let mut expected_same_key_ids = vec![schedules[1].schedule_id.clone(), same_key.schedule_id];
    expected_same_key_ids.sort_by(|left, right| left.as_ref().cmp(right.as_ref()));
    assert_eq!(same_key_ids, expected_same_key_ids);
    let invalid_schedule_cursor = expected_same_key_ids
        .into_iter()
        .find(|id| id != &schedules[1].schedule_id)
        .unwrap();
    assert!(matches!(
        store.list_schedules(ScheduleQuery {
            execution_scope_id: Some(scope.clone()),
            cursor: Some(invalid_schedule_cursor),
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

    let schedule = store
        .get_schedule(&schedules[0].schedule_id)
        .unwrap()
        .unwrap();
    let due = store
        .list_due(&scope, observed_at + Duration::from_secs(10), 10)
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
            observed_at: observed_at + Duration::from_secs(10),
            owner: "scheduler-1".into(),
            lease: Duration::from_secs(30),
        })
        .unwrap();
    let actor = ActorRef::new("local-user").unwrap();
    let manual_trigger_ids =
        ["manual-0", "manual-1"].map(|id| ryvus_execution::ScheduleTriggerId::new(id).unwrap());
    for (index, requested_at) in [130, 130].into_iter().enumerate() {
        store
            .create_manual_trigger(ManualTriggerRequest {
                execution_scope_id: scope.clone(),
                schedule_id: schedule.schedule_id.clone(),
                trigger_id: manual_trigger_ids[index].clone(),
                execution_id: ryvus_protocol::ExecutionId::from(format!("execution-{index}")),
                actor: actor.clone(),
                requested_at: UNIX_EPOCH + Duration::from_secs(requested_at),
                claim_owner: "scheduler-1".into(),
                claim_expires_at: UNIX_EPOCH + Duration::from_secs(requested_at + 30),
                idempotency_key_hash: None,
                immutable_request_fingerprint: format!("fingerprint-{index}"),
            })
            .unwrap();
    }

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
            cursor: first.next_cursor,
            limit: 2,
        })
        .unwrap();
    assert_eq!(first.items.len(), 2);
    assert_eq!(second.items.len(), 1);
    assert!(second.next_cursor.is_none());
    assert_eq!(
        first
            .items
            .iter()
            .chain(&second.items)
            .map(|trigger| trigger.trigger_id.clone())
            .collect::<Vec<_>>(),
        vec![
            manual_trigger_ids[1].clone(),
            manual_trigger_ids[0].clone(),
            scheduled_trigger_id.clone(),
        ]
    );

    let first_manual = store
        .list_triggers(TriggerQuery {
            schedule_id: schedule.schedule_id.clone(),
            kind: Some(ScheduleTriggerKind::Manual),
            cursor: None,
            limit: 1,
        })
        .unwrap();
    let second_manual = store
        .list_triggers(TriggerQuery {
            schedule_id: schedule.schedule_id.clone(),
            kind: Some(ScheduleTriggerKind::Manual),
            cursor: first_manual.next_cursor,
            limit: 1,
        })
        .unwrap();
    assert_eq!(first_manual.items[0].trigger_id.as_ref(), "manual-1");
    assert_eq!(second_manual.items[0].trigger_id.as_ref(), "manual-0");
    assert!(second_manual.next_cursor.is_none());
    assert_eq!(
        first_manual.items[0].created_at,
        second_manual.items[0].created_at
    );
    assert!(first_manual.items[0].trigger_id.as_ref() > second_manual.items[0].trigger_id.as_ref());
    assert!(matches!(
        store.list_triggers(TriggerQuery {
            schedule_id: schedule.schedule_id,
            kind: Some(ScheduleTriggerKind::Manual),
            cursor: Some(scheduled_trigger_id),
            limit: 10,
        }),
        Err(SchedulerError::InvalidCursor(_))
    ));
}

#[test]
fn re_enable_chooses_a_future_occurrence() {
    let store = MemoryScheduleStore::default();
    let factory = ExecutionIdentityFactory;
    let scope = ExecutionScopeId::new("local-project").unwrap();
    let observed_at = UNIX_EPOCH + Duration::from_secs(100);
    let schedule = discovered(&factory, &scope, "every 10s");
    store
        .reconcile(&scope, std::slice::from_ref(&schedule), observed_at)
        .unwrap();
    let actor = ActorRef::new("local-user").unwrap();
    store
        .disable(&schedule.schedule_id, &actor, observed_at)
        .unwrap();
    let enabled_at = observed_at + Duration::from_secs(60);

    let enabled = store
        .enable(&schedule.schedule_id, &actor, enabled_at)
        .unwrap();

    assert_eq!(enabled.enablement, ScheduleEnablement::Enabled);
    assert_eq!(
        enabled.next_trigger_at,
        Some(enabled_at + Duration::from_secs(10))
    );
}

#[test]
fn manual_idempotency_conflict_rejects_different_input() {
    let store = MemoryScheduleStore::default();
    let factory = ExecutionIdentityFactory;
    let scope = ExecutionScopeId::new("local-project").unwrap();
    let observed_at = UNIX_EPOCH + Duration::from_secs(100);
    let schedule = discovered(&factory, &scope, "every 10s");
    store
        .reconcile(&scope, std::slice::from_ref(&schedule), observed_at)
        .unwrap();
    let request = ManualTriggerRequest {
        execution_scope_id: scope,
        schedule_id: schedule.schedule_id,
        trigger_id: ryvus_execution::ScheduleTriggerId::new("manual-1").unwrap(),
        execution_id: ryvus_protocol::ExecutionId::from("execution-1"),
        actor: ActorRef::new("local-user").unwrap(),
        requested_at: observed_at,
        claim_owner: "scheduler-1".into(),
        claim_expires_at: observed_at + Duration::from_secs(30),
        idempotency_key_hash: Some("same-key".into()),
        immutable_request_fingerprint: "first-input".into(),
    };
    store.create_manual_trigger(request.clone()).unwrap();
    let conflicting = ManualTriggerRequest {
        trigger_id: ryvus_execution::ScheduleTriggerId::new("manual-2").unwrap(),
        execution_id: ryvus_protocol::ExecutionId::from("execution-2"),
        immutable_request_fingerprint: "different-input".into(),
        ..request
    };

    assert!(matches!(
        store.create_manual_trigger(conflicting),
        Err(SchedulerError::Conflict(message))
            if message == "manual trigger idempotency key was reused with different input"
    ));
}

#[test]
fn terminal_trigger_transitions_are_immutable() {
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
    let ClaimOccurrenceResult::Claimed(claimed) = store
        .claim_occurrence(ClaimOccurrenceRequest {
            execution_scope_id: scope,
            schedule_id: schedule.schedule_id,
            schedule_version: due.schedule.version,
            schedule_revision: 1,
            trigger_id: trigger_id.clone(),
            execution_id: Some(execution_id.clone()),
            scheduled_for: due.scheduled_for,
            observed_at: due_at,
            owner: "scheduler-1".into(),
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
        Err(SchedulerError::Conflict(message)) if message == "terminal trigger state is immutable"
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

fn discovered_with_key(
    factory: &ExecutionIdentityFactory,
    scope: &ExecutionScopeId,
    key: &str,
) -> DiscoveredSchedule {
    let mut schedule = discovered(factory, scope, "every 10s");
    schedule.schedule_id = factory.schedule_id(scope, key);
    schedule.stable_schedule_key = key.into();
    schedule.display_name = key.into();
    schedule.action_id = key.into();
    schedule
}
