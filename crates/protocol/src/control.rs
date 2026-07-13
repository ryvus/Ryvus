use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{AttemptId, ExecutionId};

pub const RUNTIME_CONTROL_PROTOCOL_VERSION: &str = "ryvus.runtime-control.v1";

macro_rules! control_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new() -> Self {
                Self(uuid::Uuid::new_v4().to_string())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_string())
            }
        }
    };
}

control_id!(RuntimeHostId);
control_id!(RuntimeSessionId);
control_id!(WorkerId);
control_id!(ControlMessageId);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RuntimeCapabilities {
    pub terminate_attempt: bool,
    pub drain: bool,
    pub shutdown: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveAttemptOwnership {
    pub execution_id: ExecutionId,
    pub attempt_id: AttemptId,
    pub attempt_number: u32,
    pub worker_id: WorkerId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeRegistration {
    pub protocol_version: String,
    pub message_id: ControlMessageId,
    pub runtime_host_id: RuntimeHostId,
    pub runtime_session_id: RuntimeSessionId,
    pub revision: String,
    pub max_concurrency: u32,
    pub capabilities: RuntimeCapabilities,
    pub active_attempts: Vec<ActiveAttemptOwnership>,
}

impl RuntimeRegistration {
    pub fn validate(&self) -> Result<(), RuntimeControlValidationError> {
        validate_common(
            &self.protocol_version,
            &self.message_id,
            &self.runtime_host_id,
            &self.runtime_session_id,
        )?;
        validate_non_empty("revision", &self.revision)?;
        if self.max_concurrency == 0 {
            return Err(RuntimeControlValidationError::InvalidMaxConcurrency);
        }

        let mut attempts = HashSet::new();
        let mut workers = HashSet::new();
        for ownership in &self.active_attempts {
            validate_attempt(
                &ownership.execution_id,
                &ownership.attempt_id,
                ownership.attempt_number,
            )?;
            validate_non_empty("worker_id", ownership.worker_id.as_ref())?;
            if !attempts.insert(ownership.attempt_id.clone()) {
                return Err(RuntimeControlValidationError::DuplicateAttempt {
                    attempt_id: ownership.attempt_id.clone(),
                });
            }
            if !workers.insert(ownership.worker_id.clone()) {
                return Err(RuntimeControlValidationError::DuplicateWorker {
                    worker_id: ownership.worker_id.clone(),
                });
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminationReason {
    Cancellation,
    Timeout,
    Drain,
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOutcome {
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
    InfrastructureFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlCommandOutcome {
    Confirmed,
    AlreadyTerminal,
    AttemptNotFound,
    OwnershipMismatch,
    StaleSession,
    Unsupported,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuntimeControlCommand {
    TerminateAttempt {
        protocol_version: String,
        message_id: ControlMessageId,
        runtime_host_id: RuntimeHostId,
        runtime_session_id: RuntimeSessionId,
        execution_id: ExecutionId,
        attempt_id: AttemptId,
        attempt_number: u32,
        reason: TerminationReason,
    },
    DrainRuntime {
        protocol_version: String,
        message_id: ControlMessageId,
        runtime_host_id: RuntimeHostId,
        runtime_session_id: RuntimeSessionId,
    },
    ShutdownRuntime {
        protocol_version: String,
        message_id: ControlMessageId,
        runtime_host_id: RuntimeHostId,
        runtime_session_id: RuntimeSessionId,
    },
}

impl RuntimeControlCommand {
    pub fn validate(&self) -> Result<(), RuntimeControlValidationError> {
        match self {
            Self::TerminateAttempt {
                protocol_version,
                message_id,
                runtime_host_id,
                runtime_session_id,
                execution_id,
                attempt_id,
                attempt_number,
                ..
            } => {
                validate_common(
                    protocol_version,
                    message_id,
                    runtime_host_id,
                    runtime_session_id,
                )?;
                validate_attempt(execution_id, attempt_id, *attempt_number)
            }
            Self::DrainRuntime {
                protocol_version,
                message_id,
                runtime_host_id,
                runtime_session_id,
            }
            | Self::ShutdownRuntime {
                protocol_version,
                message_id,
                runtime_host_id,
                runtime_session_id,
            } => validate_common(
                protocol_version,
                message_id,
                runtime_host_id,
                runtime_session_id,
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuntimeControlEvent {
    Registered {
        protocol_version: String,
        message_id: ControlMessageId,
        runtime_host_id: RuntimeHostId,
        runtime_session_id: RuntimeSessionId,
    },
    AttemptStarted {
        protocol_version: String,
        message_id: ControlMessageId,
        runtime_host_id: RuntimeHostId,
        runtime_session_id: RuntimeSessionId,
        execution_id: ExecutionId,
        attempt_id: AttemptId,
        attempt_number: u32,
        worker_id: WorkerId,
    },
    AttemptFinished {
        protocol_version: String,
        message_id: ControlMessageId,
        runtime_host_id: RuntimeHostId,
        runtime_session_id: RuntimeSessionId,
        execution_id: ExecutionId,
        attempt_id: AttemptId,
        attempt_number: u32,
        worker_id: WorkerId,
        outcome: AttemptOutcome,
    },
    CommandResult {
        protocol_version: String,
        message_id: ControlMessageId,
        runtime_host_id: RuntimeHostId,
        runtime_session_id: RuntimeSessionId,
        command_message_id: ControlMessageId,
        outcome: ControlCommandOutcome,
        message: Option<String>,
    },
    Heartbeat {
        protocol_version: String,
        message_id: ControlMessageId,
        runtime_host_id: RuntimeHostId,
        runtime_session_id: RuntimeSessionId,
    },
}

impl RuntimeControlEvent {
    pub fn validate(&self) -> Result<(), RuntimeControlValidationError> {
        match self {
            Self::AttemptStarted {
                protocol_version,
                message_id,
                runtime_host_id,
                runtime_session_id,
                execution_id,
                attempt_id,
                attempt_number,
                worker_id,
            }
            | Self::AttemptFinished {
                protocol_version,
                message_id,
                runtime_host_id,
                runtime_session_id,
                execution_id,
                attempt_id,
                attempt_number,
                worker_id,
                ..
            } => {
                validate_common(
                    protocol_version,
                    message_id,
                    runtime_host_id,
                    runtime_session_id,
                )?;
                validate_attempt(execution_id, attempt_id, *attempt_number)?;
                validate_non_empty("worker_id", worker_id.as_ref())
            }
            Self::CommandResult {
                protocol_version,
                message_id,
                runtime_host_id,
                runtime_session_id,
                command_message_id,
                ..
            } => {
                validate_common(
                    protocol_version,
                    message_id,
                    runtime_host_id,
                    runtime_session_id,
                )?;
                validate_non_empty("command_message_id", command_message_id.as_ref())
            }
            Self::Registered {
                protocol_version,
                message_id,
                runtime_host_id,
                runtime_session_id,
            }
            | Self::Heartbeat {
                protocol_version,
                message_id,
                runtime_host_id,
                runtime_session_id,
            } => validate_common(
                protocol_version,
                message_id,
                runtime_host_id,
                runtime_session_id,
            ),
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RuntimeControlValidationError {
    #[error("unsupported runtime-control protocol version '{actual}'")]
    UnsupportedProtocolVersion { actual: String },
    #[error("runtime-control field '{field}' must not be empty")]
    EmptyField { field: &'static str },
    #[error("runtime registration max_concurrency must be greater than zero")]
    InvalidMaxConcurrency,
    #[error("attempt number must be greater than zero for attempt '{attempt_id}'")]
    InvalidAttemptNumber { attempt_id: AttemptId },
    #[error("runtime registration contains duplicate attempt '{attempt_id}'")]
    DuplicateAttempt { attempt_id: AttemptId },
    #[error("runtime registration contains duplicate worker '{worker_id}'")]
    DuplicateWorker { worker_id: WorkerId },
}

fn validate_common(
    protocol_version: &str,
    message_id: &ControlMessageId,
    runtime_host_id: &RuntimeHostId,
    runtime_session_id: &RuntimeSessionId,
) -> Result<(), RuntimeControlValidationError> {
    if protocol_version != RUNTIME_CONTROL_PROTOCOL_VERSION {
        return Err(RuntimeControlValidationError::UnsupportedProtocolVersion {
            actual: protocol_version.to_string(),
        });
    }
    validate_non_empty("message_id", message_id.as_ref())?;
    validate_non_empty("runtime_host_id", runtime_host_id.as_ref())?;
    validate_non_empty("runtime_session_id", runtime_session_id.as_ref())
}

fn validate_attempt(
    execution_id: &ExecutionId,
    attempt_id: &AttemptId,
    attempt_number: u32,
) -> Result<(), RuntimeControlValidationError> {
    validate_non_empty("execution_id", execution_id.as_ref())?;
    validate_non_empty("attempt_id", attempt_id.as_ref())?;
    if attempt_number == 0 {
        return Err(RuntimeControlValidationError::InvalidAttemptNumber {
            attempt_id: attempt_id.clone(),
        });
    }
    Ok(())
}

fn validate_non_empty(
    field: &'static str,
    value: &str,
) -> Result<(), RuntimeControlValidationError> {
    if value.trim().is_empty() {
        Err(RuntimeControlValidationError::EmptyField { field })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn registration_serializes_active_attempt_ownership() {
        let registration = registration();

        assert_eq!(
            serde_json::to_value(&registration).unwrap(),
            json!({
                "protocol_version": "ryvus.runtime-control.v1",
                "message_id": "message-1",
                "runtime_host_id": "host-1",
                "runtime_session_id": "session-1",
                "revision": "revision-a",
                "max_concurrency": 2,
                "capabilities": {
                    "terminate_attempt": true,
                    "drain": true,
                    "shutdown": true
                },
                "active_attempts": [{
                    "execution_id": "execution-1",
                    "attempt_id": "attempt-1",
                    "attempt_number": 2,
                    "worker_id": "worker-1"
                }]
            })
        );
        registration.validate().unwrap();
    }

    #[test]
    fn terminate_command_has_stable_message_and_attempt_identity() {
        let command = terminate_command();

        assert_eq!(
            serde_json::to_value(&command).unwrap(),
            json!({
                "type": "terminate_attempt",
                "protocol_version": "ryvus.runtime-control.v1",
                "message_id": "command-1",
                "runtime_host_id": "host-1",
                "runtime_session_id": "session-1",
                "execution_id": "execution-1",
                "attempt_id": "attempt-1",
                "attempt_number": 2,
                "reason": "cancellation"
            })
        );
        command.validate().unwrap();
    }

    #[test]
    fn command_result_correlates_semantic_outcome() {
        let event = RuntimeControlEvent::CommandResult {
            protocol_version: version(),
            message_id: ControlMessageId::from("event-1"),
            runtime_host_id: RuntimeHostId::from("host-1"),
            runtime_session_id: RuntimeSessionId::from("session-1"),
            command_message_id: ControlMessageId::from("command-1"),
            outcome: ControlCommandOutcome::OwnershipMismatch,
            message: Some("attempt belongs to another worker".to_string()),
        };

        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({
                "type": "command_result",
                "protocol_version": "ryvus.runtime-control.v1",
                "message_id": "event-1",
                "runtime_host_id": "host-1",
                "runtime_session_id": "session-1",
                "command_message_id": "command-1",
                "outcome": "ownership_mismatch",
                "message": "attempt belongs to another worker"
            })
        );
        event.validate().unwrap();
    }

    #[test]
    fn all_commands_and_events_round_trip() {
        let commands = vec![
            terminate_command(),
            RuntimeControlCommand::DrainRuntime {
                protocol_version: version(),
                message_id: ControlMessageId::from("command-2"),
                runtime_host_id: RuntimeHostId::from("host-1"),
                runtime_session_id: RuntimeSessionId::from("session-1"),
            },
            RuntimeControlCommand::ShutdownRuntime {
                protocol_version: version(),
                message_id: ControlMessageId::from("command-3"),
                runtime_host_id: RuntimeHostId::from("host-1"),
                runtime_session_id: RuntimeSessionId::from("session-1"),
            },
        ];
        for command in commands {
            command.validate().unwrap();
            let encoded = serde_json::to_string(&command).unwrap();
            assert_eq!(
                serde_json::from_str::<RuntimeControlCommand>(&encoded).unwrap(),
                command
            );
        }

        let events = vec![
            RuntimeControlEvent::Registered {
                protocol_version: version(),
                message_id: ControlMessageId::from("event-1"),
                runtime_host_id: RuntimeHostId::from("host-1"),
                runtime_session_id: RuntimeSessionId::from("session-1"),
            },
            RuntimeControlEvent::AttemptStarted {
                protocol_version: version(),
                message_id: ControlMessageId::from("event-2"),
                runtime_host_id: RuntimeHostId::from("host-1"),
                runtime_session_id: RuntimeSessionId::from("session-1"),
                execution_id: ExecutionId::from("execution-1"),
                attempt_id: AttemptId::from("attempt-1"),
                attempt_number: 2,
                worker_id: WorkerId::from("worker-1"),
            },
            RuntimeControlEvent::AttemptFinished {
                protocol_version: version(),
                message_id: ControlMessageId::from("event-3"),
                runtime_host_id: RuntimeHostId::from("host-1"),
                runtime_session_id: RuntimeSessionId::from("session-1"),
                execution_id: ExecutionId::from("execution-1"),
                attempt_id: AttemptId::from("attempt-1"),
                attempt_number: 2,
                worker_id: WorkerId::from("worker-1"),
                outcome: AttemptOutcome::TimedOut,
            },
            RuntimeControlEvent::CommandResult {
                protocol_version: version(),
                message_id: ControlMessageId::from("event-4"),
                runtime_host_id: RuntimeHostId::from("host-1"),
                runtime_session_id: RuntimeSessionId::from("session-1"),
                command_message_id: ControlMessageId::from("command-1"),
                outcome: ControlCommandOutcome::Confirmed,
                message: None,
            },
            RuntimeControlEvent::Heartbeat {
                protocol_version: version(),
                message_id: ControlMessageId::from("event-5"),
                runtime_host_id: RuntimeHostId::from("host-1"),
                runtime_session_id: RuntimeSessionId::from("session-1"),
            },
        ];
        for event in events {
            event.validate().unwrap();
            let encoded = serde_json::to_string(&event).unwrap();
            assert_eq!(
                serde_json::from_str::<RuntimeControlEvent>(&encoded).unwrap(),
                event
            );
        }
    }

    #[test]
    fn reasons_and_outcomes_have_stable_serialization() {
        assert_eq!(
            serde_json::to_value([
                TerminationReason::Cancellation,
                TerminationReason::Timeout,
                TerminationReason::Drain,
                TerminationReason::Shutdown,
            ])
            .unwrap(),
            json!(["cancellation", "timeout", "drain", "shutdown"])
        );
        assert_eq!(
            serde_json::to_value([
                AttemptOutcome::Succeeded,
                AttemptOutcome::Failed,
                AttemptOutcome::Cancelled,
                AttemptOutcome::TimedOut,
                AttemptOutcome::InfrastructureFailed,
            ])
            .unwrap(),
            json!([
                "succeeded",
                "failed",
                "cancelled",
                "timed_out",
                "infrastructure_failed"
            ])
        );
        assert_eq!(
            serde_json::to_value([
                ControlCommandOutcome::Confirmed,
                ControlCommandOutcome::AlreadyTerminal,
                ControlCommandOutcome::AttemptNotFound,
                ControlCommandOutcome::OwnershipMismatch,
                ControlCommandOutcome::StaleSession,
                ControlCommandOutcome::Unsupported,
                ControlCommandOutcome::Failed,
            ])
            .unwrap(),
            json!([
                "confirmed",
                "already_terminal",
                "attempt_not_found",
                "ownership_mismatch",
                "stale_session",
                "unsupported",
                "failed"
            ])
        );
    }

    #[test]
    fn validation_rejects_invalid_registration_shape() {
        let mut value = registration();
        value.protocol_version = "other".to_string();
        assert!(matches!(
            value.validate(),
            Err(RuntimeControlValidationError::UnsupportedProtocolVersion { .. })
        ));

        let mut value = registration();
        value.max_concurrency = 0;
        assert_eq!(
            value.validate(),
            Err(RuntimeControlValidationError::InvalidMaxConcurrency)
        );

        let mut value = registration();
        value.active_attempts[0].attempt_number = 0;
        assert!(matches!(
            value.validate(),
            Err(RuntimeControlValidationError::InvalidAttemptNumber { .. })
        ));
    }

    #[test]
    fn registration_rejects_duplicate_attempts_and_workers() {
        let mut duplicate_attempt = registration();
        duplicate_attempt
            .active_attempts
            .push(ActiveAttemptOwnership {
                execution_id: ExecutionId::from("execution-2"),
                attempt_id: AttemptId::from("attempt-1"),
                attempt_number: 1,
                worker_id: WorkerId::from("worker-2"),
            });
        assert!(matches!(
            duplicate_attempt.validate(),
            Err(RuntimeControlValidationError::DuplicateAttempt { .. })
        ));

        let mut duplicate_worker = registration();
        duplicate_worker
            .active_attempts
            .push(ActiveAttemptOwnership {
                execution_id: ExecutionId::from("execution-2"),
                attempt_id: AttemptId::from("attempt-2"),
                attempt_number: 1,
                worker_id: WorkerId::from("worker-1"),
            });
        assert!(matches!(
            duplicate_worker.validate(),
            Err(RuntimeControlValidationError::DuplicateWorker { .. })
        ));
    }

    #[test]
    fn validation_rejects_empty_identity() {
        let command = RuntimeControlCommand::DrainRuntime {
            protocol_version: version(),
            message_id: ControlMessageId::from(""),
            runtime_host_id: RuntimeHostId::from("host-1"),
            runtime_session_id: RuntimeSessionId::from("session-1"),
        };

        assert_eq!(
            command.validate(),
            Err(RuntimeControlValidationError::EmptyField {
                field: "message_id"
            })
        );
    }

    #[test]
    fn additive_fields_are_compatible_but_unknown_variants_are_not() {
        let mut value = serde_json::to_value(terminate_command()).unwrap();
        value["future_field"] = json!(true);
        assert!(serde_json::from_value::<RuntimeControlCommand>(value).is_ok());

        let unknown = json!({
            "type": "replace_runtime",
            "protocol_version": "ryvus.runtime-control.v1"
        });
        assert!(serde_json::from_value::<RuntimeControlCommand>(unknown).is_err());
    }

    fn registration() -> RuntimeRegistration {
        RuntimeRegistration {
            protocol_version: version(),
            message_id: ControlMessageId::from("message-1"),
            runtime_host_id: RuntimeHostId::from("host-1"),
            runtime_session_id: RuntimeSessionId::from("session-1"),
            revision: "revision-a".to_string(),
            max_concurrency: 2,
            capabilities: RuntimeCapabilities {
                terminate_attempt: true,
                drain: true,
                shutdown: true,
            },
            active_attempts: vec![ActiveAttemptOwnership {
                execution_id: ExecutionId::from("execution-1"),
                attempt_id: AttemptId::from("attempt-1"),
                attempt_number: 2,
                worker_id: WorkerId::from("worker-1"),
            }],
        }
    }

    fn terminate_command() -> RuntimeControlCommand {
        RuntimeControlCommand::TerminateAttempt {
            protocol_version: version(),
            message_id: ControlMessageId::from("command-1"),
            runtime_host_id: RuntimeHostId::from("host-1"),
            runtime_session_id: RuntimeSessionId::from("session-1"),
            execution_id: ExecutionId::from("execution-1"),
            attempt_id: AttemptId::from("attempt-1"),
            attempt_number: 2,
            reason: TerminationReason::Cancellation,
        }
    }

    fn version() -> String {
        RUNTIME_CONTROL_PROTOCOL_VERSION.to_string()
    }
}
