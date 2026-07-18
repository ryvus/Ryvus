use std::time::{SystemTime, UNIX_EPOCH};

use ryvus_protocol::{ExecutionId, ExecutionScopeId};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

const IDENTITY_NAMESPACE: Uuid = Uuid::from_bytes([
    0x1d, 0x75, 0xa1, 0x2c, 0x0a, 0x22, 0x4a, 0xe7, 0x91, 0xb7, 0x4f, 0xad, 0x77, 0x1e, 0x34, 0xd8,
]);

#[derive(Debug, Error, PartialEq, Eq)]
pub enum IdentityError {
    #[error("{kind} must not be empty")]
    Empty { kind: &'static str },
    #[error("scheduled time is before the Unix epoch")]
    TimeBeforeEpoch,
}

macro_rules! opaque_id {
    ($name:ident, $kind:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, IdentityError> {
                let value = value.into();
                if value.trim().is_empty() {
                    return Err(IdentityError::Empty { kind: $kind });
                }
                Ok(Self(value))
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    };
}

opaque_id!(ActorRef, "actor reference");
opaque_id!(ScheduleId, "schedule id");
opaque_id!(ScheduleTriggerId, "schedule trigger id");
opaque_id!(ExecutionDataRef, "execution data reference");

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionDataReferences {
    pub input_ref: Option<ExecutionDataRef>,
    pub result_ref: Option<ExecutionDataRef>,
    pub error_ref: Option<ExecutionDataRef>,
    pub event_stream_ref: Option<ExecutionDataRef>,
    pub artifact_manifest_ref: Option<ExecutionDataRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ManualExecutionSource {
    Schedule {
        schedule_id: ScheduleId,
        schedule_revision: u64,
        trigger_id: ScheduleTriggerId,
    },
    Direct,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExecutionTrigger {
    Api,
    Schedule {
        schedule_id: ScheduleId,
        schedule_revision: u64,
        trigger_id: ScheduleTriggerId,
        scheduled_for: SystemTime,
    },
    Flow {
        flow_run_id: String,
        step_key: String,
    },
    Manual {
        actor: ActorRef,
        source: ManualExecutionSource,
    },
    Queue {
        queue: String,
        message_id: Option<String>,
    },
    Unknown,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ExecutionIdentityFactory;

impl ExecutionIdentityFactory {
    pub fn random_execution(&self) -> ExecutionId {
        ExecutionId::new()
    }

    pub fn random_trigger(&self) -> ScheduleTriggerId {
        ScheduleTriggerId(Uuid::new_v4().to_string())
    }

    pub fn schedule_id(&self, scope: &ExecutionScopeId, stable_key: &str) -> ScheduleId {
        ScheduleId(named("schedule", &[scope.as_ref(), stable_key]))
    }

    pub fn scheduled_trigger(
        &self,
        scope: &ExecutionScopeId,
        schedule_id: &ScheduleId,
        schedule_revision: u64,
        scheduled_for: SystemTime,
    ) -> Result<ScheduleTriggerId, IdentityError> {
        let revision = schedule_revision.to_string();
        let scheduled_for = unix_nanos(scheduled_for)?.to_string();
        Ok(ScheduleTriggerId(named(
            "scheduled-trigger",
            &[
                scope.as_ref(),
                schedule_id.as_ref(),
                &revision,
                &scheduled_for,
            ],
        )))
    }

    pub fn scheduled_execution(
        &self,
        scope: &ExecutionScopeId,
        schedule_id: &ScheduleId,
        schedule_revision: u64,
        scheduled_for: SystemTime,
    ) -> Result<ExecutionId, IdentityError> {
        let revision = schedule_revision.to_string();
        let scheduled_for = unix_nanos(scheduled_for)?.to_string();
        Ok(ExecutionId::from(named(
            "scheduled-execution",
            &[
                scope.as_ref(),
                schedule_id.as_ref(),
                &revision,
                &scheduled_for,
            ],
        )))
    }
}

fn named(domain: &str, parts: &[&str]) -> String {
    let mut bytes = canonical(&[domain]);
    bytes.extend(canonical(parts));
    Uuid::new_v5(&IDENTITY_NAMESPACE, &bytes).to_string()
}

fn canonical(parts: &[&str]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for part in parts {
        bytes.extend_from_slice(&(part.len() as u64).to_be_bytes());
        bytes.extend_from_slice(part.as_bytes());
    }
    bytes
}

fn unix_nanos(time: SystemTime) -> Result<u128, IdentityError> {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .map_err(|_| IdentityError::TimeBeforeEpoch)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn scheduled_identities_are_stable_and_scoped() {
        let factory = ExecutionIdentityFactory;
        let first_scope = ExecutionScopeId::new("tenant-a").unwrap();
        let other_scope = ExecutionScopeId::new("tenant-b").unwrap();
        let first_schedule = factory.schedule_id(&first_scope, "restock");
        let repeated_schedule = factory.schedule_id(&first_scope, "restock");
        let other_schedule = factory.schedule_id(&other_scope, "restock");
        let scheduled_for = UNIX_EPOCH + Duration::from_secs(100);

        assert_eq!(first_schedule, repeated_schedule);
        assert_ne!(first_schedule, other_schedule);
        assert_eq!(
            factory
                .scheduled_execution(&first_scope, &first_schedule, 2, scheduled_for)
                .unwrap(),
            factory
                .scheduled_execution(&first_scope, &first_schedule, 2, scheduled_for)
                .unwrap()
        );
        assert_ne!(
            factory
                .scheduled_execution(&first_scope, &first_schedule, 2, scheduled_for)
                .unwrap(),
            factory
                .scheduled_execution(&first_scope, &first_schedule, 3, scheduled_for)
                .unwrap()
        );
    }
}
