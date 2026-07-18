use std::{
    collections::VecDeque,
    sync::{Mutex, MutexGuard},
};

use crate::{projection::StoreProjection, LogStreamMetadata};
use crate::{
    store::validate_query_limit, ExecutionLogRecord, ExecutionLogStore, LogBatch, LogLossCause,
    LogLossRange, LogRecordPage, LogRecordQuery, LogStoreError, LogStreamCompleteness,
    LogStreamCursor, LogStreamId, LogStreamPage, LogStreamQuery, LogStreamSummary,
    LogStreamTransition,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryLogStoreConfig {
    pub max_streams: usize,
    pub max_records: usize,
    pub max_tombstones: usize,
}

impl Default for MemoryLogStoreConfig {
    fn default() -> Self {
        Self {
            max_streams: 1_000,
            max_records: 10_000,
            max_tombstones: 1_000,
        }
    }
}

#[derive(Clone, Default)]
struct MemoryState {
    projection: StoreProjection,
    tombstones: VecDeque<LogStreamTombstone>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogStreamTombstone {
    pub stream: LogStreamMetadata,
    pub former_completeness: LogStreamCompleteness,
    pub first_sequence: Option<u64>,
    pub last_sequence: Option<u64>,
    pub loss_ranges: Vec<LogLossRange>,
    pub ended_at_unix_nanos: Option<i64>,
}

pub struct InMemoryExecutionLogStore {
    config: MemoryLogStoreConfig,
    state: Mutex<MemoryState>,
}

impl Default for InMemoryExecutionLogStore {
    fn default() -> Self {
        Self {
            config: MemoryLogStoreConfig::default(),
            state: Mutex::new(MemoryState::default()),
        }
    }
}

impl InMemoryExecutionLogStore {
    pub fn new(config: MemoryLogStoreConfig) -> Result<Self, LogStoreError> {
        if config.max_streams == 0 || config.max_records == 0 {
            return Err(LogStoreError::InvalidConfiguration(
                "memory stream and record limits must be greater than zero".to_string(),
            ));
        }
        Ok(Self {
            config,
            state: Mutex::new(MemoryState::default()),
        })
    }

    pub fn tombstone_count(&self) -> Result<usize, LogStoreError> {
        Ok(self.lock_state()?.tombstones.len())
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, MemoryState>, LogStoreError> {
        self.state.lock().map_err(|_| LogStoreError::Unavailable)
    }

    fn enforce_retention(&self, state: &mut MemoryState) -> Result<(), LogStoreError> {
        while state.projection.streams.len() > self.config.max_streams {
            let stream_id = oldest_stream(&state.projection, true)
                .or_else(|| oldest_stream(&state.projection, false))
                .ok_or(LogStoreError::CapacityOverflow)?;
            evict_stream(state, &stream_id, self.config.max_tombstones)?;
        }

        while total_records(&state.projection)? > self.config.max_records {
            if let Some(stream_id) = oldest_terminal_with_records(&state.projection) {
                evict_stream(state, &stream_id, self.config.max_tombstones)?;
                continue;
            }
            let stream_id = oldest_stream_with_records(&state.projection)
                .ok_or(LogStoreError::CapacityOverflow)?;
            let excess = total_records(&state.projection)?
                .checked_sub(self.config.max_records)
                .ok_or(LogStoreError::CapacityOverflow)?;
            let stream = state
                .projection
                .streams
                .get_mut(&stream_id)
                .ok_or(LogStoreError::NotFound)?;
            if stream.remove_oldest_record_run(excess)? == 0 {
                return Err(LogStoreError::CapacityOverflow);
            }
        }
        Ok(())
    }
}

impl ExecutionLogStore for InMemoryExecutionLogStore {
    fn append_batch(&self, batch: LogBatch) -> Result<(), LogStoreError> {
        let mut guard = self.lock_state()?;
        if guard
            .tombstones
            .iter()
            .any(|tombstone| tombstone.stream.stream_id == batch.stream.stream_id)
        {
            return Err(LogStoreError::Conflict(
                "an evicted stream cannot accept later batches".to_string(),
            ));
        }
        let mut candidate = guard.clone();
        candidate.projection.append_batch(batch)?;
        self.enforce_retention(&mut candidate)?;
        *guard = candidate;
        Ok(())
    }

    fn list_streams(&self, query: LogStreamQuery) -> Result<LogStreamPage, LogStoreError> {
        validate_query_limit(query.limit)?;
        if let Some(cursor) = &query.cursor {
            if cursor.stream_id.execution_scope != query.execution_scope {
                return Err(LogStoreError::InvalidQuery(
                    "cursor belongs to another execution scope".to_string(),
                ));
            }
        }
        let state = self.lock_state()?;
        let mut summaries = Vec::new();
        for stream in state.projection.streams.values() {
            if stream_matches_query(stream, &query) {
                summaries.push(stream.summary()?);
            }
        }
        for tombstone in &state.tombstones {
            if tombstone_matches_query(tombstone, &query) {
                summaries.push(tombstone_summary(tombstone)?);
            }
        }
        summaries.sort_by(|left, right| {
            right
                .stream
                .started_at_unix_nanos
                .cmp(&left.stream.started_at_unix_nanos)
                .then_with(|| {
                    right
                        .stream
                        .stream_id
                        .runtime_host_id
                        .as_ref()
                        .cmp(left.stream.stream_id.runtime_host_id.as_ref())
                })
        });

        let start = match &query.cursor {
            Some(cursor) => summaries
                .iter()
                .position(|summary| {
                    summary.stream.stream_id == cursor.stream_id
                        && summary.stream.started_at_unix_nanos == cursor.started_at_unix_nanos
                })
                .map(|index| index + 1)
                .ok_or_else(|| LogStoreError::InvalidQuery("cursor was not found".to_string()))?,
            None => 0,
        };
        let has_more = summaries.len().saturating_sub(start) > query.limit;
        let streams: Vec<_> = summaries
            .into_iter()
            .skip(start)
            .take(query.limit)
            .collect();
        let next_cursor = has_more
            .then(|| {
                streams.last().map(|summary| LogStreamCursor {
                    stream_id: summary.stream.stream_id.clone(),
                    started_at_unix_nanos: summary.stream.started_at_unix_nanos,
                })
            })
            .flatten();
        Ok(LogStreamPage {
            streams,
            next_cursor,
        })
    }

    fn list_records(&self, query: LogRecordQuery) -> Result<LogRecordPage, LogStoreError> {
        validate_query_limit(query.limit)?;
        let state = self.lock_state()?;
        if state
            .tombstones
            .iter()
            .any(|tombstone| tombstone.stream.stream_id == query.stream_id)
        {
            return Ok(LogRecordPage {
                records: Vec::new(),
                next_cursor: None,
            });
        }
        let stream = state
            .projection
            .streams
            .get(&query.stream_id)
            .ok_or(LogStoreError::NotFound)?;
        let mut matching = stream.records.values().filter(|record| {
            query
                .cursor
                .is_none_or(|cursor| record.stream_sequence > cursor)
                && correlation_matches(
                    record,
                    query.execution_id.as_ref(),
                    query.attempt_id.as_ref(),
                )
        });
        let records: Vec<_> = matching.by_ref().take(query.limit).cloned().collect();
        let has_more = matching.next().is_some();
        let next_cursor = has_more
            .then(|| records.last().map(|record| record.stream_sequence))
            .flatten();
        Ok(LogRecordPage {
            records,
            next_cursor,
        })
    }
}

fn stream_matches_query(
    stream: &crate::projection::StreamProjection,
    query: &LogStreamQuery,
) -> bool {
    stream.metadata.stream_id.execution_scope == query.execution_scope
        && query
            .action_key_id
            .as_ref()
            .is_none_or(|value| &stream.metadata.action_key_id == value)
        && query
            .action_revision
            .as_ref()
            .is_none_or(|value| &stream.metadata.action_revision == value)
        && query
            .runtime_host_id
            .as_ref()
            .is_none_or(|value| &stream.metadata.stream_id.runtime_host_id == value)
        && (query.execution_id.is_none() && query.attempt_id.is_none()
            || stream.records.values().any(|record| {
                correlation_matches(
                    record,
                    query.execution_id.as_ref(),
                    query.attempt_id.as_ref(),
                )
            }))
}

fn tombstone_matches_query(tombstone: &LogStreamTombstone, query: &LogStreamQuery) -> bool {
    tombstone.stream.stream_id.execution_scope == query.execution_scope
        && query
            .action_key_id
            .as_ref()
            .is_none_or(|value| &tombstone.stream.action_key_id == value)
        && query
            .action_revision
            .as_ref()
            .is_none_or(|value| &tombstone.stream.action_revision == value)
        && query
            .runtime_host_id
            .as_ref()
            .is_none_or(|value| &tombstone.stream.stream_id.runtime_host_id == value)
        && query.execution_id.is_none()
        && query.attempt_id.is_none()
}

fn correlation_matches(
    record: &ExecutionLogRecord,
    execution_id: Option<&ryvus_protocol::ExecutionId>,
    attempt_id: Option<&ryvus_protocol::AttemptId>,
) -> bool {
    record.correlation.as_ref().is_some_and(|correlation| {
        execution_id.is_none_or(|value| &correlation.execution_id == value)
            && attempt_id.is_none_or(|value| &correlation.attempt_id == value)
    }) || (execution_id.is_none() && attempt_id.is_none())
}

fn total_records(projection: &StoreProjection) -> Result<usize, LogStoreError> {
    projection
        .streams
        .values()
        .try_fold(0_usize, |total, stream| {
            total
                .checked_add(stream.records.len())
                .ok_or(LogStoreError::CapacityOverflow)
        })
}

fn oldest_stream(projection: &StoreProjection, terminal_only: bool) -> Option<LogStreamId> {
    projection
        .streams
        .values()
        .filter(|stream| !terminal_only || stream.transition == Some(LogStreamTransition::Complete))
        .min_by(stream_age_cmp)
        .map(|stream| stream.metadata.stream_id.clone())
}

fn oldest_terminal_with_records(projection: &StoreProjection) -> Option<LogStreamId> {
    projection
        .streams
        .values()
        .filter(|stream| {
            !stream.records.is_empty() && stream.transition == Some(LogStreamTransition::Complete)
        })
        .min_by(stream_age_cmp)
        .map(|stream| stream.metadata.stream_id.clone())
}

fn oldest_stream_with_records(projection: &StoreProjection) -> Option<LogStreamId> {
    projection
        .streams
        .values()
        .filter(|stream| !stream.records.is_empty())
        .min_by(stream_age_cmp)
        .map(|stream| stream.metadata.stream_id.clone())
}

fn stream_age_cmp(
    left: &&crate::projection::StreamProjection,
    right: &&crate::projection::StreamProjection,
) -> std::cmp::Ordering {
    left.metadata
        .started_at_unix_nanos
        .cmp(&right.metadata.started_at_unix_nanos)
        .then_with(|| {
            left.metadata
                .stream_id
                .runtime_host_id
                .as_ref()
                .cmp(right.metadata.stream_id.runtime_host_id.as_ref())
        })
}

fn evict_stream(
    state: &mut MemoryState,
    stream_id: &LogStreamId,
    max_tombstones: usize,
) -> Result<(), LogStoreError> {
    let original_summary = state
        .projection
        .streams
        .get(stream_id)
        .ok_or(LogStoreError::NotFound)?
        .summary()?;
    loop {
        let stream = state
            .projection
            .streams
            .get_mut(stream_id)
            .ok_or(LogStoreError::NotFound)?;
        if stream.records.is_empty() {
            break;
        }
        stream.remove_oldest_record_run(usize::MAX)?;
    }
    let stream = state
        .projection
        .streams
        .remove(stream_id)
        .ok_or(LogStoreError::NotFound)?;
    state.tombstones.push_back(LogStreamTombstone {
        stream: original_summary.stream,
        former_completeness: original_summary.completeness,
        first_sequence: original_summary.first_sequence,
        last_sequence: original_summary.last_sequence,
        loss_ranges: stream.loss_ranges,
        ended_at_unix_nanos: original_summary.ended_at_unix_nanos,
    });
    while state.tombstones.len() > max_tombstones {
        state.tombstones.pop_front();
    }
    Ok(())
}

fn tombstone_summary(tombstone: &LogStreamTombstone) -> Result<LogStreamSummary, LogStoreError> {
    let mut ingestion = 0_u64;
    let mut provider = 0_u64;
    let mut eviction = 0_u64;
    for range in &tombstone.loss_ranges {
        let count = range
            .last_sequence
            .checked_sub(range.first_sequence)
            .and_then(|difference| difference.checked_add(1))
            .ok_or(LogStoreError::CapacityOverflow)?;
        let target = match range.cause {
            LogLossCause::IngestionOverflow => &mut ingestion,
            LogLossCause::ProviderFailure => &mut provider,
            LogLossCause::RetentionEviction => &mut eviction,
        };
        *target = target
            .checked_add(count)
            .ok_or(LogStoreError::CapacityOverflow)?;
    }
    Ok(LogStreamSummary {
        stream: tombstone.stream.clone(),
        first_sequence: tombstone.first_sequence,
        last_sequence: tombstone.last_sequence,
        persisted_record_count: 0,
        ingestion_dropped_count: ingestion,
        provider_dropped_count: provider,
        evicted_record_count: eviction,
        loss_ranges: tombstone.loss_ranges.clone(),
        ended_at_unix_nanos: tombstone.ended_at_unix_nanos,
        completeness: LogStreamCompleteness::Incomplete,
        evicted: true,
        evicted_from: Some(tombstone.former_completeness),
    })
}
