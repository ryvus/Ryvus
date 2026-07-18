use std::collections::{BTreeMap, HashMap};

use sha2::{Digest, Sha256};

use crate::{
    AttributeValue, ExecutionLogRecord, LogBatch, LogLossCause, LogLossRange, LogRecordPage,
    LogRecordQuery, LogStoreError, LogStreamCompleteness, LogStreamCursor, LogStreamId,
    LogStreamMetadata, LogStreamPage, LogStreamQuery, LogStreamSummary, LogStreamTransition,
};
use ryvus_protocol::{LogLevel, RuntimeKind};

pub fn normalize_loss_ranges(
    mut ranges: Vec<LogLossRange>,
) -> Result<Vec<LogLossRange>, LogStoreError> {
    for range in &ranges {
        if range.first_sequence == 0 || range.first_sequence > range.last_sequence {
            return Err(LogStoreError::InvalidBatch(
                "loss ranges must be non-zero and ordered".to_string(),
            ));
        }
    }
    ranges.sort_by_key(|range| (range.first_sequence, range.last_sequence));

    let mut normalized: Vec<LogLossRange> = Vec::with_capacity(ranges.len());
    for range in ranges {
        if let Some(previous) = normalized.last_mut() {
            if range.first_sequence <= previous.last_sequence {
                if range.cause != previous.cause {
                    return Err(LogStoreError::InvalidBatch(
                        "overlapping loss ranges have conflicting causes".to_string(),
                    ));
                }
                previous.last_sequence = previous.last_sequence.max(range.last_sequence);
                continue;
            }
            if previous.cause == range.cause
                && previous.last_sequence.checked_add(1) == Some(range.first_sequence)
            {
                previous.last_sequence = range.last_sequence;
                continue;
            }
        }
        normalized.push(range);
    }
    Ok(normalized)
}

#[derive(Clone, Default)]
pub(crate) struct StoreProjection {
    pub(crate) streams: HashMap<LogStreamId, StreamProjection>,
}

#[derive(Clone)]
pub(crate) struct StreamProjection {
    pub(crate) metadata: LogStreamMetadata,
    pub(crate) records: BTreeMap<u64, ExecutionLogRecord>,
    pub(crate) loss_ranges: Vec<LogLossRange>,
    pub(crate) transition: Option<LogStreamTransition>,
    recovered_nonterminal: bool,
    batches: HashMap<BatchReplayKey, [u8; 32]>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum BatchReplayKey {
    Caller(String),
    Retention { first: u64, last: u64 },
}

impl StoreProjection {
    pub(crate) fn append_batch(&mut self, mut batch: LogBatch) -> Result<(), LogStoreError> {
        validate_batch_shape(&batch)?;
        batch.loss_ranges = normalize_loss_ranges(batch.loss_ranges)?;

        if let Some(stream) = self.streams.get_mut(&batch.stream.stream_id) {
            return stream.append_batch(batch);
        }

        let mut stream = StreamProjection {
            metadata: batch.stream.clone(),
            records: BTreeMap::new(),
            loss_ranges: Vec::new(),
            transition: None,
            recovered_nonterminal: false,
            batches: HashMap::new(),
        };
        stream.append_batch(batch)?;
        self.streams
            .insert(stream.metadata.stream_id.clone(), stream);
        Ok(())
    }

    pub(crate) fn contains_batch(&self, batch: &LogBatch) -> Result<bool, LogStoreError> {
        let Some(stream) = self.streams.get(&batch.stream.stream_id) else {
            return Ok(false);
        };
        let key = BatchReplayKey::Caller(batch.batch_id.clone());
        let Some(committed) = stream.batches.get(&key) else {
            return Ok(false);
        };
        Ok(committed == &batch_fingerprint(batch)?)
    }

    pub(crate) fn list_streams(
        &self,
        query: LogStreamQuery,
    ) -> Result<LogStreamPage, LogStoreError> {
        crate::store::validate_query_limit(query.limit)?;
        if query
            .cursor
            .as_ref()
            .is_some_and(|cursor| cursor.stream_id.execution_scope != query.execution_scope)
        {
            return Err(LogStoreError::InvalidQuery(
                "cursor belongs to another execution scope".to_string(),
            ));
        }

        let mut summaries = self
            .streams
            .values()
            .filter(|stream| stream_matches_query(stream, &query))
            .map(StreamProjection::summary)
            .collect::<Result<Vec<_>, _>>()?;
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
        let streams = summaries
            .into_iter()
            .skip(start)
            .take(query.limit)
            .collect::<Vec<_>>();
        let next_cursor = has_more.then(|| {
            streams.last().map(|summary| LogStreamCursor {
                stream_id: summary.stream.stream_id.clone(),
                started_at_unix_nanos: summary.stream.started_at_unix_nanos,
            })
        });
        Ok(LogStreamPage {
            streams,
            next_cursor: next_cursor.flatten(),
        })
    }

    pub(crate) fn list_records(
        &self,
        query: LogRecordQuery,
    ) -> Result<LogRecordPage, LogStoreError> {
        crate::store::validate_query_limit(query.limit)?;
        let stream = self
            .streams
            .get(&query.stream_id)
            .ok_or(LogStoreError::NotFound)?;
        let mut matching = stream
            .records
            .values()
            .filter(|record| crate::store::record_matches_query(record, &query));
        let records = matching
            .by_ref()
            .take(query.limit)
            .cloned()
            .collect::<Vec<_>>();
        let has_more = matching.next().is_some();
        let next_cursor = has_more.then(|| records.last().map(|record| record.stream_sequence));
        Ok(LogRecordPage {
            records,
            next_cursor: next_cursor.flatten(),
        })
    }
}

fn stream_matches_query(stream: &StreamProjection, query: &LogStreamQuery) -> bool {
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
        && if query.execution_id.is_none()
            && query.attempt_id.is_none()
            && query.severity.is_none()
            && query.message_contains.is_none()
        {
            true
        } else {
            let record_query = LogRecordQuery {
                stream_id: stream.metadata.stream_id.clone(),
                execution_id: query.execution_id.clone(),
                attempt_id: query.attempt_id.clone(),
                severity: query.severity.clone(),
                message_contains: query.message_contains.clone(),
                cursor: None,
                limit: 1,
            };
            stream
                .records
                .values()
                .any(|record| crate::store::record_matches_query(record, &record_query))
        }
}

impl StreamProjection {
    fn append_batch(&mut self, batch: LogBatch) -> Result<(), LogStoreError> {
        let key = BatchReplayKey::Caller(batch.batch_id.clone());
        self.append_batch_with_retention_policy(batch, key, false)
    }

    fn append_batch_with_retention_policy(
        &mut self,
        batch: LogBatch,
        replay_key: BatchReplayKey,
        allow_terminal_retention: bool,
    ) -> Result<(), LogStoreError> {
        let fingerprint = batch_fingerprint(&batch)?;
        if let Some(committed) = self.batches.get(&replay_key) {
            return if committed == &fingerprint {
                Ok(())
            } else {
                Err(LogStoreError::Conflict(format!(
                    "batch {} was already committed with different content",
                    batch.batch_id
                )))
            };
        }
        if !allow_terminal_retention
            && matches!(
                self.transition,
                Some(LogStreamTransition::Complete | LogStreamTransition::Incomplete)
            )
        {
            return Err(LogStoreError::Conflict(
                "terminal streams reject later batches".to_string(),
            ));
        }
        if self.metadata != batch.stream {
            return Err(LogStoreError::Conflict(
                "immutable stream metadata changed".to_string(),
            ));
        }

        for record in &batch.records {
            if self.records.contains_key(&record.stream_sequence)
                || sequence_is_lost(&self.loss_ranges, record.stream_sequence)
            {
                return Err(LogStoreError::Conflict(format!(
                    "sequence {} was already committed",
                    record.stream_sequence
                )));
            }
            if sequence_is_lost(&batch.loss_ranges, record.stream_sequence) {
                return Err(LogStoreError::InvalidBatch(format!(
                    "sequence {} is both a record and a loss",
                    record.stream_sequence
                )));
            }
        }
        for range in &batch.loss_ranges {
            if self
                .records
                .range(range.first_sequence..=range.last_sequence)
                .next()
                .is_some()
            {
                return Err(LogStoreError::Conflict(
                    "loss range overlaps a committed record".to_string(),
                ));
            }
            if self.loss_ranges.iter().any(|committed| {
                committed.first_sequence <= range.last_sequence
                    && range.first_sequence <= committed.last_sequence
            }) {
                return Err(LogStoreError::Conflict(
                    "loss sequence was already committed by another batch".to_string(),
                ));
            }
        }

        let mut combined_loss = self.loss_ranges.clone();
        combined_loss.extend(batch.loss_ranges.iter().cloned());
        combined_loss = normalize_loss_ranges(combined_loss)?;
        let mut combined_records = self.records.clone();
        for record in &batch.records {
            combined_records.insert(record.stream_sequence, record.clone());
        }
        validate_covered_sequences(&combined_records, &combined_loss)?;
        validate_transition(batch.transition, &combined_loss)?;

        self.records = combined_records;
        self.loss_ranges = combined_loss;
        if batch.transition.is_some() {
            self.transition = batch.transition;
        }
        self.batches.insert(replay_key, fingerprint);
        self.recovered_nonterminal = false;
        Ok(())
    }

    pub(crate) fn mark_recovered(&mut self) {
        self.recovered_nonterminal = !matches!(
            self.transition,
            Some(LogStreamTransition::Complete | LogStreamTransition::Incomplete)
        );
    }

    pub(crate) fn summary(&self) -> Result<LogStreamSummary, LogStoreError> {
        let bounds = sequence_bounds(&self.records, &self.loss_ranges);
        let first_sequence = bounds.map(|(first, _)| first);
        let last_sequence = bounds.map(|(_, last)| last);
        let (ingestion, provider, eviction) = loss_counts(&self.loss_ranges)?;
        let persisted_record_count =
            u64::try_from(self.records.len()).map_err(|_| LogStoreError::CapacityOverflow)?;
        let completeness = match self.transition {
            Some(LogStreamTransition::Complete) => LogStreamCompleteness::Complete,
            Some(LogStreamTransition::Incomplete) => LogStreamCompleteness::Incomplete,
            Some(LogStreamTransition::Active) | None if self.recovered_nonterminal => {
                LogStreamCompleteness::Unknown
            }
            Some(LogStreamTransition::Active) | None => LogStreamCompleteness::Active,
        };
        let ended_at_unix_nanos = matches!(
            self.transition,
            Some(LogStreamTransition::Complete | LogStreamTransition::Incomplete)
        )
        .then(|| {
            self.records
                .values()
                .map(|record| record.observed_timestamp_unix_nanos)
                .max()
                .unwrap_or(self.metadata.started_at_unix_nanos)
        });

        Ok(LogStreamSummary {
            stream: self.metadata.clone(),
            first_sequence,
            last_sequence,
            persisted_record_count,
            ingestion_dropped_count: ingestion,
            provider_dropped_count: provider,
            evicted_record_count: eviction,
            loss_ranges: self.loss_ranges.clone(),
            ended_at_unix_nanos,
            completeness,
            evicted: eviction > 0,
            evicted_from: None,
        })
    }

    pub(crate) fn remove_oldest_record_run(
        &mut self,
        max_records: usize,
    ) -> Result<usize, LogStoreError> {
        let Some((&first, _)) = self.records.first_key_value() else {
            return Ok(0);
        };
        let mut last = first;
        let mut count = 1_usize;
        while count < max_records
            && self.records.contains_key(&last.saturating_add(1))
            && last != u64::MAX
        {
            last = last.checked_add(1).ok_or(LogStoreError::CapacityOverflow)?;
            count = count
                .checked_add(1)
                .ok_or(LogStoreError::CapacityOverflow)?;
        }
        let removed: Vec<_> = self
            .records
            .range(first..=last)
            .map(|(sequence, _)| *sequence)
            .collect();
        for sequence in &removed {
            self.records.remove(sequence);
        }

        let retention_batch = LogBatch {
            stream: self.metadata.clone(),
            batch_id: format!("retention:{first}:{last}"),
            records: Vec::new(),
            loss_ranges: vec![LogLossRange {
                first_sequence: first,
                last_sequence: last,
                cause: LogLossCause::RetentionEviction,
            }],
            transition: None,
        };
        self.append_batch_with_retention_policy(
            retention_batch,
            BatchReplayKey::Retention { first, last },
            true,
        )?;
        Ok(removed.len())
    }
}

fn validate_batch_shape(batch: &LogBatch) -> Result<(), LogStoreError> {
    if batch.batch_id.trim().is_empty()
        || batch.stream.action_key_id.trim().is_empty()
        || batch.stream.action_revision.trim().is_empty()
        || batch
            .stream
            .stream_id
            .runtime_host_id
            .as_ref()
            .trim()
            .is_empty()
    {
        return Err(LogStoreError::InvalidBatch(
            "batch and immutable stream identifiers must not be empty".to_string(),
        ));
    }
    if batch.records.is_empty() && batch.loss_ranges.is_empty() && batch.transition.is_none() {
        return Err(LogStoreError::InvalidBatch(
            "empty batches require a lifecycle transition or loss accounting".to_string(),
        ));
    }

    let mut previous = None;
    for record in &batch.records {
        record
            .validate()
            .map_err(|error| LogStoreError::InvalidBatch(error.to_string()))?;
        if record.stream_sequence == 0
            || previous.is_some_and(|sequence| record.stream_sequence <= sequence)
        {
            return Err(LogStoreError::InvalidBatch(
                "batch record sequences must be non-zero and strictly increasing".to_string(),
            ));
        }
        if record.stream_id != batch.stream.stream_id
            || record.action_key_id != batch.stream.action_key_id
            || record.action_revision != batch.stream.action_revision
            || record.runtime_language != batch.stream.runtime_language
        {
            return Err(LogStoreError::InvalidBatch(
                "record metadata does not match its stream".to_string(),
            ));
        }
        previous = Some(record.stream_sequence);
    }
    Ok(())
}

fn batch_fingerprint(batch: &LogBatch) -> Result<[u8; 32], LogStoreError> {
    let mut encoder = CanonicalHash::new();
    encoder.bytes(b"ryvus.log-batch.v1")?;
    encoder.metadata(&batch.stream)?;
    encoder.string(&batch.batch_id)?;
    encoder.len(batch.records.len())?;
    for record in &batch.records {
        encoder.record(record)?;
    }
    encoder.len(batch.loss_ranges.len())?;
    for range in &batch.loss_ranges {
        encoder.u64(range.first_sequence);
        encoder.u64(range.last_sequence);
        encoder.byte(match range.cause {
            LogLossCause::IngestionOverflow => 0,
            LogLossCause::ProviderFailure => 1,
            LogLossCause::RetentionEviction => 2,
        });
    }
    encoder.byte(match batch.transition {
        None => 0,
        Some(LogStreamTransition::Active) => 1,
        Some(LogStreamTransition::Complete) => 2,
        Some(LogStreamTransition::Incomplete) => 3,
    });
    Ok(encoder.finish())
}

struct CanonicalHash(Sha256);

impl CanonicalHash {
    fn new() -> Self {
        Self(Sha256::new())
    }

    fn finish(self) -> [u8; 32] {
        self.0.finalize().into()
    }

    fn byte(&mut self, value: u8) {
        self.0.update([value]);
    }

    fn u32(&mut self, value: u32) {
        self.0.update(value.to_be_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.0.update(value.to_be_bytes());
    }

    fn i64(&mut self, value: i64) {
        self.0.update(value.to_be_bytes());
    }

    fn len(&mut self, value: usize) -> Result<(), LogStoreError> {
        self.u64(u64::try_from(value).map_err(|_| LogStoreError::CapacityOverflow)?);
        Ok(())
    }

    fn bytes(&mut self, value: &[u8]) -> Result<(), LogStoreError> {
        self.len(value.len())?;
        self.0.update(value);
        Ok(())
    }

    fn string(&mut self, value: &str) -> Result<(), LogStoreError> {
        self.bytes(value.as_bytes())
    }

    fn stream_id(&mut self, stream_id: &LogStreamId) -> Result<(), LogStoreError> {
        self.string(stream_id.execution_scope.as_ref())?;
        self.string(stream_id.runtime_host_id.as_ref())
    }

    fn runtime_kind(&mut self, runtime_kind: &RuntimeKind) {
        self.byte(match runtime_kind {
            RuntimeKind::Python => 0,
            RuntimeKind::Node => 1,
            RuntimeKind::Rust => 2,
        });
    }

    fn metadata(&mut self, metadata: &LogStreamMetadata) -> Result<(), LogStoreError> {
        self.stream_id(&metadata.stream_id)?;
        self.string(&metadata.action_key_id)?;
        self.string(&metadata.action_revision)?;
        self.runtime_kind(&metadata.runtime_language);
        self.i64(metadata.started_at_unix_nanos);
        Ok(())
    }

    fn record(&mut self, record: &ExecutionLogRecord) -> Result<(), LogStoreError> {
        self.i64(record.timestamp_unix_nanos);
        self.i64(record.observed_timestamp_unix_nanos);
        self.u64(record.stream_sequence);
        self.stream_id(&record.stream_id)?;
        self.string(&record.action_key_id)?;
        self.string(&record.action_revision)?;
        self.runtime_kind(&record.runtime_language);
        match &record.runtime_session_id {
            Some(session_id) => {
                self.byte(1);
                self.string(session_id.as_ref())?;
            }
            None => self.byte(0),
        }
        match &record.correlation {
            Some(correlation) => {
                self.byte(1);
                self.string(correlation.execution_id.as_ref())?;
                self.string(correlation.attempt_id.as_ref())?;
                self.u32(correlation.attempt_number);
            }
            None => self.byte(0),
        }
        self.byte(match &record.severity {
            LogLevel::Trace => 0,
            LogLevel::Debug => 1,
            LogLevel::Info => 2,
            LogLevel::Warn => 3,
            LogLevel::Error => 4,
        });
        self.string(&record.message)?;
        self.len(record.attributes.len())?;
        for (key, value) in &record.attributes {
            self.string(key)?;
            self.attribute(value)?;
        }
        match record.trace_id {
            Some(trace_id) => {
                self.byte(1);
                self.bytes(trace_id.as_bytes())?;
            }
            None => self.byte(0),
        }
        match record.span_id {
            Some(span_id) => {
                self.byte(1);
                self.bytes(span_id.as_bytes())?;
            }
            None => self.byte(0),
        }
        Ok(())
    }

    fn attribute(&mut self, value: &AttributeValue) -> Result<(), LogStoreError> {
        match value {
            AttributeValue::String(value) => {
                self.byte(0);
                self.string(value)?;
            }
            AttributeValue::Bool(value) => {
                self.byte(1);
                self.byte(u8::from(*value));
            }
            AttributeValue::I64(value) => {
                self.byte(2);
                self.i64(*value);
            }
            AttributeValue::F64(value) => {
                self.byte(3);
                self.u64(value.to_bits());
            }
            AttributeValue::StringArray(values) => {
                self.byte(4);
                self.len(values.len())?;
                for value in values {
                    self.string(value)?;
                }
            }
            AttributeValue::BoolArray(values) => {
                self.byte(5);
                self.len(values.len())?;
                for value in values {
                    self.byte(u8::from(*value));
                }
            }
            AttributeValue::I64Array(values) => {
                self.byte(6);
                self.len(values.len())?;
                for value in values {
                    self.i64(*value);
                }
            }
            AttributeValue::F64Array(values) => {
                self.byte(7);
                self.len(values.len())?;
                for value in values {
                    self.u64(value.to_bits());
                }
            }
        }
        Ok(())
    }
}

fn validate_transition(
    transition: Option<LogStreamTransition>,
    loss_ranges: &[LogLossRange],
) -> Result<(), LogStoreError> {
    match transition {
        Some(LogStreamTransition::Complete) if !loss_ranges.is_empty() => {
            Err(LogStoreError::InvalidBatch(
                "a stream with committed loss cannot be complete".to_string(),
            ))
        }
        Some(LogStreamTransition::Incomplete) if loss_ranges.is_empty() => Err(
            LogStoreError::InvalidBatch("an incomplete stream requires committed loss".to_string()),
        ),
        _ => Ok(()),
    }
}

fn validate_covered_sequences(
    records: &BTreeMap<u64, ExecutionLogRecord>,
    ranges: &[LogLossRange],
) -> Result<(), LogStoreError> {
    let Some((_, last)) = sequence_bounds(records, ranges) else {
        return Ok(());
    };
    let mut expected = 1_u64;
    let mut record_sequences = records.keys().copied().peekable();
    let mut ranges = ranges.iter().peekable();
    while expected <= last {
        if record_sequences.peek().copied() == Some(expected) {
            record_sequences.next();
            expected = match expected.checked_add(1) {
                Some(next) => next,
                None if expected == last => break,
                None => return Err(LogStoreError::CapacityOverflow),
            };
            continue;
        }
        if let Some(range) = ranges.peek() {
            if range.first_sequence == expected {
                expected = match range.last_sequence.checked_add(1) {
                    Some(next) => next,
                    None if range.last_sequence == last => break,
                    None => return Err(LogStoreError::CapacityOverflow),
                };
                ranges.next();
                continue;
            }
        }
        return Err(LogStoreError::InvalidBatch(format!(
            "sequence gap beginning at {expected} is not accounted as loss"
        )));
    }
    Ok(())
}

fn sequence_is_lost(ranges: &[LogLossRange], sequence: u64) -> bool {
    ranges
        .iter()
        .any(|range| range.first_sequence <= sequence && sequence <= range.last_sequence)
}

fn sequence_bounds(
    records: &BTreeMap<u64, ExecutionLogRecord>,
    ranges: &[LogLossRange],
) -> Option<(u64, u64)> {
    let first = records
        .first_key_value()
        .map(|(sequence, _)| *sequence)
        .into_iter()
        .chain(ranges.first().map(|range| range.first_sequence))
        .min()?;
    let last = records
        .last_key_value()
        .map(|(sequence, _)| *sequence)
        .into_iter()
        .chain(ranges.last().map(|range| range.last_sequence))
        .max()?;
    Some((first, last))
}

fn loss_counts(ranges: &[LogLossRange]) -> Result<(u64, u64, u64), LogStoreError> {
    let mut counts = (0_u64, 0_u64, 0_u64);
    for range in ranges {
        let count = range
            .last_sequence
            .checked_sub(range.first_sequence)
            .and_then(|difference| difference.checked_add(1))
            .ok_or(LogStoreError::CapacityOverflow)?;
        let target = match range.cause {
            LogLossCause::IngestionOverflow => &mut counts.0,
            LogLossCause::ProviderFailure => &mut counts.1,
            LogLossCause::RetentionEviction => &mut counts.2,
        };
        *target = target
            .checked_add(count)
            .ok_or(LogStoreError::CapacityOverflow)?;
    }
    Ok(counts)
}
