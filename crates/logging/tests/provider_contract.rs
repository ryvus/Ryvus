use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicU64, Ordering},
};

use ryvus_logging::{
    normalize_loss_ranges, AttributeValue, ExecutionLogCorrelation, ExecutionLogRecord,
    ExecutionLogStore, FilesystemExecutionLogStore, FilesystemLogStoreConfig,
    InMemoryExecutionLogStore, LogBatch, LogLossCause, LogLossRange, LogRecordQuery, LogStoreError,
    LogStreamCompleteness, LogStreamId, LogStreamMetadata, LogStreamQuery, LogStreamTransition,
    MemoryLogStoreConfig,
};
use ryvus_protocol::{
    AttemptId, ExecutionId, ExecutionScopeId, LogLevel, RuntimeHostId, RuntimeKind,
};

fn scope(value: &str) -> ExecutionScopeId {
    ExecutionScopeId::new(value).expect("test scope should be valid")
}

fn metadata(scope_name: &str, host: &str, started_at: i64) -> LogStreamMetadata {
    LogStreamMetadata {
        stream_id: LogStreamId::new(scope(scope_name), RuntimeHostId::from(host)),
        action_key_id: "action".to_string(),
        action_revision: "revision".to_string(),
        runtime_language: RuntimeKind::Rust,
        started_at_unix_nanos: started_at,
    }
}

fn record(
    stream: &LogStreamMetadata,
    sequence: u64,
    execution: &str,
    attempt: &str,
) -> ExecutionLogRecord {
    ExecutionLogRecord {
        timestamp_unix_nanos: i64::try_from(sequence).expect("small test sequence"),
        observed_timestamp_unix_nanos: i64::try_from(sequence).expect("small test sequence"),
        stream_sequence: sequence,
        stream_id: stream.stream_id.clone(),
        action_key_id: stream.action_key_id.clone(),
        action_revision: stream.action_revision.clone(),
        runtime_language: stream.runtime_language.clone(),
        runtime_session_id: None,
        correlation: Some(
            ExecutionLogCorrelation::new(ExecutionId::from(execution), AttemptId::from(attempt), 1)
                .expect("test correlation should be valid"),
        ),
        severity: LogLevel::Info,
        message: format!("record {sequence}"),
        attributes: BTreeMap::new(),
        trace_id: None,
        span_id: None,
    }
}

fn batch(
    stream: &LogStreamMetadata,
    batch_id: &str,
    sequences: &[u64],
    losses: Vec<LogLossRange>,
    transition: Option<LogStreamTransition>,
) -> LogBatch {
    LogBatch {
        stream: stream.clone(),
        batch_id: batch_id.to_string(),
        records: sequences
            .iter()
            .map(|sequence| record(stream, *sequence, "execution", "attempt"))
            .collect(),
        loss_ranges: losses,
        transition,
    }
}

fn stream_query(scope_name: &str, limit: usize) -> LogStreamQuery {
    LogStreamQuery {
        execution_scope: scope(scope_name),
        action_key_id: None,
        action_revision: None,
        runtime_host_id: None,
        execution_id: None,
        attempt_id: None,
        severity: None,
        message_contains: None,
        cursor: None,
        limit,
    }
}

fn record_query(stream_id: LogStreamId, limit: usize) -> LogRecordQuery {
    LogRecordQuery {
        stream_id,
        execution_id: None,
        attempt_id: None,
        severity: None,
        message_contains: None,
        cursor: None,
        limit,
    }
}

fn replay_is_idempotent_and_conflicting_replay_is_rejected(store: &dyn ExecutionLogStore) {
    let stream = metadata("scope", "host", 1);
    let first = batch(&stream, "batch-1", &[1], Vec::new(), None);
    store.append_batch(first.clone()).expect("first append");
    store.append_batch(first.clone()).expect("identical replay");

    let mut conflicting = first;
    conflicting.records[0].message = "different".to_string();
    assert!(matches!(
        store.append_batch(conflicting),
        Err(LogStoreError::Conflict(_))
    ));
    assert_eq!(
        store
            .list_records(record_query(stream.stream_id, 10))
            .expect("records")
            .records
            .len(),
        1
    );
}

fn stream_identity_and_batch_identity_are_scope_local(store: &dyn ExecutionLogStore) {
    let first = metadata("scope-a", "same-host", 1);
    let second = metadata("scope-b", "same-host", 1);
    store
        .append_batch(batch(&first, "same-batch", &[1], Vec::new(), None))
        .expect("first scope");
    store
        .append_batch(batch(&second, "same-batch", &[1], Vec::new(), None))
        .expect("second scope");

    assert_eq!(
        store
            .list_streams(stream_query("scope-a", 10))
            .expect("a")
            .streams
            .len(),
        1
    );
    assert_eq!(
        store
            .list_streams(stream_query("scope-b", 10))
            .expect("b")
            .streams
            .len(),
        1
    );
}

fn validates_sequences_metadata_loss_and_terminality(store: &dyn ExecutionLogStore) {
    let stream = metadata("scope", "host", 1);
    let loss = LogLossRange {
        first_sequence: 2,
        last_sequence: 2,
        cause: LogLossCause::IngestionOverflow,
    };
    store
        .append_batch(batch(&stream, "first", &[1, 3], vec![loss], None))
        .expect("accounted gap");

    assert!(matches!(
        store.append_batch(batch(&stream, "overlap", &[3], Vec::new(), None)),
        Err(LogStoreError::Conflict(_))
    ));
    assert!(matches!(
        store.append_batch(batch(
            &stream,
            "loss-overlap",
            &[],
            vec![LogLossRange {
                first_sequence: 2,
                last_sequence: 2,
                cause: LogLossCause::IngestionOverflow,
            }],
            None,
        )),
        Err(LogStoreError::Conflict(_))
    ));
    let mut changed = stream.clone();
    changed.action_revision = "changed".to_string();
    assert!(matches!(
        store.append_batch(batch(&changed, "metadata", &[4], Vec::new(), None)),
        Err(LogStoreError::Conflict(_))
    ));
    assert!(matches!(
        store.append_batch(batch(&stream, "gap", &[5], Vec::new(), None)),
        Err(LogStoreError::InvalidBatch(_))
    ));
    store
        .append_batch(batch(
            &stream,
            "terminal",
            &[],
            Vec::new(),
            Some(LogStreamTransition::Incomplete),
        ))
        .expect("incomplete terminal transition");
    assert!(matches!(
        store.append_batch(batch(&stream, "late", &[4], Vec::new(), None)),
        Err(LogStoreError::Conflict(_))
    ));
}

fn filters_and_paginates_deterministically(store: &dyn ExecutionLogStore) {
    let older = metadata("scope", "host-a", 1);
    let newer = metadata("scope", "host-b", 2);
    store
        .append_batch(batch(&older, "older", &[1, 2], Vec::new(), None))
        .expect("older");
    store
        .append_batch(batch(&newer, "newer", &[1], Vec::new(), None))
        .expect("newer");

    let first_page = store
        .list_streams(stream_query("scope", 1))
        .expect("first page");
    assert_eq!(first_page.streams[0].stream.stream_id, newer.stream_id);
    let mut next_query = stream_query("scope", 1);
    next_query.cursor = first_page.next_cursor;
    let second_page = store.list_streams(next_query).expect("second page");
    assert_eq!(second_page.streams[0].stream.stream_id, older.stream_id);

    let mut records = record_query(older.stream_id, 1);
    let first_records = store.list_records(records.clone()).expect("first records");
    assert_eq!(first_records.records[0].stream_sequence, 1);
    records.cursor = first_records.next_cursor;
    let second_records = store.list_records(records).expect("second records");
    assert_eq!(second_records.records[0].stream_sequence, 2);

    let mut filtered = stream_query("scope", 10);
    filtered.execution_id = Some(ExecutionId::from("missing"));
    assert!(store
        .list_streams(filtered)
        .expect("filtered")
        .streams
        .is_empty());
}

fn record_filters(store: &dyn ExecutionLogStore) {
    let stream = metadata("scope", "host", 1);
    let mut records = vec![
        record(&stream, 1, "execution", "attempt"),
        record(&stream, 2, "execution", "attempt"),
        record(&stream, 3, "execution", "attempt"),
    ];
    records[0].message = "database connected".into();
    records[1].severity = LogLevel::Error;
    records[1].message = "cache failed".into();
    records[2].severity = LogLevel::Error;
    records[2].message = "Database unavailable".into();
    store
        .append_batch(LogBatch {
            stream: stream.clone(),
            batch_id: "filters".into(),
            records,
            loss_ranges: Vec::new(),
            transition: None,
        })
        .expect("filtered records");
    let mismatch = metadata("scope", "mismatch", 2);
    let mut mismatch_records = vec![
        record(&mismatch, 1, "execution", "attempt"),
        record(&mismatch, 2, "execution", "attempt"),
    ];
    mismatch_records[0].severity = LogLevel::Error;
    mismatch_records[0].message = "cache failed".into();
    mismatch_records[1].message = "database connected".into();
    store
        .append_batch(LogBatch {
            stream: mismatch,
            batch_id: "mismatch".into(),
            records: mismatch_records,
            loss_ranges: Vec::new(),
            transition: None,
        })
        .expect("nonmatching stream records");

    let page = store
        .list_records(LogRecordQuery {
            stream_id: stream.stream_id.clone(),
            execution_id: None,
            attempt_id: None,
            severity: Some(LogLevel::Error),
            message_contains: Some("DATABASE".into()),
            cursor: None,
            limit: 10,
        })
        .expect("record filters");
    assert_eq!(
        page.records
            .iter()
            .map(|record| record.stream_sequence)
            .collect::<Vec<_>>(),
        [3]
    );

    let streams = store
        .list_streams(LogStreamQuery {
            execution_scope: scope("scope"),
            action_key_id: Some("action".into()),
            action_revision: None,
            runtime_host_id: None,
            execution_id: None,
            attempt_id: None,
            severity: Some(LogLevel::Error),
            message_contains: Some("DATABASE".into()),
            cursor: None,
            limit: 10,
        })
        .expect("stream filters");
    assert_eq!(streams.streams.len(), 1);

    let first = store
        .list_records(LogRecordQuery {
            stream_id: stream.stream_id.clone(),
            execution_id: None,
            attempt_id: None,
            severity: None,
            message_contains: Some("DATABASE".into()),
            cursor: None,
            limit: 1,
        })
        .expect("first filtered page");
    assert_eq!(first.records[0].stream_sequence, 1);
    let second = store
        .list_records(LogRecordQuery {
            stream_id: stream.stream_id,
            execution_id: None,
            attempt_id: None,
            severity: None,
            message_contains: Some("DATABASE".into()),
            cursor: first.next_cursor,
            limit: 1,
        })
        .expect("second filtered page");
    assert_eq!(
        second
            .records
            .iter()
            .map(|record| record.stream_sequence)
            .collect::<Vec<_>>(),
        [3]
    );
    assert_eq!(second.next_cursor, None);
}

fn loss_ranges_are_canonical_and_summary_counts_are_derived(store: &dyn ExecutionLogStore) {
    let normalized = normalize_loss_ranges(vec![
        LogLossRange {
            first_sequence: 2,
            last_sequence: 3,
            cause: LogLossCause::ProviderFailure,
        },
        LogLossRange {
            first_sequence: 1,
            last_sequence: 1,
            cause: LogLossCause::ProviderFailure,
        },
    ])
    .expect("ranges normalize");
    assert_eq!(normalized.len(), 1);
    assert_eq!(normalized[0].first_sequence, 1);
    assert_eq!(normalized[0].last_sequence, 3);
    assert!(normalize_loss_ranges(vec![
        LogLossRange {
            first_sequence: 1,
            last_sequence: 2,
            cause: LogLossCause::ProviderFailure,
        },
        LogLossRange {
            first_sequence: 2,
            last_sequence: 3,
            cause: LogLossCause::IngestionOverflow,
        },
    ])
    .is_err());

    let stream = metadata("scope", "loss", 1);
    store
        .append_batch(batch(
            &stream,
            "loss",
            &[],
            vec![LogLossRange {
                first_sequence: 1,
                last_sequence: 3,
                cause: LogLossCause::ProviderFailure,
            }],
            Some(LogStreamTransition::Incomplete),
        ))
        .expect("loss batch");
    let summary = &store
        .list_streams(stream_query("scope", 10))
        .expect("summary")
        .streams[0];
    assert_eq!(summary.provider_dropped_count, 3);
    assert_eq!(summary.loss_ranges, normalized);
}

fn run_provider_contract(make_store: impl Fn() -> Box<dyn ExecutionLogStore>) {
    replay_is_idempotent_and_conflicting_replay_is_rejected(make_store().as_ref());
    stream_identity_and_batch_identity_are_scope_local(make_store().as_ref());
    validates_sequences_metadata_loss_and_terminality(make_store().as_ref());
    filters_and_paginates_deterministically(make_store().as_ref());
    loss_ranges_are_canonical_and_summary_counts_are_derived(make_store().as_ref());
}

#[test]
fn memory_record_filters() {
    record_filters(&InMemoryExecutionLogStore::default());
}

#[test]
fn filesystem_record_filters() {
    let root = std::env::temp_dir().join(format!(
        "ryvus-logging-record-filter-contract-{}",
        std::process::id()
    ));
    let store = FilesystemExecutionLogStore::new(FilesystemLogStoreConfig {
        root: root.clone(),
        ..FilesystemLogStoreConfig::default()
    })
    .expect("filesystem store");
    record_filters(&store);
    drop(store);
    std::fs::remove_dir_all(root).expect("remove contract files");
}

#[test]
fn memory_provider_contract() {
    run_provider_contract(|| Box::new(InMemoryExecutionLogStore::default()));
}

#[test]
fn filesystem_provider_contract() {
    static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);
    let base = std::env::temp_dir().join(format!("ryvus-logging-contract-{}", std::process::id()));
    run_provider_contract(|| {
        let root = base.join(NEXT_ROOT.fetch_add(1, Ordering::Relaxed).to_string());
        Box::new(
            FilesystemExecutionLogStore::new(FilesystemLogStoreConfig {
                root,
                ..FilesystemLogStoreConfig::default()
            })
            .expect("filesystem store"),
        )
    });
    std::fs::remove_dir_all(base).expect("remove contract files");
}

#[test]
fn memory_retention_is_bounded_and_accounts_exact_eviction() {
    let store = InMemoryExecutionLogStore::new(MemoryLogStoreConfig {
        max_streams: 2,
        max_records: 2,
        max_tombstones: 1,
    })
    .expect("valid config");
    let active = metadata("scope", "active", 3);
    let active_batch = batch(&active, "active", &[1, 2, 3], Vec::new(), None);
    store
        .append_batch(active_batch.clone())
        .expect("active records");
    store
        .append_batch(active_batch.clone())
        .expect("evicted payload replay remains idempotent");
    let mut conflicting_replay = active_batch;
    conflicting_replay.records[0].message = "changed after eviction".to_string();
    assert!(matches!(
        store.append_batch(conflicting_replay),
        Err(LogStoreError::Conflict(_))
    ));
    let active_summary = &store
        .list_streams(stream_query("scope", 10))
        .expect("summary")
        .streams[0];
    assert_eq!(active_summary.persisted_record_count, 2);
    assert_eq!(active_summary.evicted_record_count, 1);
    assert_eq!(
        active_summary.loss_ranges[0],
        LogLossRange {
            first_sequence: 1,
            last_sequence: 1,
            cause: LogLossCause::RetentionEviction,
        }
    );

    for (host, started) in [("complete-a", 1), ("complete-b", 2)] {
        let stream = metadata("scope", host, started);
        store
            .append_batch(batch(
                &stream,
                host,
                &[1],
                Vec::new(),
                Some(LogStreamTransition::Complete),
            ))
            .expect("complete stream");
    }
    assert!(
        store
            .list_streams(stream_query("scope", 10))
            .expect("bounded")
            .streams
            .len()
            <= 3
    );
    assert!(store.tombstone_count().expect("tombstones") <= 1);
    assert!(store
        .list_streams(stream_query("scope", 10))
        .expect("streams")
        .streams
        .iter()
        .any(|summary| {
            summary.evicted
                && summary.completeness == LogStreamCompleteness::Incomplete
                && summary.evicted_from == Some(LogStreamCompleteness::Complete)
        }));
    let mut content_query = stream_query("scope", 10);
    content_query.severity = Some(LogLevel::Info);
    content_query.message_contains = Some("RECORD".into());
    let content_filtered = store
        .list_streams(content_query)
        .expect("content filtered streams")
        .streams;
    assert!(!content_filtered.is_empty());
    assert!(content_filtered
        .iter()
        .all(|summary| summary.evicted_from.is_none()));
}

#[test]
fn memory_retention_selects_complete_streams_before_other_states() {
    for config in [
        MemoryLogStoreConfig {
            max_streams: 2,
            max_records: 100,
            max_tombstones: 2,
        },
        MemoryLogStoreConfig {
            max_streams: 10,
            max_records: 2,
            max_tombstones: 2,
        },
    ] {
        assert_complete_first(config);
    }
}

#[test]
fn memory_caller_batch_ids_do_not_collide_with_retention_identity() {
    let store = InMemoryExecutionLogStore::new(MemoryLogStoreConfig {
        max_streams: 10,
        max_records: 1,
        max_tombstones: 1,
    })
    .expect("valid config");
    let stream = metadata("scope", "collision", 1);
    let caller_batch = batch(&stream, "retention:1:1", &[1, 2], Vec::new(), None);
    store
        .append_batch(caller_batch.clone())
        .expect("caller name must not collide with provider retention");
    store
        .append_batch(caller_batch.clone())
        .expect("caller replay remains idempotent");
    let mut conflicting = caller_batch;
    conflicting.records[0].message = "different".to_string();
    assert!(matches!(
        store.append_batch(conflicting),
        Err(LogStoreError::Conflict(_))
    ));
}

#[test]
fn memory_replay_fingerprint_preserves_non_finite_float_bits() {
    let store = InMemoryExecutionLogStore::new(MemoryLogStoreConfig {
        max_streams: 10,
        max_records: 1,
        max_tombstones: 1,
    })
    .expect("valid config");
    let stream = metadata("scope", "floats", 1);
    let mut float_batch = batch(&stream, "floats", &[1, 2], Vec::new(), None);
    float_batch.records[0].attributes.insert(
        "scalar".to_string(),
        AttributeValue::F64(f64::from_bits(0x7ff8_0000_0000_0001)),
    );
    float_batch.records[0].attributes.insert(
        "array".to_string(),
        AttributeValue::F64Array(vec![f64::INFINITY, f64::NEG_INFINITY, f64::NAN]),
    );
    store
        .append_batch(float_batch.clone())
        .expect("non-finite values are valid model content");
    store
        .append_batch(float_batch.clone())
        .expect("bit-identical non-finite replay");

    let mut conflicting = float_batch;
    conflicting.records[0].attributes.insert(
        "scalar".to_string(),
        AttributeValue::F64(f64::from_bits(0x7ff8_0000_0000_0002)),
    );
    assert!(matches!(
        store.append_batch(conflicting),
        Err(LogStoreError::Conflict(_))
    ));
}

fn assert_complete_first(config: MemoryLogStoreConfig) {
    let store = InMemoryExecutionLogStore::new(config).expect("valid config");
    let incomplete = metadata("scope", "incomplete", 1);
    store
        .append_batch(batch(
            &incomplete,
            "incomplete",
            &[1],
            vec![LogLossRange {
                first_sequence: 2,
                last_sequence: 2,
                cause: LogLossCause::ProviderFailure,
            }],
            Some(LogStreamTransition::Incomplete),
        ))
        .expect("incomplete stream");
    let active = metadata("scope", "active", 2);
    store
        .append_batch(batch(&active, "active", &[1], Vec::new(), None))
        .expect("active stream");
    let complete = metadata("scope", "complete", 3);
    store
        .append_batch(batch(
            &complete,
            "complete",
            &[1],
            Vec::new(),
            Some(LogStreamTransition::Complete),
        ))
        .expect("complete stream");

    let summaries = store
        .list_streams(stream_query("scope", 10))
        .expect("summaries")
        .streams;
    let complete_tombstone = summaries
        .iter()
        .find(|summary| summary.stream.stream_id == complete.stream_id)
        .expect("complete stream should become the tombstone");
    assert!(complete_tombstone.evicted);
    assert_eq!(
        complete_tombstone.evicted_from,
        Some(LogStreamCompleteness::Complete)
    );
    assert_eq!(complete_tombstone.evicted_record_count, 1);
    assert_eq!(
        complete_tombstone.loss_ranges,
        vec![LogLossRange {
            first_sequence: 1,
            last_sequence: 1,
            cause: LogLossCause::RetentionEviction,
        }]
    );
    assert!(summaries
        .iter()
        .any(|summary| summary.stream.stream_id == incomplete.stream_id && !summary.evicted));
    assert!(summaries
        .iter()
        .any(|summary| summary.stream.stream_id == active.stream_id && !summary.evicted));
}
