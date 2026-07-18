use ryvus_protocol::{AttemptId, ExecutionId, ExecutionScopeId, LogLevel, RuntimeHostId};
use serde::{Deserialize, Serialize};

use crate::{
    ExecutionLogRecord, LogBatch, LogLossRange, LogStoreError, LogStreamId, LogStreamMetadata,
};

pub const MAX_QUERY_LIMIT: usize = 1_000;

pub trait ExecutionLogStore: Send + Sync {
    fn append_batch(&self, batch: LogBatch) -> Result<(), LogStoreError>;
    fn list_streams(&self, query: LogStreamQuery) -> Result<LogStreamPage, LogStoreError>;
    fn list_records(&self, query: LogRecordQuery) -> Result<LogRecordPage, LogStoreError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogStreamCursor {
    pub stream_id: LogStreamId,
    pub started_at_unix_nanos: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogStreamQuery {
    pub execution_scope: ExecutionScopeId,
    pub action_key_id: Option<String>,
    pub action_revision: Option<String>,
    pub runtime_host_id: Option<RuntimeHostId>,
    pub execution_id: Option<ExecutionId>,
    pub attempt_id: Option<AttemptId>,
    pub severity: Option<LogLevel>,
    pub message_contains: Option<String>,
    pub cursor: Option<LogStreamCursor>,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRecordQuery {
    pub stream_id: LogStreamId,
    pub execution_id: Option<ExecutionId>,
    pub attempt_id: Option<AttemptId>,
    pub severity: Option<LogLevel>,
    pub message_contains: Option<String>,
    pub cursor: Option<u64>,
    pub limit: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogStreamCompleteness {
    Active,
    Complete,
    Incomplete,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogStreamSummary {
    pub stream: LogStreamMetadata,
    pub first_sequence: Option<u64>,
    pub last_sequence: Option<u64>,
    pub persisted_record_count: u64,
    pub ingestion_dropped_count: u64,
    pub provider_dropped_count: u64,
    pub evicted_record_count: u64,
    pub loss_ranges: Vec<LogLossRange>,
    pub ended_at_unix_nanos: Option<i64>,
    pub completeness: LogStreamCompleteness,
    pub evicted: bool,
    pub evicted_from: Option<LogStreamCompleteness>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogStreamPage {
    pub streams: Vec<LogStreamSummary>,
    pub next_cursor: Option<LogStreamCursor>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LogRecordPage {
    pub records: Vec<ExecutionLogRecord>,
    pub next_cursor: Option<u64>,
}

pub(crate) fn validate_query_limit(limit: usize) -> Result<(), LogStoreError> {
    if limit == 0 || limit > MAX_QUERY_LIMIT {
        return Err(LogStoreError::InvalidQuery(format!(
            "limit must be between 1 and {MAX_QUERY_LIMIT}"
        )));
    }
    Ok(())
}

fn correlation_matches(
    record: &ExecutionLogRecord,
    execution_id: Option<&ExecutionId>,
    attempt_id: Option<&AttemptId>,
) -> bool {
    record.correlation.as_ref().is_some_and(|correlation| {
        execution_id.is_none_or(|value| &correlation.execution_id == value)
            && attempt_id.is_none_or(|value| &correlation.attempt_id == value)
    }) || (execution_id.is_none() && attempt_id.is_none())
}

pub(crate) fn record_matches_query(record: &ExecutionLogRecord, query: &LogRecordQuery) -> bool {
    query
        .cursor
        .is_none_or(|cursor| record.stream_sequence > cursor)
        && correlation_matches(
            record,
            query.execution_id.as_ref(),
            query.attempt_id.as_ref(),
        )
        && query
            .severity
            .as_ref()
            .is_none_or(|level| &record.severity == level)
        && query.message_contains.as_ref().is_none_or(|needle| {
            record
                .message
                .to_lowercase()
                .contains(&needle.to_lowercase())
        })
}
