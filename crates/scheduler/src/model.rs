use std::time::{Duration, SystemTime};

use ryvus_execution::{ActorRef, ExecutionScopeId, ScheduleId, ScheduleTriggerId};
use ryvus_protocol::{ActionDefinition, ExecutionId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleAvailability {
    Available,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleEnablement {
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MisfirePolicy {
    SkipMissed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduleRecord {
    pub execution_scope_id: ExecutionScopeId,
    pub schedule_id: ScheduleId,
    pub stable_schedule_key: String,
    pub display_name: String,
    pub current_revision: u64,
    pub availability: ScheduleAvailability,
    pub enablement: ScheduleEnablement,
    pub next_trigger_at: Option<SystemTime>,
    pub last_scheduled_trigger_at: Option<SystemTime>,
    pub last_discovered_at: SystemTime,
    pub unavailable_since: Option<SystemTime>,
    pub misfire_policy: MisfirePolicy,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
    pub version: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduleRevisionRecord {
    pub execution_scope_id: ExecutionScopeId,
    pub schedule_id: ScheduleId,
    pub schedule_revision: u64,
    pub action_id: String,
    pub action_revision: String,
    pub action: ActionDefinition,
    pub schedule_expression: String,
    pub interval: Duration,
    pub created_at: SystemTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleTriggerKind {
    Scheduled,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleTriggerStatus {
    Pending,
    Claimed,
    ExecutionCreated,
    Missed,
    Failed,
}

impl ScheduleTriggerStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::ExecutionCreated | Self::Missed | Self::Failed)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduleTriggerRecord {
    pub execution_scope_id: ExecutionScopeId,
    pub trigger_id: ScheduleTriggerId,
    pub schedule_id: ScheduleId,
    pub schedule_revision: u64,
    pub action_id: String,
    pub action_revision: String,
    pub kind: ScheduleTriggerKind,
    pub scheduled_for: Option<SystemTime>,
    pub observed_at: Option<SystemTime>,
    pub requested_at: Option<SystemTime>,
    pub requested_by: Option<ActorRef>,
    pub status: ScheduleTriggerStatus,
    pub execution_id: Option<ExecutionId>,
    pub claim_owner: Option<String>,
    pub claim_expires_at: Option<SystemTime>,
    pub failure_code: Option<String>,
    pub failure_summary: Option<String>,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
    pub version: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleOperationalEventKind {
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduleOperationalEvent {
    pub schedule_id: ScheduleId,
    pub kind: ScheduleOperationalEventKind,
    pub actor: ActorRef,
    pub occurred_at: SystemTime,
}

#[derive(Debug, Clone)]
pub struct DiscoveredSchedule {
    pub schedule_id: ScheduleId,
    pub stable_schedule_key: String,
    pub display_name: String,
    pub action_id: String,
    pub action_revision: String,
    pub action: ActionDefinition,
    pub expression: String,
    pub interval: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileResult {
    pub created: usize,
    pub updated: usize,
    pub unavailable: usize,
}

#[derive(Debug, Clone)]
pub struct DueSchedule {
    pub schedule: ScheduleRecord,
    pub revision: ScheduleRevisionRecord,
    pub scheduled_for: SystemTime,
}

#[derive(Debug, Clone)]
pub struct ClaimOccurrenceRequest {
    pub execution_scope_id: ExecutionScopeId,
    pub schedule_id: ScheduleId,
    pub schedule_version: u64,
    pub schedule_revision: u64,
    pub trigger_id: ScheduleTriggerId,
    pub execution_id: Option<ExecutionId>,
    pub scheduled_for: SystemTime,
    pub observed_at: SystemTime,
    pub owner: String,
    pub lease: Duration,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClaimOccurrenceResult {
    Claimed(ScheduleTriggerRecord),
    Existing(ScheduleTriggerRecord),
    Busy,
    Conflict,
}

#[derive(Debug, Clone)]
pub struct ClaimedTrigger {
    pub trigger: ScheduleTriggerRecord,
    pub revision: ScheduleRevisionRecord,
}

#[derive(Debug, Clone)]
pub struct TriggerFailure {
    pub code: String,
    pub summary: String,
}

#[derive(Debug, Clone)]
pub struct ManualTriggerRequest {
    pub execution_scope_id: ExecutionScopeId,
    pub schedule_id: ScheduleId,
    pub trigger_id: ScheduleTriggerId,
    pub execution_id: ExecutionId,
    pub actor: ActorRef,
    pub requested_at: SystemTime,
    pub claim_owner: String,
    pub claim_expires_at: SystemTime,
    pub idempotency_key_hash: Option<String>,
    pub immutable_request_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ManualTriggerResult {
    Created(ScheduleTriggerRecord),
    Existing(ScheduleTriggerRecord),
}

#[derive(Debug, Clone, Default)]
pub struct ScheduleQuery {
    pub execution_scope_id: Option<ExecutionScopeId>,
    pub limit: usize,
}

#[derive(Debug, Clone)]
pub struct TriggerQuery {
    pub schedule_id: ScheduleId,
    pub kind: Option<ScheduleTriggerKind>,
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManualIdempotencyRecord {
    pub execution_scope_id: ExecutionScopeId,
    pub schedule_id: ScheduleId,
    pub key_hash: String,
    pub fingerprint: String,
    pub trigger_id: ScheduleTriggerId,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScheduleStoreSnapshot {
    pub schedules: Vec<ScheduleRecord>,
    pub revisions: Vec<ScheduleRevisionRecord>,
    pub triggers: Vec<ScheduleTriggerRecord>,
    pub manual_idempotency: Vec<ManualIdempotencyRecord>,
    pub operational_events: Vec<ScheduleOperationalEvent>,
}
