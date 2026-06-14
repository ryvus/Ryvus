use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: &str = "ryvus.invoke.v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvocationRequest {
    pub protocol_version: String,
    pub invocation_id: String,
    pub event: Value,
    pub context: InvocationContext,
}

impl InvocationRequest {
    pub fn new(event: Value) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION.to_string(),
            invocation_id: uuid::Uuid::new_v4().to_string(),
            event,
            context: InvocationContext::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InvocationContext {
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvocationResult {
    pub protocol_version: String,
    pub invocation_id: String,
    pub status: InvocationStatus,
    pub output: Option<Value>,
    pub error: Option<InvocationError>,
    pub metadata: Value,
}

impl InvocationResult {
    pub fn success(invocation_id: impl Into<String>, output: Value) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION.to_string(),
            invocation_id: invocation_id.into(),
            status: InvocationStatus::Success,
            output: Some(output),
            error: None,
            metadata: Value::Object(Default::default()),
        }
    }

    pub fn failed(invocation_id: impl Into<String>, error: InvocationError) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION.to_string(),
            invocation_id: invocation_id.into(),
            status: InvocationStatus::Failed,
            output: None,
            error: Some(error),
            metadata: Value::Object(Default::default()),
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEvent {
    pub invocation_id: String,
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
    pub invocation_id: String,
    pub name: String,
    pub value: f64,
    pub unit: String,
}
