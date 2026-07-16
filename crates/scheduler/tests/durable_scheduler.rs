use std::{
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use ryvus_execution::{ActorRef, ExecutionScopeId, ExecutionSubmission};
use ryvus_protocol::{
    ActionDefinition, ActionExecutionPolicy, ActionKind, InvocationResult, RuntimeKind,
    ScheduleAction,
};
use ryvus_scheduler::{
    DurableSchedulerService, MemoryScheduleStore, ScheduleEnablement, ScheduleExecution,
    ScheduleExecutor, ScheduleQuery, ScheduleStore, SchedulerResult, TriggerQuery,
};
use serde_json::json;

#[test]
fn scheduled_occurrence_and_run_now_are_idempotent() {
    let store = Arc::new(MemoryScheduleStore::default());
    let executor = Arc::new(RecordingExecutor::default());
    let scope = ExecutionScopeId::new("local-project").unwrap();
    let service_store: Arc<dyn ScheduleStore> = store.clone();
    let service = DurableSchedulerService::new(
        service_store,
        Arc::clone(&executor),
        scope.clone(),
        ActorRef::new("local-user").unwrap(),
        "scheduler-1",
        Duration::from_secs(30),
    );
    let discovered_at = SystemTime::now();
    service.reconcile(&[action()], discovered_at).unwrap();
    let schedule = store
        .list_schedules(ScheduleQuery {
            execution_scope_id: Some(scope),
            limit: 10,
        })
        .unwrap()
        .remove(0);

    assert_eq!(
        service
            .tick(discovered_at + Duration::from_secs(10), 10)
            .unwrap(),
        1
    );
    assert_eq!(
        service
            .tick(discovered_at + Duration::from_secs(10), 10)
            .unwrap(),
        0
    );
    assert_eq!(executor.submissions.lock().unwrap().len(), 1);

    service
        .disable(&schedule.schedule_id, discovered_at)
        .unwrap();
    let before = store
        .get_schedule(&schedule.schedule_id)
        .unwrap()
        .unwrap()
        .next_trigger_at;
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
    assert_eq!(executor.submissions.lock().unwrap().len(), 2);
    assert_eq!(
        store
            .get_schedule(&schedule.schedule_id)
            .unwrap()
            .unwrap()
            .next_trigger_at,
        before
    );
    assert_eq!(
        store
            .get_schedule(&schedule.schedule_id)
            .unwrap()
            .unwrap()
            .enablement,
        ScheduleEnablement::Disabled
    );
    assert_eq!(
        store
            .list_triggers(TriggerQuery {
                schedule_id: schedule.schedule_id,
                kind: None,
                limit: 10,
            })
            .unwrap()
            .len(),
        2
    );
}

#[derive(Default)]
struct RecordingExecutor {
    submissions: Mutex<Vec<ExecutionSubmission>>,
}

impl ScheduleExecutor for RecordingExecutor {
    fn submit(
        &self,
        _action: &ActionDefinition,
        submission: ExecutionSubmission,
    ) -> SchedulerResult<ScheduleExecution> {
        let result = InvocationResult::success(&submission.request, json!({ "ok": true }));
        let execution_id = submission.request.execution_id.clone();
        self.submissions.lock().unwrap().push(submission);
        Ok(ScheduleExecution {
            execution_id,
            result: Some(result),
        })
    }
}

fn action() -> ActionDefinition {
    ActionDefinition {
        runtime: RuntimeKind::Python,
        kind: ActionKind::Schedule(ScheduleAction {
            key: "inventory-restock".into(),
            expression: "every 10s".into(),
        }),
        source: "src/restock.py".into(),
        entrypoint: "restock".into(),
        name: Some("restock".into()),
        policy: ActionExecutionPolicy::default(),
    }
}
