use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: &str = "ryvus.invoke.v3";

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExecutionId(String);

impl ExecutionId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }
}

impl Default for ExecutionId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ExecutionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for ExecutionId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<String> for ExecutionId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for ExecutionId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AttemptId(String);

impl AttemptId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }
}

impl Default for AttemptId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for AttemptId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for AttemptId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<String> for AttemptId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for AttemptId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExecutionAttempt {
    pub execution_id: ExecutionId,
    pub attempt_id: AttemptId,
    pub attempt_number: u32,
}

impl ExecutionAttempt {
    pub fn initial() -> Self {
        Self {
            execution_id: ExecutionId::new(),
            attempt_id: AttemptId::new(),
            attempt_number: 1,
        }
    }

    pub fn retry(&self) -> Self {
        Self {
            execution_id: self.execution_id.clone(),
            attempt_id: AttemptId::new(),
            attempt_number: self
                .attempt_number
                .checked_add(1)
                .expect("attempt number should not overflow"),
        }
    }
}

impl std::fmt::Display for ExecutionAttempt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "execution_id={}, attempt_id={}, attempt_number={}",
            self.execution_id, self.attempt_id, self.attempt_number
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvocationRequest {
    pub protocol_version: String,
    pub execution_id: ExecutionId,
    pub attempt_id: AttemptId,
    pub attempt_number: u32,
    pub deadline_unix_ms: i64,
    pub remaining_budget_ms: u64,
    pub event: Value,
    pub context: InvocationContext,
}

impl InvocationRequest {
    pub fn new(event: Value) -> Self {
        Self::with_attempt(
            event,
            InvocationContext::default(),
            ExecutionAttempt::initial(),
        )
    }

    pub fn with_attempt(
        event: Value,
        context: InvocationContext,
        attempt: ExecutionAttempt,
    ) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION.to_string(),
            execution_id: attempt.execution_id,
            attempt_id: attempt.attempt_id,
            attempt_number: attempt.attempt_number,
            deadline_unix_ms: 0,
            remaining_budget_ms: 0,
            event,
            context,
        }
    }

    pub fn attempt(&self) -> ExecutionAttempt {
        ExecutionAttempt {
            execution_id: self.execution_id.clone(),
            attempt_id: self.attempt_id.clone(),
            attempt_number: self.attempt_number,
        }
    }

    pub fn retry(&self) -> Self {
        Self::with_attempt(
            self.event.clone(),
            self.context.clone(),
            self.attempt().retry(),
        )
    }

    pub fn set_deadline(&mut self, deadline_unix_ms: i64, remaining_budget_ms: u64) {
        self.deadline_unix_ms = deadline_unix_ms;
        self.remaining_budget_ms = remaining_budget_ms;
    }

    pub fn refresh_remaining_budget(&mut self, now_unix_ms: i64) {
        self.remaining_budget_ms = self
            .remaining_budget_ms
            .min(self.deadline_unix_ms.saturating_sub(now_unix_ms).max(0) as u64);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InvocationContext {
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvocationResult {
    pub protocol_version: String,
    pub execution_id: ExecutionId,
    pub attempt_id: AttemptId,
    pub attempt_number: u32,
    pub status: InvocationStatus,
    pub output: Option<Value>,
    pub error: Option<InvocationError>,
}

impl InvocationResult {
    pub fn success(request: &InvocationRequest, output: Value) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION.to_string(),
            execution_id: request.execution_id.clone(),
            attempt_id: request.attempt_id.clone(),
            attempt_number: request.attempt_number,
            status: InvocationStatus::Success,
            output: Some(output),
            error: None,
        }
    }

    pub fn failed(request: &InvocationRequest, error: InvocationError) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION.to_string(),
            execution_id: request.execution_id.clone(),
            attempt_id: request.attempt_id.clone(),
            attempt_number: request.attempt_number,
            status: InvocationStatus::Failed,
            output: None,
            error: Some(error),
        }
    }

    pub fn attempt(&self) -> ExecutionAttempt {
        ExecutionAttempt {
            execution_id: self.execution_id.clone(),
            attempt_id: self.attempt_id.clone(),
            attempt_number: self.attempt_number,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InvocationStatus {
    Success,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvocationError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub details: Value,
}

impl InvocationError {
    pub fn new(code: impl Into<String>, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable,
            details: Value::Object(Default::default()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InvocationEvent {
    Log(LogEvent),
    Metric(MetricEvent),
}

impl InvocationEvent {
    pub fn attempt(&self) -> ExecutionAttempt {
        match self {
            Self::Log(event) => ExecutionAttempt {
                execution_id: event.execution_id.clone(),
                attempt_id: event.attempt_id.clone(),
                attempt_number: event.attempt_number,
            },
            Self::Metric(event) => ExecutionAttempt {
                execution_id: event.execution_id.clone(),
                attempt_id: event.attempt_id.clone(),
                attempt_number: event.attempt_number,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEvent {
    pub execution_id: ExecutionId,
    pub attempt_id: AttemptId,
    pub attempt_number: u32,
    pub level: LogLevel,
    pub message: String,
    pub fields: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricEvent {
    pub execution_id: ExecutionId,
    pub attempt_id: AttemptId,
    pub attempt_number: u32,
    pub name: String,
    pub value: f64,
    pub unit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InvocationMessage {
    Event { event: InvocationEvent },
    Result { result: InvocationResult },
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn initial_request_has_attempt_number_one() {
        let request = InvocationRequest::new(json!({}));

        assert_eq!(request.attempt_number, 1);
        assert!(!request.execution_id.as_ref().is_empty());
        assert!(!request.attempt_id.as_ref().is_empty());
    }

    #[test]
    fn retry_preserves_execution_and_replaces_attempt_identity() {
        let first = InvocationRequest::new(json!({ "message": "hello" }));
        let second = first.retry();

        assert_eq!(second.execution_id, first.execution_id);
        assert_ne!(second.attempt_id, first.attempt_id);
        assert_eq!(second.attempt_number, 2);
        assert_eq!(second.event, first.event);
    }

    #[test]
    fn protocol_serializes_unambiguous_attempt_identity() {
        let request = InvocationRequest::new(json!({}));
        let value = serde_json::to_value(&request).unwrap();

        assert_eq!(value["protocol_version"], "ryvus.invoke.v3");
        assert_eq!(value["execution_id"], request.execution_id.as_ref());
        assert_eq!(value["attempt_id"], request.attempt_id.as_ref());
        assert_eq!(value["attempt_number"], 1);
        assert_eq!(value["deadline_unix_ms"], 0);
        assert_eq!(value["remaining_budget_ms"], 0);
        assert!(value.get("invocation_id").is_none());
    }

    #[test]
    fn retry_requires_a_fresh_deadline() {
        let mut first = InvocationRequest::new(json!({}));
        first.set_deadline(10_000, 3_000);

        let second = first.retry();

        assert_eq!(second.deadline_unix_ms, 0);
        assert_eq!(second.remaining_budget_ms, 0);
    }

    #[test]
    fn refresh_budget_never_extends_sender_budget() {
        let mut request = InvocationRequest::new(json!({}));
        request.set_deadline(10_000, 3_000);

        request.refresh_remaining_budget(8_000);
        assert_eq!(request.remaining_budget_ms, 2_000);

        request.refresh_remaining_budget(7_000);
        assert_eq!(request.remaining_budget_ms, 2_000);
    }

    #[test]
    fn log_and_metric_events_include_attempt_identity() {
        let request = InvocationRequest::new(json!({}));
        let log = LogEvent {
            execution_id: request.execution_id.clone(),
            attempt_id: request.attempt_id.clone(),
            attempt_number: request.attempt_number,
            level: LogLevel::Info,
            message: "hello".to_string(),
            fields: json!({}),
        };
        let metric = MetricEvent {
            execution_id: request.execution_id.clone(),
            attempt_id: request.attempt_id.clone(),
            attempt_number: request.attempt_number,
            name: "count".to_string(),
            value: 1.0,
            unit: "item".to_string(),
        };

        for value in [
            serde_json::to_value(InvocationEvent::Log(log)).unwrap(),
            serde_json::to_value(InvocationEvent::Metric(metric)).unwrap(),
        ] {
            assert_eq!(value["execution_id"], request.execution_id.as_ref());
            assert_eq!(value["attempt_id"], request.attempt_id.as_ref());
            assert_eq!(value["attempt_number"], 1);
        }
    }
}
