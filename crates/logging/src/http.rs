use std::{collections::BTreeMap, sync::Arc};

use axum::{
    extract::{rejection::QueryRejection, Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use ryvus_protocol::{
    AttemptId, ExecutionId, ExecutionScopeId, LogLevel, RuntimeHostId, RuntimeKind,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::json;

use crate::{
    AttributeValue, ExecutionLogCorrelation, ExecutionLogRecord, ExecutionLogStore, LogLossCause,
    LogRecordQuery, LogStoreError, LogStreamCompleteness, LogStreamCursor, LogStreamId,
    LogStreamQuery, LogStreamSummary, MAX_QUERY_LIMIT,
};

const DEFAULT_LIMIT: usize = 100;
const MAX_CURSOR_BYTES: usize = 4_096;
const MAX_SEARCH_BYTES: usize = 256;

#[derive(Clone)]
struct LogHistoryState {
    store: Arc<dyn ExecutionLogStore>,
    scope: ExecutionScopeId,
}

#[derive(Debug, Deserialize)]
struct StreamQueryParams {
    action_key_id: Option<String>,
    action_revision: Option<String>,
    runtime_host_id: Option<String>,
    execution_id: Option<String>,
    attempt_id: Option<String>,
    severity: Option<LogLevel>,
    search: Option<String>,
    cursor: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct RecordQueryParams {
    execution_id: Option<String>,
    attempt_id: Option<String>,
    severity: Option<LogLevel>,
    search: Option<String>,
    cursor: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize)]
struct VersionedCursor<T> {
    version: u8,
    cursor: T,
}

#[derive(Debug, Serialize, Deserialize)]
struct StreamCursorV1 {
    started_at_unix_nanos: i64,
    stream_id: LogStreamId,
}

#[derive(Debug, Serialize, Deserialize)]
struct RecordCursorV1 {
    stream_id: LogStreamId,
    sequence: u64,
}

#[derive(Debug, Serialize)]
struct StreamPage {
    streams: Vec<LogStreamSummaryDto>,
    next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
struct RecordPage {
    records: Vec<LogRecordDto>,
    next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
struct LogLossRangeDto {
    first_sequence: String,
    last_sequence: String,
    cause: LogLossCause,
}

#[derive(Debug, Serialize)]
struct LogStreamSummaryDto {
    runtime_host_id: String,
    action_key_id: String,
    action_revision: String,
    runtime_language: RuntimeKind,
    started_at_unix_nanos: String,
    first_sequence: Option<String>,
    last_sequence: Option<String>,
    persisted_record_count: String,
    ingestion_dropped_count: String,
    provider_dropped_count: String,
    evicted_record_count: String,
    loss_ranges: Vec<LogLossRangeDto>,
    ended_at_unix_nanos: Option<String>,
    completeness: LogStreamCompleteness,
    evicted: bool,
    evicted_from: Option<LogStreamCompleteness>,
}

#[derive(Debug, Serialize)]
struct LogCorrelationDto {
    execution_id: String,
    attempt_id: String,
    attempt_number: u32,
}

#[derive(Debug, Serialize)]
struct LogRecordDto {
    timestamp_unix_nanos: String,
    observed_timestamp_unix_nanos: String,
    stream_sequence: String,
    runtime_host_id: String,
    action_key_id: String,
    action_revision: String,
    runtime_language: RuntimeKind,
    runtime_session_id: Option<String>,
    correlation: Option<LogCorrelationDto>,
    severity: LogLevel,
    message: String,
    attributes: BTreeMap<String, LogAttributeValueDto>,
    trace_id: Option<String>,
    span_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
enum LogAttributeValueDto {
    String(String),
    Bool(bool),
    I64(String),
    F64(f64),
    StringArray(Vec<String>),
    BoolArray(Vec<bool>),
    I64Array(Vec<String>),
    F64Array(Vec<f64>),
}

impl From<AttributeValue> for LogAttributeValueDto {
    fn from(value: AttributeValue) -> Self {
        match value {
            AttributeValue::String(value) => Self::String(value),
            AttributeValue::Bool(value) => Self::Bool(value),
            AttributeValue::I64(value) => Self::I64(value.to_string()),
            AttributeValue::F64(value) => Self::F64(value),
            AttributeValue::StringArray(value) => Self::StringArray(value),
            AttributeValue::BoolArray(value) => Self::BoolArray(value),
            AttributeValue::I64Array(value) => {
                Self::I64Array(value.into_iter().map(|item| item.to_string()).collect())
            }
            AttributeValue::F64Array(value) => Self::F64Array(value),
        }
    }
}

impl From<LogStreamSummary> for LogStreamSummaryDto {
    fn from(summary: LogStreamSummary) -> Self {
        Self {
            runtime_host_id: summary.stream.stream_id.runtime_host_id.to_string(),
            action_key_id: summary.stream.action_key_id,
            action_revision: summary.stream.action_revision,
            runtime_language: summary.stream.runtime_language,
            started_at_unix_nanos: summary.stream.started_at_unix_nanos.to_string(),
            first_sequence: summary.first_sequence.map(|value| value.to_string()),
            last_sequence: summary.last_sequence.map(|value| value.to_string()),
            persisted_record_count: summary.persisted_record_count.to_string(),
            ingestion_dropped_count: summary.ingestion_dropped_count.to_string(),
            provider_dropped_count: summary.provider_dropped_count.to_string(),
            evicted_record_count: summary.evicted_record_count.to_string(),
            loss_ranges: summary
                .loss_ranges
                .into_iter()
                .map(|range| LogLossRangeDto {
                    first_sequence: range.first_sequence.to_string(),
                    last_sequence: range.last_sequence.to_string(),
                    cause: range.cause,
                })
                .collect(),
            ended_at_unix_nanos: summary.ended_at_unix_nanos.map(|value| value.to_string()),
            completeness: summary.completeness,
            evicted: summary.evicted,
            evicted_from: summary.evicted_from,
        }
    }
}

impl From<ExecutionLogRecord> for LogRecordDto {
    fn from(record: ExecutionLogRecord) -> Self {
        Self {
            timestamp_unix_nanos: record.timestamp_unix_nanos.to_string(),
            observed_timestamp_unix_nanos: record.observed_timestamp_unix_nanos.to_string(),
            stream_sequence: record.stream_sequence.to_string(),
            runtime_host_id: record.stream_id.runtime_host_id.to_string(),
            action_key_id: record.action_key_id,
            action_revision: record.action_revision,
            runtime_language: record.runtime_language,
            runtime_session_id: record.runtime_session_id.map(|value| value.to_string()),
            correlation: record.correlation.map(LogCorrelationDto::from),
            severity: record.severity,
            message: record.message,
            attributes: record
                .attributes
                .into_iter()
                .map(|(key, value)| (key, value.into()))
                .collect(),
            trace_id: record.trace_id.map(|value| value.to_string()),
            span_id: record.span_id.map(|value| value.to_string()),
        }
    }
}

impl From<ExecutionLogCorrelation> for LogCorrelationDto {
    fn from(correlation: ExecutionLogCorrelation) -> Self {
        Self {
            execution_id: correlation.execution_id.to_string(),
            attempt_id: correlation.attempt_id.to_string(),
            attempt_number: correlation.attempt_number,
        }
    }
}

pub fn log_history_routes(store: Arc<dyn ExecutionLogStore>, scope: ExecutionScopeId) -> Router {
    Router::new()
        .route("/internal/logs/streams", get(list_streams))
        .route(
            "/internal/logs/streams/{runtime_host_id}/records",
            get(list_records),
        )
        .with_state(LogHistoryState { store, scope })
}

async fn list_streams(
    State(state): State<LogHistoryState>,
    query: Result<Query<StreamQueryParams>, QueryRejection>,
) -> Result<Json<StreamPage>, LogHttpError> {
    let Query(query) = query.map_err(|_| LogHttpError::InvalidQuery)?;
    validate_limit(query.limit)?;
    validate_optional_values([
        query.action_key_id.as_deref(),
        query.action_revision.as_deref(),
        query.runtime_host_id.as_deref(),
        query.execution_id.as_deref(),
        query.attempt_id.as_deref(),
    ])?;
    let message_contains = normalize_search(query.search)?;
    let cursor = query
        .cursor
        .as_deref()
        .map(decode_cursor::<StreamCursorV1>)
        .transpose()?;
    if cursor
        .as_ref()
        .is_some_and(|cursor| cursor.stream_id.execution_scope != state.scope)
    {
        return Err(LogHttpError::InvalidCursor);
    }
    let had_cursor = cursor.is_some();
    let page = state
        .store
        .list_streams(LogStreamQuery {
            execution_scope: state.scope,
            action_key_id: query.action_key_id,
            action_revision: query.action_revision,
            runtime_host_id: query.runtime_host_id.map(RuntimeHostId::from),
            execution_id: query.execution_id.map(ExecutionId::from),
            attempt_id: query.attempt_id.map(AttemptId::from),
            severity: query.severity,
            message_contains,
            cursor: cursor.map(|cursor| LogStreamCursor {
                stream_id: cursor.stream_id,
                started_at_unix_nanos: cursor.started_at_unix_nanos,
            }),
            limit: query.limit.unwrap_or(DEFAULT_LIMIT),
        })
        .map_err(|error| LogHttpError::from_store(error, had_cursor))?;
    let next_cursor = page
        .next_cursor
        .map(|cursor| {
            encode_cursor(StreamCursorV1 {
                started_at_unix_nanos: cursor.started_at_unix_nanos,
                stream_id: cursor.stream_id,
            })
        })
        .transpose()?;
    Ok(Json(StreamPage {
        streams: page.streams.into_iter().map(Into::into).collect(),
        next_cursor,
    }))
}

async fn list_records(
    State(state): State<LogHistoryState>,
    Path(runtime_host_id): Path<String>,
    query: Result<Query<RecordQueryParams>, QueryRejection>,
) -> Result<Json<RecordPage>, LogHttpError> {
    let Query(query) = query.map_err(|_| LogHttpError::InvalidQuery)?;
    validate_limit(query.limit)?;
    validate_optional_values([
        Some(runtime_host_id.as_str()),
        query.execution_id.as_deref(),
        query.attempt_id.as_deref(),
    ])?;
    let message_contains = normalize_search(query.search)?;
    let stream_id = LogStreamId::new(state.scope, RuntimeHostId::from(runtime_host_id));
    let cursor = query
        .cursor
        .as_deref()
        .map(decode_cursor::<RecordCursorV1>)
        .transpose()?;
    if cursor
        .as_ref()
        .is_some_and(|cursor| cursor.stream_id != stream_id)
    {
        return Err(LogHttpError::InvalidCursor);
    }
    let page = state
        .store
        .list_records(LogRecordQuery {
            stream_id: stream_id.clone(),
            execution_id: query.execution_id.map(ExecutionId::from),
            attempt_id: query.attempt_id.map(AttemptId::from),
            severity: query.severity,
            message_contains,
            cursor: cursor.map(|cursor| cursor.sequence),
            limit: query.limit.unwrap_or(DEFAULT_LIMIT),
        })
        .map_err(|error| LogHttpError::from_store(error, false))?;
    let next_cursor = page
        .next_cursor
        .map(|sequence| {
            encode_cursor(RecordCursorV1 {
                stream_id,
                sequence,
            })
        })
        .transpose()?;
    Ok(Json(RecordPage {
        records: page.records.into_iter().map(Into::into).collect(),
        next_cursor,
    }))
}

fn validate_limit(limit: Option<usize>) -> Result<(), LogHttpError> {
    if limit.is_some_and(|limit| limit == 0 || limit > MAX_QUERY_LIMIT) {
        return Err(LogHttpError::InvalidQuery);
    }
    Ok(())
}

fn validate_optional_values<const N: usize>(values: [Option<&str>; N]) -> Result<(), LogHttpError> {
    if values
        .into_iter()
        .flatten()
        .any(|value| value.trim().is_empty())
    {
        return Err(LogHttpError::InvalidQuery);
    }
    Ok(())
}

fn normalize_search(search: Option<String>) -> Result<Option<String>, LogHttpError> {
    match search {
        Some(search) if search.trim().is_empty() || search.len() > MAX_SEARCH_BYTES => {
            Err(LogHttpError::InvalidQuery)
        }
        Some(search) => Ok(Some(search.to_lowercase())),
        None => Ok(None),
    }
}

fn encode_cursor<T: Serialize>(cursor: T) -> Result<String, LogHttpError> {
    let bytes = serde_json::to_vec(&VersionedCursor { version: 1, cursor })
        .map_err(|_| LogHttpError::ProviderUnavailable)?;
    Ok(hex_encode(&bytes))
}

fn decode_cursor<T: DeserializeOwned + Serialize>(value: &str) -> Result<T, LogHttpError> {
    if value.is_empty() || value.len() > MAX_CURSOR_BYTES * 2 {
        return Err(LogHttpError::InvalidCursor);
    }
    let bytes = hex_decode(value).ok_or(LogHttpError::InvalidCursor)?;
    let cursor: VersionedCursor<T> =
        serde_json::from_slice(&bytes).map_err(|_| LogHttpError::InvalidCursor)?;
    if cursor.version != 1 {
        return Err(LogHttpError::InvalidCursor);
    }
    let canonical = encode_cursor(&cursor.cursor).map_err(|_| LogHttpError::InvalidCursor)?;
    if canonical != value {
        return Err(LogHttpError::InvalidCursor);
    }
    Ok(cursor.cursor)
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn hex_decode(value: &str) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) || !value.is_ascii() {
        return None;
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = hex_digit(pair[0])?;
            let low = hex_digit(pair[1])?;
            Some((high << 4) | low)
        })
        .collect()
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

enum LogHttpError {
    InvalidQuery,
    InvalidCursor,
    NotFound,
    ProviderUnavailable,
}

impl LogHttpError {
    fn from_store(error: LogStoreError, cursor_supplied: bool) -> Self {
        match error {
            LogStoreError::NotFound => Self::NotFound,
            LogStoreError::InvalidQuery(_) if cursor_supplied => Self::InvalidCursor,
            LogStoreError::InvalidQuery(_) => Self::InvalidQuery,
            _ => Self::ProviderUnavailable,
        }
    }
}

impl IntoResponse for LogHttpError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::InvalidQuery => (
                StatusCode::BAD_REQUEST,
                "log_invalid_query",
                "invalid log query",
            ),
            Self::InvalidCursor => (
                StatusCode::BAD_REQUEST,
                "log_invalid_cursor",
                "invalid log cursor",
            ),
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                "log_stream_not_found",
                "log stream not found",
            ),
            Self::ProviderUnavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "log_provider_unavailable",
                "log provider unavailable",
            ),
        };
        (status, Json(json!({ "error": code, "message": message }))).into_response()
    }
}
