use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Arc, Barrier, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use mockall::mock;
use ryvus_logging::{
    AttributeValue, ExecutionLogStore, LogBatch, LogLossCause, LogRecordPage, LogRecordQuery,
    LogStoreError, LogStreamMetadata, LogStreamPage, LogStreamQuery, LogStreamTransition,
    RuntimeLogContext,
};
use ryvus_protocol::{
    AttemptId, ExecutionId, ExecutionScopeId, LogEvent, LogLevel, RuntimeHostId, RuntimeKind,
};
use ryvus_runtime_host::{
    normalize_log_event, LogNormalizationLimits, LogOverflowPolicy, RuntimeLogWriter,
    RuntimeLogWriterConfig, RuntimeLogWriterError,
};
use serde_json::json;

mock! {
    Store {}

    impl ExecutionLogStore for Store {
        fn append_batch(&self, batch: LogBatch) -> Result<(), LogStoreError>;
        fn list_streams(&self, query: LogStreamQuery) -> Result<LogStreamPage, LogStoreError>;
        fn list_records(&self, query: LogRecordQuery) -> Result<LogRecordPage, LogStoreError>;
    }
}

fn context() -> RuntimeLogContext {
    RuntimeLogContext::new(
        ExecutionScopeId::new("scope").expect("scope"),
        "action",
        "revision",
        RuntimeKind::Rust,
    )
    .expect("context")
}

fn event(message: &str) -> LogEvent {
    LogEvent {
        execution_id: ExecutionId::from("execution"),
        attempt_id: AttemptId::from("attempt"),
        attempt_number: 1,
        timestamp_unix_nanos: Some(10),
        trace_id: None,
        span_id: None,
        level: LogLevel::Info,
        message: message.to_string(),
        fields: json!({}),
    }
}

fn config(capacity: usize, batch_size: usize, policy: LogOverflowPolicy) -> RuntimeLogWriterConfig {
    RuntimeLogWriterConfig {
        capacity,
        batch_size,
        flush_interval: Duration::from_secs(30),
        retry_max_attempts: 3,
        retry_initial_backoff: Duration::ZERO,
        retry_max_backoff: Duration::ZERO,
        overflow_policy: policy,
        grace_period: Duration::from_secs(1),
        cleanup_period: Duration::from_secs(1),
        normalization: LogNormalizationLimits::default(),
    }
}

fn writer(store: MockStore, config: RuntimeLogWriterConfig) -> RuntimeLogWriter {
    RuntimeLogWriter::new(
        Arc::new(store),
        context(),
        RuntimeHostId::from("host"),
        1,
        config,
        None,
    )
    .expect("writer")
}

#[test]
fn normalization_is_typed_deterministic_and_preserves_invalid_trace_as_diagnostics() {
    let context = context();
    let stream = LogStreamMetadata {
        stream_id: ryvus_logging::LogStreamId::new(
            context.execution_scope,
            RuntimeHostId::from("host"),
        ),
        action_key_id: context.action_key_id,
        action_revision: context.action_revision,
        runtime_language: context.runtime_language,
        started_at_unix_nanos: 1,
    };
    let mut log = event("ééé");
    log.trace_id = Some("00112233445566778899aabbccddeeff".to_string());
    log.span_id = Some("invalid".to_string());
    log.fields = json!({
        "typed": [1, 2],
        "nested": {"z": 1, "a": {"d": 4, "c": 3}},
        "mixed": [1, "two"]
    });
    let limits = LogNormalizationLimits {
        max_message_bytes: 5,
        ..LogNormalizationLimits::default()
    };

    let record = normalize_log_event(log, 20, 1, None, &stream, &limits);

    assert_eq!(record.message, "éé");
    assert!(record.trace_id.is_some());
    assert!(record.span_id.is_none());
    assert_eq!(
        record.attributes.get("typed"),
        Some(&AttributeValue::I64Array(vec![1, 2]))
    );
    assert_eq!(
        record.attributes.get("nested"),
        Some(&AttributeValue::String(
            r#"{"a":{"c":3,"d":4},"z":1}"#.to_string()
        ))
    );
    assert!(record
        .attributes
        .contains_key("ryvus.log.invalid_trace_context"));
    assert!(record
        .attributes
        .contains_key("ryvus.log.stringified_attributes"));
}

#[test]
fn enqueue_never_waits_for_provider_and_active_batch_is_not_overflowed() {
    assert_overflow(LogOverflowPolicy::DropNewest, 4, &[2, 3]);
    assert_overflow(LogOverflowPolicy::DropOldest, 2, &[3, 4]);
}

fn assert_overflow(policy: LogOverflowPolicy, dropped: u64, queued: &[u64]) {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let batches = Arc::new(Mutex::new(Vec::new()));
    let provider_thread = Arc::new(Mutex::new(None));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut store = MockStore::new();
    store.expect_append_batch().returning({
        let release_rx = Arc::clone(&release_rx);
        let batches = Arc::clone(&batches);
        let provider_thread = Arc::clone(&provider_thread);
        let calls = Arc::clone(&calls);
        move |batch| {
            batches.lock().expect("batches").push(batch);
            *provider_thread.lock().expect("thread") = Some(thread::current().id());
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                entered_tx.send(()).expect("entered");
                release_rx
                    .lock()
                    .expect("release lock")
                    .recv()
                    .expect("release");
            }
            Ok(())
        }
    });
    let writer = writer(store, config(2, 1, policy));
    writer.enqueue(event("one"), 1, None).expect("first");
    entered_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("provider entered");

    writer.enqueue(event("two"), 2, None).expect("second");
    writer.enqueue(event("three"), 3, None).expect("third");
    let before = Instant::now();
    writer
        .enqueue(event("four"), 4, None)
        .expect("overflow enqueue");
    assert!(before.elapsed() < Duration::from_millis(100));
    assert_ne!(
        *provider_thread.lock().expect("thread"),
        Some(thread::current().id())
    );
    assert_eq!(
        writer.writer_known_loss().expect("loss")[0].first_sequence,
        dropped
    );

    release_tx.send(()).expect("release provider");
    writer
        .shutdown(Instant::now() + Duration::from_secs(2))
        .expect("shutdown");
    let batches = batches.lock().expect("batches");
    let persisted: Vec<_> = batches
        .iter()
        .flat_map(|batch| batch.records.iter().map(|record| record.stream_sequence))
        .collect();
    assert_eq!(persisted[0], 1);
    assert_eq!(&persisted[1..], queued);
    assert_eq!(
        batches
            .iter()
            .flat_map(|batch| &batch.loss_ranges)
            .filter(|range| range.first_sequence <= dropped && dropped <= range.last_sequence)
            .count(),
        1
    );
}

#[test]
fn transient_failure_replays_identical_batch_and_console_backpressure_is_non_durable() {
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut store = MockStore::new();
    store.expect_append_batch().returning({
        let attempts = Arc::clone(&attempts);
        let calls = Arc::clone(&calls);
        move |batch| {
            attempts.lock().expect("attempts").push(batch);
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(LogStoreError::Io)
            } else {
                Ok(())
            }
        }
    });
    let (console_tx, console_rx) = mpsc::sync_channel(1);
    let writer = RuntimeLogWriter::new(
        Arc::new(store),
        context(),
        RuntimeHostId::from("host"),
        1,
        config(2, 2, LogOverflowPolicy::DropNewest),
        Some(console_tx),
    )
    .expect("writer");
    writer.enqueue(event("one"), 1, None).expect("one");
    writer.enqueue(event("two"), 2, None).expect("two");
    writer
        .shutdown(Instant::now() + Duration::from_secs(2))
        .expect("shutdown");

    let attempts = attempts.lock().expect("attempts");
    assert!(attempts.len() >= 2);
    assert_eq!(attempts[0], attempts[1]);
    assert!(attempts.iter().all(|batch| batch.loss_ranges.is_empty()));
    assert_eq!(
        console_rx
            .try_recv()
            .expect("one console record")
            .stream_sequence,
        1
    );
    assert!(console_rx.try_recv().is_err());
}

#[test]
fn exhausted_failure_becomes_provider_loss_and_recovery_commits_it_once() {
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let first_failed = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let mut store = MockStore::new();
    store.expect_append_batch().returning({
        let attempts = Arc::clone(&attempts);
        let calls = Arc::clone(&calls);
        let first_failed = Arc::clone(&first_failed);
        move |batch| {
            attempts.lock().expect("attempts").push(batch);
            let call = calls.fetch_add(1, Ordering::SeqCst);
            if call < 3 {
                if call == 2 {
                    *first_failed.0.lock().expect("failed") = true;
                    first_failed.1.notify_all();
                }
                Err(LogStoreError::Unavailable)
            } else {
                Ok(())
            }
        }
    });
    let writer = writer(store, config(2, 1, LogOverflowPolicy::DropNewest));
    writer.enqueue(event("one"), 1, None).expect("one");
    let mut failed = first_failed.0.lock().expect("failed");
    while !*failed {
        failed = first_failed.1.wait(failed).expect("wait failed");
    }
    drop(failed);
    writer.enqueue(event("two"), 2, None).expect("two");
    writer
        .shutdown(Instant::now() + Duration::from_secs(2))
        .expect("shutdown");

    let attempts = attempts.lock().expect("attempts");
    assert_eq!(attempts[0], attempts[1]);
    assert_eq!(attempts[1], attempts[2]);
    let accepted = &attempts[3..];
    assert_eq!(
        accepted
            .iter()
            .flat_map(|batch| &batch.loss_ranges)
            .filter(|range| range.first_sequence == 1 && range.last_sequence == 1)
            .count(),
        1
    );
}

#[test]
fn permanent_failure_is_not_retried_and_is_committed_after_recovery() {
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let failed = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let mut store = MockStore::new();
    store.expect_append_batch().returning({
        let attempts = Arc::clone(&attempts);
        let calls = Arc::clone(&calls);
        let failed = Arc::clone(&failed);
        move |batch| {
            attempts.lock().expect("attempts").push(batch);
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                *failed.0.lock().expect("failed") = true;
                failed.1.notify_all();
                Err(LogStoreError::Conflict("permanent".to_string()))
            } else {
                Ok(())
            }
        }
    });
    let writer = writer(store, config(2, 1, LogOverflowPolicy::DropNewest));
    writer.enqueue(event("one"), 1, None).expect("one");
    let mut observed = failed.0.lock().expect("failed");
    while !*observed {
        observed = failed.1.wait(observed).expect("wait failed");
    }
    drop(observed);
    writer.enqueue(event("two"), 2, None).expect("two");
    writer
        .shutdown(Instant::now() + Duration::from_secs(2))
        .expect("shutdown");

    let attempts = attempts.lock().expect("attempts");
    assert_eq!(attempts[0].records[0].stream_sequence, 1);
    assert_eq!(attempts[1].records[0].stream_sequence, 2);
    assert_eq!(attempts[1].loss_ranges[0].first_sequence, 1);
    assert_eq!(writer.writer_known_loss().expect("known loss").len(), 1);
}

#[test]
fn drain_and_shutdown_deadlines_do_not_join_a_blocked_provider() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let mut store = MockStore::new();
    store.expect_append_batch().returning({
        let release_rx = Arc::clone(&release_rx);
        move |_| {
            entered_tx.send(()).ok();
            release_rx.lock().expect("release lock").recv().ok();
            Ok(())
        }
    });
    let mut writer_config = config(1, 1, LogOverflowPolicy::DropNewest);
    writer_config.grace_period = Duration::from_millis(20);
    writer_config.cleanup_period = Duration::from_millis(20);
    let writer = writer(store, writer_config);
    writer.enqueue(event("one"), 1, None).expect("one");
    entered_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("entered");

    assert_eq!(
        writer.drain(Instant::now() + Duration::from_millis(20)),
        Err(RuntimeLogWriterError::DeadlineExpired)
    );
    assert_eq!(
        writer.shutdown(Instant::now() + Duration::from_millis(40)),
        Err(RuntimeLogWriterError::CleanupTimeout)
    );
    release_tx.send(()).expect("release");
}

#[test]
fn concurrent_enqueue_preserves_sequence_for_single_and_multi_record_batches() {
    for batch_size in [1, 16] {
        let batches = Arc::new(Mutex::new(Vec::new()));
        let mut store = MockStore::new();
        store.expect_append_batch().returning({
            let batches = Arc::clone(&batches);
            move |batch| {
                batches.lock().expect("batches").push(batch);
                Ok(())
            }
        });
        let writer = Arc::new(writer(
            store,
            config(32, batch_size, LogOverflowPolicy::DropNewest),
        ));
        let barrier = Arc::new(Barrier::new(17));
        let mut producers = Vec::new();
        for index in 0..16 {
            let writer = Arc::clone(&writer);
            let barrier = Arc::clone(&barrier);
            producers.push(thread::spawn(move || {
                barrier.wait();
                let message = if index % 2 == 0 {
                    "x".repeat(32 * 1024)
                } else {
                    index.to_string()
                };
                writer
                    .enqueue(event(&message), index, None)
                    .expect("enqueue")
            }));
        }
        barrier.wait();
        for producer in producers {
            producer.join().expect("producer");
        }
        writer
            .shutdown(Instant::now() + Duration::from_secs(2))
            .expect("shutdown");
        let sequences: Vec<_> = batches
            .lock()
            .expect("batches")
            .iter()
            .flat_map(|batch| batch.records.iter().map(|record| record.stream_sequence))
            .collect();
        assert_eq!(sequences, (1..=16).collect::<Vec<_>>());
    }
}

#[test]
fn permanent_final_failure_commits_loss_only_incomplete_terminal_batch() {
    assert_final_failure_recovery(false);
}

#[test]
fn exhausted_final_failure_commits_loss_only_incomplete_terminal_batch() {
    assert_final_failure_recovery(true);
}

fn assert_final_failure_recovery(transient: bool) {
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut store = MockStore::new();
    store.expect_append_batch().returning({
        let attempts = Arc::clone(&attempts);
        let calls = Arc::clone(&calls);
        move |batch| {
            attempts.lock().expect("attempts").push(batch);
            let call = calls.fetch_add(1, Ordering::SeqCst);
            if (transient && call < 3) || (!transient && call == 0) {
                if transient {
                    Err(LogStoreError::Unavailable)
                } else {
                    Err(LogStoreError::Conflict("permanent".to_string()))
                }
            } else {
                Ok(())
            }
        }
    });
    let writer = writer(store, config(2, 2, LogOverflowPolicy::DropNewest));
    writer.enqueue(event("final"), 1, None).expect("enqueue");
    writer
        .shutdown(Instant::now() + Duration::from_secs(2))
        .expect("shutdown");

    let attempts = attempts.lock().expect("attempts");
    let failure_attempts = if transient { 3 } else { 1 };
    assert!(attempts[..failure_attempts]
        .windows(2)
        .all(|pair| pair[0] == pair[1]));
    let terminal = &attempts[failure_attempts];
    assert!(terminal.records.is_empty());
    assert_eq!(terminal.loss_ranges[0].first_sequence, 1);
    assert_eq!(terminal.loss_ranges[0].last_sequence, 1);
    assert_eq!(terminal.transition, Some(LogStreamTransition::Incomplete));
}

#[test]
fn drain_retries_new_provider_loss_as_a_loss_only_batch() {
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut store = MockStore::new();
    store.expect_append_batch().returning({
        let attempts = Arc::clone(&attempts);
        let calls = Arc::clone(&calls);
        move |batch| {
            attempts.lock().expect("attempts").push(batch);
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(LogStoreError::Conflict("permanent".to_string()))
            } else {
                Ok(())
            }
        }
    });
    let writer = writer(store, config(2, 2, LogOverflowPolicy::DropNewest));
    writer.enqueue(event("one"), 1, None).expect("enqueue");
    writer
        .drain(Instant::now() + Duration::from_secs(1))
        .expect("drain");
    writer
        .shutdown(Instant::now() + Duration::from_secs(1))
        .expect("shutdown");

    let attempts = attempts.lock().expect("attempts");
    assert_eq!(attempts[0].records[0].stream_sequence, 1);
    assert!(attempts[1].records.is_empty());
    assert_eq!(attempts[1].loss_ranges[0].first_sequence, 1);
    assert_eq!(
        attempts[2].transition,
        Some(LogStreamTransition::Incomplete)
    );
}

#[test]
fn oversized_nested_trace_and_diagnostics_stay_within_all_limits() {
    let context = context();
    let stream = LogStreamMetadata {
        stream_id: ryvus_logging::LogStreamId::new(
            context.execution_scope,
            RuntimeHostId::from("host"),
        ),
        action_key_id: context.action_key_id,
        action_revision: context.action_revision,
        runtime_language: context.runtime_language,
        started_at_unix_nanos: 1,
    };
    let mut log = event(&"é".repeat(1_000));
    log.trace_id = Some("t".repeat(100_000));
    log.span_id = Some("s".repeat(100_000));
    let nested = json!({"payload": "x".repeat(100_000)});
    log.fields = serde_json::Value::Object(
        (0..100)
            .map(|index| (format!("key-{index}-{}", "k".repeat(100)), nested.clone()))
            .collect(),
    );
    let limits = LogNormalizationLimits {
        max_message_bytes: 32,
        max_attributes: 8,
        max_attribute_key_bytes: 40,
        max_attribute_value_bytes: 64,
        max_record_bytes: 700,
    };

    let record = normalize_log_event(log, 1, 1, None, &stream, &limits);

    assert!(record.message.len() <= limits.max_message_bytes);
    assert!(record.attributes.len() <= limits.max_attributes);
    assert!(record
        .attributes
        .keys()
        .all(|key| key.len() <= limits.max_attribute_key_bytes));
    assert!(record.attributes.values().all(|value| match value {
        AttributeValue::String(value) => value.len() <= limits.max_attribute_value_bytes,
        AttributeValue::StringArray(values) =>
            values.iter().map(|value| value.len() + 1).sum::<usize>()
                <= limits.max_attribute_value_bytes,
        _ => true,
    }));
    assert!(serde_json::to_vec(&record).expect("serialize").len() <= limits.max_record_bytes);
}

#[test]
fn writer_rejects_a_record_limit_smaller_than_immutable_context() {
    let mut writer_config = config(1, 1, LogOverflowPolicy::DropNewest);
    writer_config.normalization.max_record_bytes = 1;
    let store = MockStore::new();
    assert!(matches!(
        RuntimeLogWriter::new(
            Arc::new(store),
            context(),
            RuntimeHostId::from("host"),
            1,
            writer_config,
            None,
        ),
        Err(RuntimeLogWriterError::InvalidConfiguration(_))
    ));
}

#[test]
fn smallest_practical_record_limit_still_bounds_accepted_records() {
    let batches = Arc::new(Mutex::new(Vec::new()));
    let mut store = MockStore::new();
    store.expect_append_batch().returning({
        let batches = Arc::clone(&batches);
        move |batch| {
            batches.lock().expect("batches").push(batch);
            Ok(())
        }
    });
    let mut writer_config = config(1, 1, LogOverflowPolicy::DropNewest);
    writer_config.normalization = LogNormalizationLimits {
        max_message_bytes: 8,
        max_attributes: 3,
        max_attribute_key_bytes: 40,
        max_attribute_value_bytes: 16,
        max_record_bytes: 1_300,
    };
    let writer = writer(store, writer_config.clone());
    let mut oversized = event(&"m".repeat(10_000));
    oversized.trace_id = Some("00112233445566778899aabbccddeeff".to_string());
    oversized.span_id = Some("0011223344556677".to_string());
    oversized.fields = json!({"nested": {"payload": "x".repeat(10_000)}});
    writer
        .enqueue(
            oversized,
            1,
            Some(ryvus_protocol::RuntimeSessionId::from("session")),
        )
        .expect("enqueue");
    writer
        .shutdown(Instant::now() + Duration::from_secs(1))
        .expect("shutdown");

    let batches = batches.lock().expect("batches");
    let records: Vec<_> = batches.iter().flat_map(|batch| &batch.records).collect();
    assert!(records.iter().all(|record| {
        serde_json::to_vec(record).expect("serialize").len()
            <= writer_config.normalization.max_record_bytes
    }));
    assert!(records.iter().all(|record| record.correlation.is_some()
        && record.runtime_session_id.is_some()
        && record.trace_id.is_some()
        && record.span_id.is_some()));
}

#[test]
fn always_failing_loss_only_batch_keeps_one_identity_and_stops_at_deadline() {
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let mut store = MockStore::new();
    store.expect_append_batch().returning({
        let attempts = Arc::clone(&attempts);
        move |batch| {
            attempts.lock().expect("attempts").push(batch);
            Err(LogStoreError::Conflict("always unavailable".to_string()))
        }
    });
    let mut writer_config = config(2, 2, LogOverflowPolicy::DropNewest);
    writer_config.retry_max_attempts = 1;
    writer_config.retry_initial_backoff = Duration::from_millis(5);
    writer_config.retry_max_backoff = Duration::from_millis(5);
    writer_config.grace_period = Duration::from_millis(50);
    writer_config.cleanup_period = Duration::from_millis(50);
    let writer = writer(store, writer_config);
    writer.enqueue(event("final"), 1, None).expect("enqueue");

    let started = Instant::now();
    assert_eq!(
        writer.shutdown(Instant::now() + Duration::from_millis(150)),
        Err(RuntimeLogWriterError::CleanupTimeout)
    );
    assert!(started.elapsed() < Duration::from_millis(150));
    let count_after_join = attempts.lock().expect("attempts").len();
    thread::sleep(Duration::from_millis(20));
    let attempts = attempts.lock().expect("attempts");
    assert_eq!(attempts.len(), count_after_join);
    let loss_attempts = &attempts[1..];
    assert!(!loss_attempts.is_empty());
    assert!(loss_attempts.iter().all(|batch| batch.records.is_empty()));
    assert!(loss_attempts
        .iter()
        .all(|batch| batch.batch_id == loss_attempts[0].batch_id && batch == &loss_attempts[0]));
}

#[test]
fn oversized_homogeneous_and_late_mixed_arrays_take_the_bounded_string_path() {
    let context = context();
    let stream = LogStreamMetadata {
        stream_id: ryvus_logging::LogStreamId::new(
            context.execution_scope,
            RuntimeHostId::from("host"),
        ),
        action_key_id: context.action_key_id,
        action_revision: context.action_revision,
        runtime_language: context.runtime_language,
        started_at_unix_nanos: 1,
    };
    let mut late_mixed = vec![json!("value"); 100_000];
    late_mixed.push(json!(1));
    let mut log = event("arrays");
    log.fields = json!({
        "homogeneous": vec![true; 100_000],
        "late_mixed": late_mixed,
    });
    let limits = LogNormalizationLimits {
        max_attribute_value_bytes: 32,
        ..LogNormalizationLimits::default()
    };

    let record = normalize_log_event(log, 1, 1, None, &stream, &limits);

    for key in ["homogeneous", "late_mixed"] {
        let Some(AttributeValue::String(value)) = record.attributes.get(key) else {
            panic!("{key} should use bounded string normalization");
        };
        assert!(value.len() <= limits.max_attribute_value_bytes);
    }
}

#[test]
fn oversized_identity_is_rejected_before_sequence_assignment() {
    let mut store = MockStore::new();
    store.expect_append_batch().returning(|_| Ok(()));
    let writer = writer(store, config(1, 1, LogOverflowPolicy::DropNewest));
    let mut oversized = event("invalid identity");
    oversized.execution_id = ExecutionId::from("x".repeat(257));
    assert!(matches!(
        writer.enqueue(oversized, 1, None),
        Err(RuntimeLogWriterError::InvalidIdentity(_))
    ));
    assert_eq!(writer.enqueue(event("valid"), 2, None).expect("valid"), 1);
    writer
        .shutdown(Instant::now() + Duration::from_secs(1))
        .expect("shutdown");
}

#[test]
fn active_retry_observes_new_shutdown_deadline_and_accounts_exact_loss() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let mut store = MockStore::new();
    store.expect_append_batch().times(1).returning(move |_| {
        entered_tx.send(()).expect("entered");
        release_rx
            .lock()
            .expect("release lock")
            .recv()
            .expect("release");
        Err(LogStoreError::Unavailable)
    });
    let mut writer_config = config(1, 1, LogOverflowPolicy::DropNewest);
    writer_config.retry_max_attempts = 100;
    writer_config.retry_initial_backoff = Duration::from_secs(5);
    writer_config.retry_max_backoff = Duration::from_secs(5);
    writer_config.grace_period = Duration::from_millis(40);
    writer_config.cleanup_period = Duration::from_millis(40);
    let writer = Arc::new(writer(store, writer_config));
    writer.enqueue(event("active"), 1, None).expect("enqueue");
    entered_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("active provider attempt");

    let started = Instant::now();
    let release = thread::spawn(move || {
        thread::sleep(Duration::from_millis(5));
        release_tx.send(()).expect("release provider");
    });

    assert_eq!(
        writer.shutdown(Instant::now() + Duration::from_millis(120)),
        Err(RuntimeLogWriterError::CleanupTimeout)
    );
    release.join().expect("release thread");
    assert!(started.elapsed() < Duration::from_millis(120));
    assert_eq!(
        writer.writer_known_loss().expect("known loss"),
        vec![ryvus_logging::LogLossRange {
            first_sequence: 1,
            last_sequence: 1,
            cause: LogLossCause::ProviderFailure,
        }]
    );
}

#[test]
fn queue_notification_does_not_complete_transient_retry_backoff() {
    let (first_tx, first_rx) = mpsc::channel();
    let (second_tx, second_rx) = mpsc::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut store = MockStore::new();
    store.expect_append_batch().returning({
        let calls = Arc::clone(&calls);
        move |_| match calls.fetch_add(1, Ordering::SeqCst) {
            0 => {
                first_tx.send(()).expect("first attempt");
                Err(LogStoreError::Unavailable)
            }
            1 => {
                second_tx.send(Instant::now()).expect("second attempt");
                Ok(())
            }
            _ => Ok(()),
        }
    });
    let mut writer_config = config(2, 1, LogOverflowPolicy::DropNewest);
    writer_config.retry_max_attempts = 2;
    writer_config.retry_initial_backoff = Duration::from_millis(150);
    writer_config.retry_max_backoff = Duration::from_millis(150);
    let writer = writer(store, writer_config);
    let started = Instant::now();
    writer.enqueue(event("first"), 1, None).expect("first");
    first_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("first provider attempt");

    writer.enqueue(event("queued"), 2, None).expect("queued");
    assert!(second_rx.recv_timeout(Duration::from_millis(75)).is_err());
    let second_at = second_rx
        .recv_timeout(Duration::from_millis(200))
        .expect("second provider attempt after backoff");
    assert!(second_at.duration_since(started) >= Duration::from_millis(140));
    writer
        .shutdown(Instant::now() + Duration::from_secs(1))
        .expect("shutdown");
}
