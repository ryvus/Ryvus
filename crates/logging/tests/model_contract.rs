use std::collections::BTreeMap;

use ryvus_logging::{
    ExecutionLogCorrelation, ExecutionLogRecord, LogModelError, LogStreamId, RuntimeLogContext,
    SpanId, TraceId,
};
use ryvus_protocol::{
    AttemptId, ExecutionId, ExecutionScopeId, LogLevel, RuntimeHostId, RuntimeKind,
    RuntimeSessionId,
};

fn scope(value: &str) -> ExecutionScopeId {
    ExecutionScopeId::new(value).expect("test scope should be valid")
}

fn record_with_trace(trace_id: Option<TraceId>, span_id: Option<SpanId>) -> ExecutionLogRecord {
    ExecutionLogRecord {
        timestamp_unix_nanos: 1,
        observed_timestamp_unix_nanos: 2,
        stream_sequence: 3,
        stream_id: LogStreamId::new(scope("scope"), RuntimeHostId::from("host")),
        action_key_id: "action".to_string(),
        action_revision: "revision".to_string(),
        runtime_language: RuntimeKind::Python,
        runtime_session_id: None,
        correlation: Some(
            ExecutionLogCorrelation::new(
                ExecutionId::from("execution"),
                AttemptId::from("attempt"),
                1,
            )
            .expect("test correlation should be valid"),
        ),
        severity: LogLevel::Info,
        message: "message".to_string(),
        attributes: BTreeMap::new(),
        trace_id,
        span_id,
    }
}

#[test]
fn stream_identity_includes_execution_scope() {
    assert_ne!(
        LogStreamId::new(scope("a"), RuntimeHostId::from("same")),
        LogStreamId::new(scope("b"), RuntimeHostId::from("same"))
    );
}

#[test]
fn scopes_and_runtime_context_fields_must_not_be_empty() {
    assert!(ExecutionScopeId::new("  ").is_err());
    assert!(matches!(
        RuntimeLogContext::new(scope("scope"), "", "revision", RuntimeKind::Node),
        Err(LogModelError::EmptyField {
            field: "action_key_id"
        })
    ));
    assert!(matches!(
        RuntimeLogContext::new(scope("scope"), "action", " ", RuntimeKind::Node),
        Err(LogModelError::EmptyField {
            field: "action_revision"
        })
    ));
}

#[test]
fn trace_and_span_ids_have_fixed_size_lowercase_hex_values() {
    assert_eq!(TraceId::from([0xab; 16]).to_string(), "ab".repeat(16));
    assert_eq!(SpanId::from([0xcd; 8]).to_string(), "cd".repeat(8));
}

#[test]
fn runtime_session_is_optional_record_metadata() {
    let without_session = record_with_trace(None, None);
    assert!(without_session.validate().is_ok());

    let mut with_session = record_with_trace(None, None);
    with_session.runtime_session_id = Some(RuntimeSessionId::from("session"));
    assert!(with_session.validate().is_ok());
}

#[test]
fn span_requires_trace_and_attempt_number_must_be_non_zero() {
    assert!(matches!(
        record_with_trace(None, Some(SpanId::from([1; 8]))).validate(),
        Err(LogModelError::SpanWithoutTrace)
    ));

    assert!(matches!(
        ExecutionLogCorrelation::new(
            ExecutionId::from("execution"),
            AttemptId::from("attempt"),
            0,
        ),
        Err(LogModelError::InvalidAttemptNumber)
    ));
}

#[test]
fn deserialization_rejects_invalid_validated_models() {
    assert!(
        serde_json::from_value::<RuntimeLogContext>(serde_json::json!({
            "execution_scope": "scope",
            "action_key_id": " ",
            "action_revision": "revision",
            "runtime_language": "Node"
        }))
        .is_err()
    );

    assert!(
        serde_json::from_value::<ExecutionLogCorrelation>(serde_json::json!({
            "execution_id": "execution",
            "attempt_id": "attempt",
            "attempt_number": 0
        }))
        .is_err()
    );
}
