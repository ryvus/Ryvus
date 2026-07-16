use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use ryvus_execution::{
    action_revision, ActorRef, ExecutionDataReferences, ExecutionIdentityFactory, ExecutionScopeId,
    ExecutionSubmission, ExecutionTrigger, ManualExecutionSource,
};
use ryvus_protocol::{
    ActionDefinition, ActionKind, ExecutionAttempt, InvocationContext, InvocationRequest,
};
use serde_json::{json, Value};

use crate::{
    ClaimOccurrenceRequest, ClaimOccurrenceResult, ClaimedTrigger, DiscoveredSchedule,
    ManualTriggerRequest, ManualTriggerResult, ScheduleAvailability, ScheduleExecution,
    ScheduleExecutor, ScheduleId, ScheduleInterval, ScheduleStore, ScheduleTriggerKind,
    ScheduleTriggerRecord, ScheduleTriggerStatus, SchedulerError, SchedulerResult, TriggerFailure,
};

pub struct DurableSchedulerService<E> {
    store: Arc<dyn ScheduleStore>,
    executor: Arc<E>,
    identities: ExecutionIdentityFactory,
    scope: ExecutionScopeId,
    actor: ActorRef,
    owner: String,
    lease: Duration,
    started_at: SystemTime,
}

impl<E> DurableSchedulerService<E>
where
    E: ScheduleExecutor,
{
    pub fn new(
        store: Arc<dyn ScheduleStore>,
        executor: Arc<E>,
        scope: ExecutionScopeId,
        actor: ActorRef,
        owner: impl Into<String>,
        lease: Duration,
    ) -> Self {
        Self {
            store,
            executor,
            identities: ExecutionIdentityFactory,
            scope,
            actor,
            owner: owner.into(),
            lease,
            started_at: SystemTime::now(),
        }
    }

    pub fn reconcile(
        &self,
        actions: &[ActionDefinition],
        observed_at: SystemTime,
    ) -> SchedulerResult<crate::ReconcileResult> {
        let mut discovered = Vec::new();
        for action in actions {
            let ActionKind::Schedule(schedule) = &action.kind else {
                continue;
            };
            let interval = ScheduleInterval::parse(&schedule.expression)?.duration();
            discovered.push(DiscoveredSchedule {
                schedule_id: self.identities.schedule_id(&self.scope, &schedule.key),
                stable_schedule_key: schedule.key.clone(),
                display_name: action
                    .name
                    .clone()
                    .unwrap_or_else(|| action.entrypoint.clone()),
                action_id: action
                    .name
                    .clone()
                    .unwrap_or_else(|| action.entrypoint.clone()),
                action_revision: action_revision(action)
                    .map_err(|error| SchedulerError::StoreBackend(error.to_string()))?,
                action: action.clone(),
                expression: schedule.expression.clone(),
                interval,
            });
        }
        self.store.reconcile(&self.scope, &discovered, observed_at)
    }

    pub fn recover(&self, now: SystemTime, limit: usize) -> SchedulerResult<usize> {
        let triggers =
            self.store
                .recover_incomplete(&self.scope, &self.owner, now, self.lease, limit)?;
        let count = triggers.len();
        for trigger in triggers {
            self.process_claimed(trigger)?;
        }
        Ok(count)
    }

    pub async fn run(self: Arc<Self>) -> SchedulerResult<()>
    where
        E: 'static,
    {
        let service = Arc::clone(&self);
        tokio::task::spawn_blocking(move || service.recover(SystemTime::now(), 100))
            .await
            .map_err(|error| SchedulerError::StoreBackend(error.to_string()))??;
        loop {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let service = Arc::clone(&self);
            tokio::task::spawn_blocking(move || service.tick(SystemTime::now(), 100))
                .await
                .map_err(|error| SchedulerError::StoreBackend(error.to_string()))??;
        }
    }

    pub fn tick(&self, now: SystemTime, limit: usize) -> SchedulerResult<usize> {
        let due = self.store.list_due(&self.scope, now, limit)?;
        let mut processed = 0;
        for candidate in due {
            let missed = candidate.scheduled_for < self.started_at;
            let trigger_id = self
                .identities
                .scheduled_trigger(
                    &self.scope,
                    &candidate.schedule.schedule_id,
                    candidate.schedule.current_revision,
                    candidate.scheduled_for,
                )
                .map_err(|error| SchedulerError::StoreBackend(error.to_string()))?;
            let execution_id = if missed {
                None
            } else {
                Some(
                    self.identities
                        .scheduled_execution(
                            &self.scope,
                            &candidate.schedule.schedule_id,
                            candidate.schedule.current_revision,
                            candidate.scheduled_for,
                        )
                        .map_err(|error| SchedulerError::StoreBackend(error.to_string()))?,
                )
            };
            let claimed = self.store.claim_occurrence(ClaimOccurrenceRequest {
                execution_scope_id: self.scope.clone(),
                schedule_id: candidate.schedule.schedule_id.clone(),
                schedule_version: candidate.schedule.version,
                schedule_revision: candidate.schedule.current_revision,
                trigger_id,
                execution_id,
                scheduled_for: candidate.scheduled_for,
                observed_at: now,
                owner: self.owner.clone(),
                lease: self.lease,
            })?;
            let trigger = match claimed {
                ClaimOccurrenceResult::Claimed(trigger) => trigger,
                ClaimOccurrenceResult::Existing(trigger) => trigger,
                ClaimOccurrenceResult::Busy => continue,
                ClaimOccurrenceResult::Conflict => continue,
            };
            if missed && !trigger.status.is_terminal() {
                let missed = self
                    .store
                    .miss_trigger(&trigger.trigger_id, trigger.version)?;
                self.advance(&candidate.schedule, &candidate.revision, &missed, now)?;
            } else {
                self.process_claimed(ClaimedTrigger {
                    trigger,
                    revision: candidate.revision,
                })?;
            }
            processed += 1;
        }
        Ok(processed)
    }

    pub fn run_now(
        &self,
        schedule_id: &ScheduleId,
        event: Value,
        idempotency_key: Option<&str>,
        now: SystemTime,
    ) -> SchedulerResult<ScheduleExecution> {
        let schedule = self.store.get_schedule(schedule_id)?.ok_or_else(|| {
            SchedulerError::DurableScheduleNotFound {
                schedule_id: schedule_id.clone(),
            }
        })?;
        if schedule.availability != ScheduleAvailability::Available {
            return Err(SchedulerError::Conflict(
                "unavailable schedule cannot run now".into(),
            ));
        }
        let revision = self
            .store
            .list_revisions(schedule_id)?
            .into_iter()
            .find(|revision| revision.schedule_revision == schedule.current_revision)
            .ok_or_else(|| SchedulerError::StoreBackend("missing schedule revision".into()))?;
        let fingerprint = value_fingerprint(&event)?;
        let idempotency_key_hash =
            idempotency_key.map(|key| scoped_key(&self.scope, schedule_id, &self.actor, key));
        let reserved = self.store.create_manual_trigger(ManualTriggerRequest {
            execution_scope_id: self.scope.clone(),
            schedule_id: schedule_id.clone(),
            trigger_id: self.identities.random_trigger(),
            execution_id: self.identities.random_execution(),
            actor: self.actor.clone(),
            requested_at: now,
            claim_owner: self.owner.clone(),
            claim_expires_at: now.checked_add(self.lease).unwrap_or(now),
            idempotency_key_hash,
            immutable_request_fingerprint: fingerprint,
        })?;
        let trigger = match reserved {
            ManualTriggerResult::Created(trigger) => trigger,
            ManualTriggerResult::Existing(trigger)
                if trigger.status == ScheduleTriggerStatus::ExecutionCreated =>
            {
                return Ok(ScheduleExecution {
                    execution_id: trigger.execution_id.ok_or_else(|| {
                        SchedulerError::StoreBackend("linked trigger has no execution".into())
                    })?,
                    result: None,
                });
            }
            ManualTriggerResult::Existing(trigger) => trigger,
        };
        let execution = self.submit_manual(&revision, &trigger, event)?;
        self.store.link_execution(
            &trigger.trigger_id,
            &execution.execution_id,
            trigger.version,
        )?;
        Ok(execution)
    }

    pub fn enable(
        &self,
        schedule_id: &ScheduleId,
        at: SystemTime,
    ) -> SchedulerResult<crate::ScheduleRecord> {
        self.store.enable(schedule_id, &self.actor, at)
    }

    pub fn disable(
        &self,
        schedule_id: &ScheduleId,
        at: SystemTime,
    ) -> SchedulerResult<crate::ScheduleRecord> {
        self.store.disable(schedule_id, &self.actor, at)
    }

    pub fn get_schedule(
        &self,
        schedule_id: &ScheduleId,
    ) -> SchedulerResult<Option<crate::ScheduleRecord>> {
        self.store.get_schedule(schedule_id)
    }

    pub fn list_schedules(&self, limit: usize) -> SchedulerResult<Vec<crate::ScheduleRecord>> {
        self.store.list_schedules(crate::ScheduleQuery {
            execution_scope_id: Some(self.scope.clone()),
            limit,
        })
    }

    pub fn list_revisions(
        &self,
        schedule_id: &ScheduleId,
    ) -> SchedulerResult<Vec<crate::ScheduleRevisionRecord>> {
        self.store.list_revisions(schedule_id)
    }

    pub fn list_triggers(
        &self,
        schedule_id: &ScheduleId,
        kind: Option<ScheduleTriggerKind>,
        limit: usize,
    ) -> SchedulerResult<Vec<ScheduleTriggerRecord>> {
        self.store.list_triggers(crate::TriggerQuery {
            schedule_id: schedule_id.clone(),
            kind,
            limit,
        })
    }

    pub fn list_operational_events(
        &self,
        schedule_id: &ScheduleId,
        limit: usize,
    ) -> SchedulerResult<Vec<crate::ScheduleOperationalEvent>> {
        self.store.list_operational_events(schedule_id, limit)
    }

    fn process_claimed(&self, claimed: ClaimedTrigger) -> SchedulerResult<()> {
        if claimed.trigger.status == ScheduleTriggerStatus::ExecutionCreated {
            let schedule = self
                .store
                .get_schedule(&claimed.trigger.schedule_id)?
                .ok_or_else(|| SchedulerError::DurableScheduleNotFound {
                    schedule_id: claimed.trigger.schedule_id.clone(),
                })?;
            return self.advance(
                &schedule,
                &claimed.revision,
                &claimed.trigger,
                SystemTime::now(),
            );
        }
        let Some(execution_id) = claimed.trigger.execution_id.clone() else {
            return Err(SchedulerError::StoreBackend(
                "claimed scheduled trigger has no execution id".into(),
            ));
        };
        let scheduled_for = claimed.trigger.scheduled_for.ok_or_else(|| {
            SchedulerError::StoreBackend("scheduled trigger has no scheduled_for".into())
        })?;
        let request = request(
            execution_id.clone(),
            json!({
                "trigger": "schedule",
                "scheduled_at": unix_millis(scheduled_for),
                "expression": claimed.revision.schedule_expression,
            }),
        );
        let policy =
            ryvus_execution::ExecutionPolicy::from_action_policy(&claimed.revision.action.policy)
                .map_err(|error| SchedulerError::ExecutionFailed {
                action: claimed.revision.action_id.clone(),
                message: error.to_string(),
            })?;
        let execution = self.executor.submit(
            &claimed.revision.action,
            ExecutionSubmission {
                scope: self.scope.clone(),
                action_id: claimed.revision.action_id.clone(),
                trigger: ExecutionTrigger::Schedule {
                    schedule_id: claimed.trigger.schedule_id.clone(),
                    schedule_revision: claimed.trigger.schedule_revision,
                    trigger_id: claimed.trigger.trigger_id.clone(),
                    scheduled_for,
                },
                request,
                policy,
                data_refs: ExecutionDataReferences::default(),
            },
        );
        let execution = match execution {
            Ok(execution) if execution.execution_id == execution_id => execution,
            Ok(_) => {
                return Err(SchedulerError::Conflict(
                    "execution service returned a different execution id".into(),
                ));
            }
            Err(error) => {
                self.store.fail_trigger(
                    &claimed.trigger.trigger_id,
                    TriggerFailure {
                        code: "execution_creation_failed".into(),
                        summary: error.to_string(),
                    },
                    claimed.trigger.version,
                )?;
                return Err(error);
            }
        };
        let linked = self.store.link_execution(
            &claimed.trigger.trigger_id,
            &execution.execution_id,
            claimed.trigger.version,
        )?;
        let schedule = self
            .store
            .get_schedule(&linked.schedule_id)?
            .ok_or_else(|| SchedulerError::DurableScheduleNotFound {
                schedule_id: linked.schedule_id.clone(),
            })?;
        self.advance(&schedule, &claimed.revision, &linked, SystemTime::now())
    }

    fn submit_manual(
        &self,
        revision: &crate::ScheduleRevisionRecord,
        trigger: &ScheduleTriggerRecord,
        event: Value,
    ) -> SchedulerResult<ScheduleExecution> {
        let execution_id = trigger.execution_id.clone().ok_or_else(|| {
            SchedulerError::StoreBackend("manual trigger has no execution id".into())
        })?;
        let policy = ryvus_execution::ExecutionPolicy::from_action_policy(&revision.action.policy)
            .map_err(|error| SchedulerError::ExecutionFailed {
                action: revision.action_id.clone(),
                message: error.to_string(),
            })?;
        self.executor.submit(
            &revision.action,
            ExecutionSubmission {
                scope: self.scope.clone(),
                action_id: revision.action_id.clone(),
                trigger: ExecutionTrigger::Manual {
                    actor: self.actor.clone(),
                    source: ManualExecutionSource::Schedule {
                        schedule_id: trigger.schedule_id.clone(),
                        schedule_revision: trigger.schedule_revision,
                        trigger_id: trigger.trigger_id.clone(),
                    },
                },
                request: request(execution_id, event),
                policy,
                data_refs: ExecutionDataReferences::default(),
            },
        )
    }

    fn advance(
        &self,
        schedule: &crate::ScheduleRecord,
        revision: &crate::ScheduleRevisionRecord,
        trigger: &ScheduleTriggerRecord,
        now: SystemTime,
    ) -> SchedulerResult<()> {
        let scheduled_for = trigger.scheduled_for.ok_or_else(|| {
            SchedulerError::StoreBackend("scheduled trigger has no scheduled_for".into())
        })?;
        let next = first_after(scheduled_for, revision.interval, now)?;
        self.store.advance_schedule(
            &schedule.schedule_id,
            &trigger.trigger_id,
            schedule.version,
            next,
        )?;
        Ok(())
    }
}

fn request(execution_id: ryvus_protocol::ExecutionId, event: Value) -> InvocationRequest {
    InvocationRequest::with_attempt(
        event,
        InvocationContext::default(),
        ExecutionAttempt {
            execution_id,
            attempt_id: ryvus_protocol::AttemptId::new(),
            attempt_number: 1,
        },
    )
}

fn first_after(
    anchor: SystemTime,
    interval: Duration,
    now: SystemTime,
) -> SchedulerResult<SystemTime> {
    let elapsed = now.duration_since(anchor).unwrap_or_default();
    let steps = elapsed.as_nanos() / interval.as_nanos() + 1;
    let steps = u32::try_from(steps)
        .map_err(|_| SchedulerError::StoreBackend("schedule interval overflow".into()))?;
    anchor
        .checked_add(interval.saturating_mul(steps))
        .ok_or_else(|| SchedulerError::StoreBackend("next trigger time overflow".into()))
}

fn unix_millis(time: SystemTime) -> u128 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

fn value_fingerprint(value: &Value) -> SchedulerResult<String> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| SchedulerError::StoreBackend(error.to_string()))?;
    Ok(format!("run-now-v1:{:016x}", fnv(&bytes)))
}

fn scoped_key(
    scope: &ExecutionScopeId,
    schedule_id: &ScheduleId,
    actor: &ActorRef,
    key: &str,
) -> String {
    let value = format!(
        "{}\0{}\0schedule_run_now\0{}\0{}",
        scope, schedule_id, actor, key
    );
    format!("run-now-key-v1:{:016x}", fnv(value.as_bytes()))
}

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}
