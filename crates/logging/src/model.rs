use std::collections::BTreeMap;

use ryvus_protocol::{
    AttemptId, ExecutionId, ExecutionScopeId, LogLevel, RuntimeHostId, RuntimeKind,
    RuntimeSessionId,
};
use serde::{de::Error as _, Deserialize, Deserializer, Serialize, Serializer};

use crate::LogModelError;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LogStreamId {
    pub execution_scope: ExecutionScopeId,
    pub runtime_host_id: RuntimeHostId,
}

impl LogStreamId {
    pub fn new(execution_scope: ExecutionScopeId, runtime_host_id: RuntimeHostId) -> Self {
        Self {
            execution_scope,
            runtime_host_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuntimeLogContext {
    pub execution_scope: ExecutionScopeId,
    pub action_key_id: String,
    pub action_revision: String,
    pub runtime_language: RuntimeKind,
}

impl<'de> Deserialize<'de> for RuntimeLogContext {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Fields {
            execution_scope: ExecutionScopeId,
            action_key_id: String,
            action_revision: String,
            runtime_language: RuntimeKind,
        }

        let fields = Fields::deserialize(deserializer)?;
        Self::new(
            fields.execution_scope,
            fields.action_key_id,
            fields.action_revision,
            fields.runtime_language,
        )
        .map_err(D::Error::custom)
    }
}

impl RuntimeLogContext {
    pub fn new(
        execution_scope: ExecutionScopeId,
        action_key_id: impl Into<String>,
        action_revision: impl Into<String>,
        runtime_language: RuntimeKind,
    ) -> Result<Self, LogModelError> {
        let context = Self {
            execution_scope,
            action_key_id: action_key_id.into(),
            action_revision: action_revision.into(),
            runtime_language,
        };
        validate_non_empty("action_key_id", &context.action_key_id)?;
        validate_non_empty("action_revision", &context.action_revision)?;
        Ok(context)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum AttributeValue {
    String(String),
    Bool(bool),
    I64(i64),
    F64(f64),
    StringArray(Vec<String>),
    BoolArray(Vec<bool>),
    I64Array(Vec<i64>),
    F64Array(Vec<f64>),
}

impl PartialEq for AttributeValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::String(left), Self::String(right)) => left == right,
            (Self::Bool(left), Self::Bool(right)) => left == right,
            (Self::I64(left), Self::I64(right)) => left == right,
            (Self::F64(left), Self::F64(right)) => left.to_bits() == right.to_bits(),
            (Self::StringArray(left), Self::StringArray(right)) => left == right,
            (Self::BoolArray(left), Self::BoolArray(right)) => left == right,
            (Self::I64Array(left), Self::I64Array(right)) => left == right,
            (Self::F64Array(left), Self::F64Array(right)) => {
                left.len() == right.len()
                    && left
                        .iter()
                        .zip(right)
                        .all(|(left, right)| left.to_bits() == right.to_bits())
            }
            _ => false,
        }
    }
}

impl Eq for AttributeValue {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecutionLogCorrelation {
    pub execution_id: ExecutionId,
    pub attempt_id: AttemptId,
    pub attempt_number: u32,
}

impl<'de> Deserialize<'de> for ExecutionLogCorrelation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Fields {
            execution_id: ExecutionId,
            attempt_id: AttemptId,
            attempt_number: u32,
        }

        let fields = Fields::deserialize(deserializer)?;
        Self::new(
            fields.execution_id,
            fields.attempt_id,
            fields.attempt_number,
        )
        .map_err(D::Error::custom)
    }
}

impl ExecutionLogCorrelation {
    pub fn new(
        execution_id: ExecutionId,
        attempt_id: AttemptId,
        attempt_number: u32,
    ) -> Result<Self, LogModelError> {
        if attempt_number == 0 {
            return Err(LogModelError::InvalidAttemptNumber);
        }
        Ok(Self {
            execution_id,
            attempt_id,
            attempt_number,
        })
    }

    fn validate(&self) -> Result<(), LogModelError> {
        if self.attempt_number == 0 {
            return Err(LogModelError::InvalidAttemptNumber);
        }
        Ok(())
    }
}

macro_rules! hex_id {
    ($name:ident, $size:literal, $kind:literal) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub struct $name([u8; $size]);

        impl From<[u8; $size]> for $name {
            fn from(value: [u8; $size]) -> Self {
                Self(value)
            }
        }

        impl $name {
            pub fn as_bytes(&self) -> &[u8; $size] {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                for byte in self.0 {
                    write!(formatter, "{byte:02x}")?;
                }
                Ok(())
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.to_string())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                decode_hex::<$size>(&value, $kind)
                    .map(Self)
                    .map_err(D::Error::custom)
            }
        }
    };
}

hex_id!(TraceId, 16, "trace id");
hex_id!(SpanId, 8, "span id");

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionLogRecord {
    pub timestamp_unix_nanos: i64,
    pub observed_timestamp_unix_nanos: i64,
    pub stream_sequence: u64,
    pub stream_id: LogStreamId,
    pub action_key_id: String,
    pub action_revision: String,
    pub runtime_language: RuntimeKind,
    pub runtime_session_id: Option<RuntimeSessionId>,
    pub correlation: Option<ExecutionLogCorrelation>,
    pub severity: LogLevel,
    pub message: String,
    pub attributes: BTreeMap<String, AttributeValue>,
    pub trace_id: Option<TraceId>,
    pub span_id: Option<SpanId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLossCause {
    IngestionOverflow,
    ProviderFailure,
    RetentionEviction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogLossRange {
    pub first_sequence: u64,
    pub last_sequence: u64,
    pub cause: LogLossCause,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogStreamTransition {
    Active,
    Complete,
    Incomplete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogStreamMetadata {
    pub stream_id: LogStreamId,
    pub action_key_id: String,
    pub action_revision: String,
    pub runtime_language: RuntimeKind,
    pub started_at_unix_nanos: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogBatch {
    pub stream: LogStreamMetadata,
    pub batch_id: String,
    pub records: Vec<ExecutionLogRecord>,
    pub loss_ranges: Vec<LogLossRange>,
    pub transition: Option<LogStreamTransition>,
}

impl ExecutionLogRecord {
    pub fn validate(&self) -> Result<(), LogModelError> {
        validate_non_empty("runtime_host_id", self.stream_id.runtime_host_id.as_ref())?;
        validate_non_empty("action_key_id", &self.action_key_id)?;
        validate_non_empty("action_revision", &self.action_revision)?;
        if let Some(correlation) = &self.correlation {
            correlation.validate()?;
        }
        if self.trace_id.is_none() && self.span_id.is_some() {
            return Err(LogModelError::SpanWithoutTrace);
        }
        Ok(())
    }
}

fn validate_non_empty(field: &'static str, value: &str) -> Result<(), LogModelError> {
    if value.trim().is_empty() {
        return Err(LogModelError::EmptyField { field });
    }
    Ok(())
}

fn decode_hex<const N: usize>(value: &str, kind: &'static str) -> Result<[u8; N], LogModelError> {
    if value.len() != N * 2 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(LogModelError::InvalidHexId {
            kind,
            expected: N * 2,
        });
    }

    let mut bytes = [0; N];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let pair = std::str::from_utf8(pair).map_err(|_| LogModelError::InvalidHexId {
            kind,
            expected: N * 2,
        })?;
        bytes[index] = u8::from_str_radix(pair, 16).map_err(|_| LogModelError::InvalidHexId {
            kind,
            expected: N * 2,
        })?;
    }
    Ok(bytes)
}
