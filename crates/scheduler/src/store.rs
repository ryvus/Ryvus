use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, SystemTime},
};

use ryvus_execution::{ActorRef, ExecutionScopeId, ScheduleId, ScheduleTriggerId};
use ryvus_protocol::ExecutionId;

use crate::{
    ClaimOccurrenceRequest, ClaimOccurrenceResult, ClaimedTrigger, DiscoveredSchedule, DueSchedule,
    ManualIdempotencyRecord, ManualTriggerRequest, ManualTriggerResult, ReconcileResult,
    ScheduleAvailability, ScheduleEnablement, ScheduleOperationalEvent,
    ScheduleOperationalEventKind, SchedulePage, ScheduleQuery, ScheduleRecord,
    ScheduleRevisionRecord, ScheduleStoreSnapshot, ScheduleTriggerKind, ScheduleTriggerRecord,
    ScheduleTriggerStatus, SchedulerError, SchedulerResult, TriggerFailure, TriggerPage,
    TriggerQuery,
};

#[cfg_attr(test, mockall::automock)]
pub trait ScheduleStore: Send + Sync {
    fn reconcile(
        &self,
        scope: &ExecutionScopeId,
        discovered: &[DiscoveredSchedule],
        observed_at: SystemTime,
    ) -> SchedulerResult<ReconcileResult>;
    fn list_due(
        &self,
        scope: &ExecutionScopeId,
        now: SystemTime,
        limit: usize,
    ) -> SchedulerResult<Vec<DueSchedule>>;
    fn claim_occurrence(
        &self,
        request: ClaimOccurrenceRequest,
    ) -> SchedulerResult<ClaimOccurrenceResult>;
    fn recover_incomplete(
        &self,
        scope: &ExecutionScopeId,
        owner: &str,
        now: SystemTime,
        lease: Duration,
        limit: usize,
    ) -> SchedulerResult<Vec<ClaimedTrigger>>;
    fn link_execution(
        &self,
        trigger_id: &ScheduleTriggerId,
        execution_id: &ExecutionId,
        expected_version: u64,
    ) -> SchedulerResult<ScheduleTriggerRecord>;
    fn advance_schedule(
        &self,
        schedule_id: &ScheduleId,
        completed_trigger_id: &ScheduleTriggerId,
        expected_version: u64,
        next_trigger_at: SystemTime,
    ) -> SchedulerResult<ScheduleRecord>;
    fn miss_trigger(
        &self,
        trigger_id: &ScheduleTriggerId,
        expected_version: u64,
    ) -> SchedulerResult<ScheduleTriggerRecord>;
    fn fail_trigger(
        &self,
        trigger_id: &ScheduleTriggerId,
        failure: TriggerFailure,
        expected_version: u64,
    ) -> SchedulerResult<ScheduleTriggerRecord>;
    fn enable(
        &self,
        schedule_id: &ScheduleId,
        actor: &ActorRef,
        at: SystemTime,
    ) -> SchedulerResult<ScheduleRecord>;
    fn disable(
        &self,
        schedule_id: &ScheduleId,
        actor: &ActorRef,
        at: SystemTime,
    ) -> SchedulerResult<ScheduleRecord>;
    fn create_manual_trigger(
        &self,
        request: ManualTriggerRequest,
    ) -> SchedulerResult<ManualTriggerResult>;
    fn get_schedule(&self, schedule_id: &ScheduleId) -> SchedulerResult<Option<ScheduleRecord>>;
    fn get_trigger(
        &self,
        trigger_id: &ScheduleTriggerId,
    ) -> SchedulerResult<Option<ScheduleTriggerRecord>>;
    fn list_schedules(&self, query: ScheduleQuery) -> SchedulerResult<SchedulePage>;
    fn list_revisions(
        &self,
        schedule_id: &ScheduleId,
    ) -> SchedulerResult<Vec<ScheduleRevisionRecord>>;
    fn list_triggers(&self, query: TriggerQuery) -> SchedulerResult<TriggerPage>;
    fn list_operational_events(
        &self,
        schedule_id: &ScheduleId,
        limit: usize,
    ) -> SchedulerResult<Vec<ScheduleOperationalEvent>>;
}

#[derive(Default)]
pub struct MemoryScheduleStore {
    state: Mutex<MemoryState>,
}

#[derive(Default)]
struct MemoryState {
    schedules: HashMap<ScheduleId, ScheduleRecord>,
    revisions: HashMap<(ScheduleId, u64), ScheduleRevisionRecord>,
    triggers: HashMap<ScheduleTriggerId, ScheduleTriggerRecord>,
    occurrence_ids: HashMap<(ScheduleId, u64, SystemTime), ScheduleTriggerId>,
    manual_keys: HashMap<(ExecutionScopeId, ScheduleId, String), ManualReservation>,
    operational_events: Vec<ScheduleOperationalEvent>,
}

#[derive(Clone)]
struct ManualReservation {
    fingerprint: String,
    trigger_id: ScheduleTriggerId,
}

impl ScheduleStore for MemoryScheduleStore {
    fn reconcile(
        &self,
        scope: &ExecutionScopeId,
        discovered: &[DiscoveredSchedule],
        observed_at: SystemTime,
    ) -> SchedulerResult<ReconcileResult> {
        let mut state = self.lock()?;
        let mut result = ReconcileResult {
            created: 0,
            updated: 0,
            unavailable: 0,
        };
        let discovered_by_id = discovered
            .iter()
            .map(|schedule| (schedule.schedule_id.clone(), schedule))
            .collect::<HashMap<_, _>>();

        for schedule in discovered {
            match state.schedules.get(&schedule.schedule_id).cloned() {
                None => {
                    let record = ScheduleRecord {
                        execution_scope_id: scope.clone(),
                        schedule_id: schedule.schedule_id.clone(),
                        stable_schedule_key: schedule.stable_schedule_key.clone(),
                        display_name: schedule.display_name.clone(),
                        current_revision: 1,
                        availability: ScheduleAvailability::Available,
                        enablement: ScheduleEnablement::Enabled,
                        next_trigger_at: observed_at.checked_add(schedule.interval),
                        last_scheduled_trigger_at: None,
                        last_discovered_at: observed_at,
                        unavailable_since: None,
                        misfire_policy: crate::MisfirePolicy::SkipMissed,
                        created_at: observed_at,
                        updated_at: observed_at,
                        version: 0,
                    };
                    state.revisions.insert(
                        (schedule.schedule_id.clone(), 1),
                        revision(schedule, scope, 1, observed_at),
                    );
                    state.schedules.insert(schedule.schedule_id.clone(), record);
                    result.created += 1;
                }
                Some(mut current) => {
                    if current.execution_scope_id != *scope
                        || current.stable_schedule_key != schedule.stable_schedule_key
                    {
                        return Err(SchedulerError::Conflict(format!(
                            "schedule id '{}' belongs to another scope or key",
                            schedule.schedule_id
                        )));
                    }
                    let current_revision = state
                        .revisions
                        .get(&(schedule.schedule_id.clone(), current.current_revision))
                        .ok_or_else(|| {
                            SchedulerError::StoreBackend("missing schedule revision".into())
                        })?;
                    let changed = current_revision.action_id != schedule.action_id
                        || current_revision.action_revision != schedule.action_revision
                        || current_revision.schedule_expression != schedule.expression;
                    let rediscovered = current.availability == ScheduleAvailability::Unavailable;
                    if changed {
                        current.current_revision =
                            current.current_revision.checked_add(1).ok_or_else(|| {
                                SchedulerError::StoreBackend("schedule revision overflow".into())
                            })?;
                        state.revisions.insert(
                            (schedule.schedule_id.clone(), current.current_revision),
                            revision(schedule, scope, current.current_revision, observed_at),
                        );
                    }
                    current.display_name = schedule.display_name.clone();
                    current.availability = ScheduleAvailability::Available;
                    current.unavailable_since = None;
                    current.last_discovered_at = observed_at;
                    if changed || rediscovered {
                        current.next_trigger_at = observed_at.checked_add(schedule.interval);
                    }
                    if changed || rediscovered {
                        current.version = increment(current.version, "schedule version")?;
                        current.updated_at = observed_at;
                        result.updated += 1;
                    }
                    state
                        .schedules
                        .insert(schedule.schedule_id.clone(), current);
                }
            }
        }

        let missing = state
            .schedules
            .values()
            .filter(|schedule| {
                schedule.execution_scope_id == *scope
                    && schedule.availability == ScheduleAvailability::Available
                    && !discovered_by_id.contains_key(&schedule.schedule_id)
            })
            .map(|schedule| schedule.schedule_id.clone())
            .collect::<Vec<_>>();
        for schedule_id in missing {
            let schedule = state.schedules.get_mut(&schedule_id).ok_or_else(|| {
                SchedulerError::DurableScheduleNotFound {
                    schedule_id: schedule_id.clone(),
                }
            })?;
            schedule.availability = ScheduleAvailability::Unavailable;
            schedule.unavailable_since = Some(observed_at);
            schedule.next_trigger_at = None;
            schedule.updated_at = observed_at;
            schedule.version = increment(schedule.version, "schedule version")?;
            result.unavailable += 1;
        }

        Ok(result)
    }

    fn list_due(
        &self,
        scope: &ExecutionScopeId,
        now: SystemTime,
        limit: usize,
    ) -> SchedulerResult<Vec<DueSchedule>> {
        let state = self.lock()?;
        let mut due = state
            .schedules
            .values()
            .filter(|schedule| {
                schedule.execution_scope_id == *scope
                    && schedule.availability == ScheduleAvailability::Available
                    && schedule.enablement == ScheduleEnablement::Enabled
                    && schedule.next_trigger_at.is_some_and(|next| next <= now)
            })
            .filter_map(|schedule| {
                let revision = state
                    .revisions
                    .get(&(schedule.schedule_id.clone(), schedule.current_revision))?;
                Some(DueSchedule {
                    schedule: schedule.clone(),
                    revision: revision.clone(),
                    scheduled_for: schedule.next_trigger_at?,
                })
            })
            .collect::<Vec<_>>();
        due.sort_by_key(|candidate| candidate.scheduled_for);
        due.truncate(limit.clamp(1, 100));
        Ok(due)
    }

    fn claim_occurrence(
        &self,
        request: ClaimOccurrenceRequest,
    ) -> SchedulerResult<ClaimOccurrenceResult> {
        let mut state = self.lock()?;
        let occurrence = (
            request.schedule_id.clone(),
            request.schedule_revision,
            request.scheduled_for,
        );
        if let Some(trigger_id) = state.occurrence_ids.get(&occurrence).cloned() {
            let trigger = state
                .triggers
                .get_mut(&trigger_id)
                .ok_or_else(|| SchedulerError::StoreBackend("missing occurrence trigger".into()))?;
            if trigger.status.is_terminal() {
                return Ok(ClaimOccurrenceResult::Existing(trigger.clone()));
            }
            if trigger
                .claim_expires_at
                .is_some_and(|expires_at| expires_at <= request.observed_at)
            {
                trigger.claim_owner = Some(request.owner);
                trigger.claim_expires_at = request.observed_at.checked_add(request.lease);
                trigger.updated_at = request.observed_at;
                trigger.version += 1;
                return Ok(ClaimOccurrenceResult::Claimed(trigger.clone()));
            }
            return Ok(ClaimOccurrenceResult::Busy);
        }
        let schedule = state.schedules.get(&request.schedule_id).ok_or_else(|| {
            SchedulerError::DurableScheduleNotFound {
                schedule_id: request.schedule_id.clone(),
            }
        })?;
        if schedule.execution_scope_id != request.execution_scope_id
            || schedule.version != request.schedule_version
            || schedule.current_revision != request.schedule_revision
            || schedule.availability != ScheduleAvailability::Available
            || schedule.enablement != ScheduleEnablement::Enabled
            || schedule.next_trigger_at != Some(request.scheduled_for)
        {
            return Ok(ClaimOccurrenceResult::Conflict);
        }
        let revision = state
            .revisions
            .get(&(request.schedule_id.clone(), request.schedule_revision))
            .ok_or_else(|| SchedulerError::StoreBackend("missing schedule revision".into()))?;
        let trigger = ScheduleTriggerRecord {
            execution_scope_id: request.execution_scope_id,
            trigger_id: request.trigger_id.clone(),
            schedule_id: request.schedule_id,
            schedule_revision: request.schedule_revision,
            action_id: revision.action_id.clone(),
            action_revision: revision.action_revision.clone(),
            kind: ScheduleTriggerKind::Scheduled,
            scheduled_for: Some(request.scheduled_for),
            observed_at: Some(request.observed_at),
            requested_at: None,
            requested_by: None,
            status: ScheduleTriggerStatus::Claimed,
            execution_id: request.execution_id,
            claim_owner: Some(request.owner),
            claim_expires_at: request.observed_at.checked_add(request.lease),
            failure_code: None,
            failure_summary: None,
            created_at: request.observed_at,
            updated_at: request.observed_at,
            version: 0,
        };
        state
            .occurrence_ids
            .insert(occurrence, request.trigger_id.clone());
        state.triggers.insert(request.trigger_id, trigger.clone());
        Ok(ClaimOccurrenceResult::Claimed(trigger))
    }

    fn recover_incomplete(
        &self,
        scope: &ExecutionScopeId,
        owner: &str,
        now: SystemTime,
        lease: Duration,
        limit: usize,
    ) -> SchedulerResult<Vec<ClaimedTrigger>> {
        let mut state = self.lock()?;
        let ids = state
            .triggers
            .values()
            .filter(|trigger| {
                let linked_schedule_needs_advance = trigger.kind == ScheduleTriggerKind::Scheduled
                    && trigger.status == ScheduleTriggerStatus::ExecutionCreated
                    && state
                        .schedules
                        .get(&trigger.schedule_id)
                        .is_some_and(|schedule| {
                            schedule.last_scheduled_trigger_at != trigger.scheduled_for
                        });
                trigger.execution_scope_id == *scope
                    && (linked_schedule_needs_advance
                        || (!trigger.status.is_terminal()
                            && trigger.claim_expires_at.is_none_or(|expiry| expiry <= now)))
            })
            .take(limit.clamp(1, 100))
            .map(|trigger| trigger.trigger_id.clone())
            .collect::<Vec<_>>();
        let mut claimed = Vec::with_capacity(ids.len());
        for id in ids {
            let trigger =
                {
                    let trigger = state.triggers.get_mut(&id).ok_or_else(|| {
                        SchedulerError::TriggerNotFound {
                            trigger_id: id.clone(),
                        }
                    })?;
                    if trigger.status != ScheduleTriggerStatus::ExecutionCreated {
                        trigger.status = ScheduleTriggerStatus::Claimed;
                        trigger.claim_owner = Some(owner.to_string());
                        trigger.claim_expires_at = now.checked_add(lease);
                        trigger.updated_at = now;
                        trigger.version = increment(trigger.version, "trigger version")?;
                    }
                    trigger.clone()
                };
            let revision = state
                .revisions
                .get(&(trigger.schedule_id.clone(), trigger.schedule_revision))
                .ok_or_else(|| SchedulerError::StoreBackend("missing schedule revision".into()))?
                .clone();
            claimed.push(ClaimedTrigger { trigger, revision });
        }
        Ok(claimed)
    }

    fn link_execution(
        &self,
        trigger_id: &ScheduleTriggerId,
        execution_id: &ExecutionId,
        expected_version: u64,
    ) -> SchedulerResult<ScheduleTriggerRecord> {
        let mut state = self.lock()?;
        let trigger = trigger_mut(&mut state, trigger_id)?;
        if trigger.status == ScheduleTriggerStatus::ExecutionCreated
            && trigger.execution_id.as_ref() == Some(execution_id)
        {
            return Ok(trigger.clone());
        }
        check_version(trigger.version, expected_version)?;
        if trigger.status.is_terminal() || trigger.execution_id.as_ref() != Some(execution_id) {
            return Err(SchedulerError::Conflict(
                "trigger cannot link a different execution".into(),
            ));
        }
        trigger.status = ScheduleTriggerStatus::ExecutionCreated;
        trigger.claim_owner = None;
        trigger.claim_expires_at = None;
        trigger.updated_at = SystemTime::now();
        trigger.version = increment(trigger.version, "trigger version")?;
        Ok(trigger.clone())
    }

    fn advance_schedule(
        &self,
        schedule_id: &ScheduleId,
        completed_trigger_id: &ScheduleTriggerId,
        expected_version: u64,
        next_trigger_at: SystemTime,
    ) -> SchedulerResult<ScheduleRecord> {
        let mut state = self.lock()?;
        let trigger = state.triggers.get(completed_trigger_id).ok_or_else(|| {
            SchedulerError::TriggerNotFound {
                trigger_id: completed_trigger_id.clone(),
            }
        })?;
        if trigger.schedule_id != *schedule_id || !trigger.status.is_terminal() {
            return Err(SchedulerError::Conflict(
                "only a terminal trigger may advance its schedule".into(),
            ));
        }
        let scheduled_for = trigger.scheduled_for;
        let schedule = state.schedules.get_mut(schedule_id).ok_or_else(|| {
            SchedulerError::DurableScheduleNotFound {
                schedule_id: schedule_id.clone(),
            }
        })?;
        if schedule.last_scheduled_trigger_at == scheduled_for
            && schedule.next_trigger_at == Some(next_trigger_at)
        {
            return Ok(schedule.clone());
        }
        check_version(schedule.version, expected_version)?;
        schedule.last_scheduled_trigger_at = scheduled_for;
        schedule.next_trigger_at = Some(next_trigger_at);
        schedule.updated_at = SystemTime::now();
        schedule.version = increment(schedule.version, "schedule version")?;
        Ok(schedule.clone())
    }

    fn miss_trigger(
        &self,
        trigger_id: &ScheduleTriggerId,
        expected_version: u64,
    ) -> SchedulerResult<ScheduleTriggerRecord> {
        terminal_trigger(
            self,
            trigger_id,
            expected_version,
            ScheduleTriggerStatus::Missed,
            None,
        )
    }

    fn fail_trigger(
        &self,
        trigger_id: &ScheduleTriggerId,
        failure: TriggerFailure,
        expected_version: u64,
    ) -> SchedulerResult<ScheduleTriggerRecord> {
        terminal_trigger(
            self,
            trigger_id,
            expected_version,
            ScheduleTriggerStatus::Failed,
            Some(failure),
        )
    }

    fn enable(
        &self,
        schedule_id: &ScheduleId,
        actor: &ActorRef,
        at: SystemTime,
    ) -> SchedulerResult<ScheduleRecord> {
        set_enablement(self, schedule_id, actor, at, ScheduleEnablement::Enabled)
    }

    fn disable(
        &self,
        schedule_id: &ScheduleId,
        actor: &ActorRef,
        at: SystemTime,
    ) -> SchedulerResult<ScheduleRecord> {
        set_enablement(self, schedule_id, actor, at, ScheduleEnablement::Disabled)
    }

    fn create_manual_trigger(
        &self,
        request: ManualTriggerRequest,
    ) -> SchedulerResult<ManualTriggerResult> {
        let mut state = self.lock()?;
        if let Some(key) = &request.idempotency_key_hash {
            let index = (
                request.execution_scope_id.clone(),
                request.schedule_id.clone(),
                key.clone(),
            );
            if let Some(existing) = state.manual_keys.get(&index) {
                if existing.fingerprint != request.immutable_request_fingerprint {
                    return Err(SchedulerError::Conflict(
                        "manual trigger idempotency key was reused with different input".into(),
                    ));
                }
                let trigger = state
                    .triggers
                    .get(&existing.trigger_id)
                    .ok_or_else(|| SchedulerError::StoreBackend("missing manual trigger".into()))?;
                return Ok(ManualTriggerResult::Existing(trigger.clone()));
            }
        }
        let schedule = state.schedules.get(&request.schedule_id).ok_or_else(|| {
            SchedulerError::DurableScheduleNotFound {
                schedule_id: request.schedule_id.clone(),
            }
        })?;
        if schedule.execution_scope_id != request.execution_scope_id
            || schedule.availability != ScheduleAvailability::Available
        {
            return Err(SchedulerError::Conflict(
                "unavailable schedule cannot run now".into(),
            ));
        }
        let revision = state
            .revisions
            .get(&(request.schedule_id.clone(), schedule.current_revision))
            .ok_or_else(|| SchedulerError::StoreBackend("missing schedule revision".into()))?;
        let trigger = ScheduleTriggerRecord {
            execution_scope_id: request.execution_scope_id.clone(),
            trigger_id: request.trigger_id.clone(),
            schedule_id: request.schedule_id.clone(),
            schedule_revision: schedule.current_revision,
            action_id: revision.action_id.clone(),
            action_revision: revision.action_revision.clone(),
            kind: ScheduleTriggerKind::Manual,
            scheduled_for: None,
            observed_at: None,
            requested_at: Some(request.requested_at),
            requested_by: Some(request.actor),
            status: ScheduleTriggerStatus::Claimed,
            execution_id: Some(request.execution_id),
            claim_owner: Some(request.claim_owner),
            claim_expires_at: Some(request.claim_expires_at),
            failure_code: None,
            failure_summary: None,
            created_at: request.requested_at,
            updated_at: request.requested_at,
            version: 0,
        };
        if let Some(key) = request.idempotency_key_hash {
            state.manual_keys.insert(
                (request.execution_scope_id, request.schedule_id, key),
                ManualReservation {
                    fingerprint: request.immutable_request_fingerprint,
                    trigger_id: request.trigger_id.clone(),
                },
            );
        }
        state.triggers.insert(request.trigger_id, trigger.clone());
        Ok(ManualTriggerResult::Created(trigger))
    }

    fn get_schedule(&self, schedule_id: &ScheduleId) -> SchedulerResult<Option<ScheduleRecord>> {
        Ok(self.lock()?.schedules.get(schedule_id).cloned())
    }

    fn get_trigger(
        &self,
        trigger_id: &ScheduleTriggerId,
    ) -> SchedulerResult<Option<ScheduleTriggerRecord>> {
        Ok(self.lock()?.triggers.get(trigger_id).cloned())
    }

    fn list_schedules(&self, query: ScheduleQuery) -> SchedulerResult<SchedulePage> {
        let state = self.lock()?;
        let mut records = state
            .schedules
            .values()
            .filter(|schedule| {
                query
                    .execution_scope_id
                    .as_ref()
                    .is_none_or(|scope| scope == &schedule.execution_scope_id)
            })
            .cloned()
            .collect::<Vec<_>>();
        records.sort_by(|left, right| {
            left.stable_schedule_key
                .cmp(&right.stable_schedule_key)
                .then_with(|| left.schedule_id.as_ref().cmp(right.schedule_id.as_ref()))
        });
        if let Some(cursor) = query.cursor {
            let position = records
                .iter()
                .position(|record| record.schedule_id == cursor)
                .ok_or_else(|| SchedulerError::InvalidCursor(cursor.to_string()))?;
            records.drain(..=position);
        }
        let limit = query.limit.clamp(1, 100);
        records.truncate(limit + 1);
        let has_more = records.len() > limit;
        records.truncate(limit);
        let next_cursor = has_more
            .then(|| records.last().map(|item| item.schedule_id.clone()))
            .flatten();
        Ok(SchedulePage {
            items: records,
            next_cursor,
        })
    }

    fn list_revisions(
        &self,
        schedule_id: &ScheduleId,
    ) -> SchedulerResult<Vec<ScheduleRevisionRecord>> {
        let state = self.lock()?;
        let mut revisions = state
            .revisions
            .iter()
            .filter(|((id, _), _)| id == schedule_id)
            .map(|(_, revision)| revision.clone())
            .collect::<Vec<_>>();
        revisions.sort_by_key(|revision| revision.schedule_revision);
        Ok(revisions)
    }

    fn list_triggers(&self, query: TriggerQuery) -> SchedulerResult<TriggerPage> {
        let state = self.lock()?;
        let mut triggers = state
            .triggers
            .values()
            .filter(|trigger| {
                trigger.schedule_id == query.schedule_id
                    && query.kind.is_none_or(|kind| kind == trigger.kind)
            })
            .cloned()
            .collect::<Vec<_>>();
        triggers.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| right.trigger_id.as_ref().cmp(left.trigger_id.as_ref()))
        });
        if let Some(cursor) = query.cursor {
            let position = triggers
                .iter()
                .position(|trigger| trigger.trigger_id == cursor)
                .ok_or_else(|| SchedulerError::InvalidCursor(cursor.to_string()))?;
            triggers.drain(..=position);
        }
        let limit = query.limit.clamp(1, 100);
        triggers.truncate(limit + 1);
        let has_more = triggers.len() > limit;
        triggers.truncate(limit);
        let next_cursor = has_more
            .then(|| triggers.last().map(|item| item.trigger_id.clone()))
            .flatten();
        Ok(TriggerPage {
            items: triggers,
            next_cursor,
        })
    }

    fn list_operational_events(
        &self,
        schedule_id: &ScheduleId,
        limit: usize,
    ) -> SchedulerResult<Vec<ScheduleOperationalEvent>> {
        let state = self.lock()?;
        Ok(state
            .operational_events
            .iter()
            .rev()
            .filter(|event| &event.schedule_id == schedule_id)
            .take(limit.clamp(1, 100))
            .cloned()
            .collect())
    }
}

impl MemoryScheduleStore {
    pub fn from_snapshot(snapshot: ScheduleStoreSnapshot) -> Self {
        let mut state = MemoryState::default();
        for schedule in snapshot.schedules {
            state
                .schedules
                .insert(schedule.schedule_id.clone(), schedule);
        }
        for revision in snapshot.revisions {
            state.revisions.insert(
                (revision.schedule_id.clone(), revision.schedule_revision),
                revision,
            );
        }
        for trigger in snapshot.triggers {
            if let Some(scheduled_for) = trigger.scheduled_for {
                state.occurrence_ids.insert(
                    (
                        trigger.schedule_id.clone(),
                        trigger.schedule_revision,
                        scheduled_for,
                    ),
                    trigger.trigger_id.clone(),
                );
            }
            state.triggers.insert(trigger.trigger_id.clone(), trigger);
        }
        for reservation in snapshot.manual_idempotency {
            state.manual_keys.insert(
                (
                    reservation.execution_scope_id,
                    reservation.schedule_id,
                    reservation.key_hash,
                ),
                ManualReservation {
                    fingerprint: reservation.fingerprint,
                    trigger_id: reservation.trigger_id,
                },
            );
        }
        state.operational_events = snapshot.operational_events;
        Self {
            state: Mutex::new(state),
        }
    }

    pub fn snapshot(&self) -> SchedulerResult<ScheduleStoreSnapshot> {
        let state = self.lock()?;
        Ok(ScheduleStoreSnapshot {
            schedules: state.schedules.values().cloned().collect(),
            revisions: state.revisions.values().cloned().collect(),
            triggers: state.triggers.values().cloned().collect(),
            manual_idempotency: state
                .manual_keys
                .iter()
                .map(
                    |((scope, schedule_id, key_hash), reservation)| ManualIdempotencyRecord {
                        execution_scope_id: scope.clone(),
                        schedule_id: schedule_id.clone(),
                        key_hash: key_hash.clone(),
                        fingerprint: reservation.fingerprint.clone(),
                        trigger_id: reservation.trigger_id.clone(),
                    },
                )
                .collect(),
            operational_events: state.operational_events.clone(),
        })
    }

    fn lock(&self) -> SchedulerResult<std::sync::MutexGuard<'_, MemoryState>> {
        self.state
            .lock()
            .map_err(|_| SchedulerError::StoreLockPoisoned)
    }
}

fn revision(
    schedule: &DiscoveredSchedule,
    scope: &ExecutionScopeId,
    number: u64,
    created_at: SystemTime,
) -> ScheduleRevisionRecord {
    ScheduleRevisionRecord {
        execution_scope_id: scope.clone(),
        schedule_id: schedule.schedule_id.clone(),
        schedule_revision: number,
        action_id: schedule.action_id.clone(),
        action_revision: schedule.action_revision.clone(),
        action: schedule.action.clone(),
        schedule_expression: schedule.expression.clone(),
        interval: schedule.interval,
        created_at,
    }
}

fn trigger_mut<'a>(
    state: &'a mut MemoryState,
    trigger_id: &ScheduleTriggerId,
) -> SchedulerResult<&'a mut ScheduleTriggerRecord> {
    state
        .triggers
        .get_mut(trigger_id)
        .ok_or_else(|| SchedulerError::TriggerNotFound {
            trigger_id: trigger_id.clone(),
        })
}

fn check_version(current: u64, expected: u64) -> SchedulerResult<()> {
    if current == expected {
        Ok(())
    } else {
        Err(SchedulerError::Conflict(format!(
            "expected version {expected}, current version is {current}"
        )))
    }
}

fn increment(value: u64, label: &str) -> SchedulerResult<u64> {
    value
        .checked_add(1)
        .ok_or_else(|| SchedulerError::StoreBackend(format!("{label} overflow")))
}

fn terminal_trigger(
    store: &MemoryScheduleStore,
    trigger_id: &ScheduleTriggerId,
    expected_version: u64,
    status: ScheduleTriggerStatus,
    failure: Option<TriggerFailure>,
) -> SchedulerResult<ScheduleTriggerRecord> {
    let mut state = store.lock()?;
    let trigger = trigger_mut(&mut state, trigger_id)?;
    if trigger.status == status {
        return Ok(trigger.clone());
    }
    check_version(trigger.version, expected_version)?;
    if trigger.status.is_terminal() {
        return Err(SchedulerError::Conflict(
            "terminal trigger state is immutable".into(),
        ));
    }
    trigger.status = status;
    trigger.claim_owner = None;
    trigger.claim_expires_at = None;
    if let Some(failure) = failure {
        trigger.failure_code = Some(failure.code);
        trigger.failure_summary = Some(failure.summary);
    }
    trigger.updated_at = SystemTime::now();
    trigger.version = increment(trigger.version, "trigger version")?;
    Ok(trigger.clone())
}

fn set_enablement(
    store: &MemoryScheduleStore,
    schedule_id: &ScheduleId,
    actor: &ActorRef,
    at: SystemTime,
    enablement: ScheduleEnablement,
) -> SchedulerResult<ScheduleRecord> {
    let mut state = store.lock()?;
    let current = state.schedules.get(schedule_id).cloned().ok_or_else(|| {
        SchedulerError::DurableScheduleNotFound {
            schedule_id: schedule_id.clone(),
        }
    })?;
    if current.enablement == enablement {
        return Ok(current);
    }
    let next_trigger_at = if enablement == ScheduleEnablement::Enabled
        && current.availability == ScheduleAvailability::Available
    {
        let interval = state
            .revisions
            .get(&(schedule_id.clone(), current.current_revision))
            .ok_or_else(|| SchedulerError::StoreBackend("missing schedule revision".into()))?
            .interval;
        Some(
            at.checked_add(interval)
                .ok_or_else(|| SchedulerError::StoreBackend("next trigger time overflow".into()))?,
        )
    } else {
        current.next_trigger_at
    };
    let schedule = state.schedules.get_mut(schedule_id).ok_or_else(|| {
        SchedulerError::DurableScheduleNotFound {
            schedule_id: schedule_id.clone(),
        }
    })?;
    schedule.enablement = enablement;
    schedule.next_trigger_at = next_trigger_at;
    schedule.updated_at = at;
    schedule.version = increment(schedule.version, "schedule version")?;
    let result = schedule.clone();
    state.operational_events.push(ScheduleOperationalEvent {
        schedule_id: schedule_id.clone(),
        kind: match enablement {
            ScheduleEnablement::Enabled => ScheduleOperationalEventKind::Enabled,
            ScheduleEnablement::Disabled => ScheduleOperationalEventKind::Disabled,
        },
        actor: actor.clone(),
        occurred_at: at,
    });
    Ok(result)
}
