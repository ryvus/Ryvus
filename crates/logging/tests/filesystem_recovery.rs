use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use ryvus_logging::{
    ExecutionLogCorrelation, ExecutionLogRecord, ExecutionLogStore, FilesystemExecutionLogStore,
    FilesystemLogStoreConfig, LogBatch, LogRecordQuery, LogStoreError, LogStreamCompleteness,
    LogStreamId, LogStreamMetadata, LogStreamQuery, LogStreamTransition,
};
use ryvus_protocol::{
    AttemptId, ExecutionId, ExecutionScopeId, LogLevel, RuntimeHostId, RuntimeKind,
};

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(name: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "ryvus-logging-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        Self(path)
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn scope(value: &str) -> ExecutionScopeId {
    ExecutionScopeId::new(value).expect("valid test scope")
}

fn batch(
    scope_name: &str,
    action: &str,
    revision: &str,
    host: &str,
    batch_id: &str,
    sequence: u64,
    transition: Option<LogStreamTransition>,
) -> LogBatch {
    let stream = LogStreamMetadata {
        stream_id: LogStreamId::new(scope(scope_name), RuntimeHostId::from(host)),
        action_key_id: action.to_string(),
        action_revision: revision.to_string(),
        runtime_language: RuntimeKind::Rust,
        started_at_unix_nanos: i64::try_from(sequence).expect("small sequence"),
    };
    LogBatch {
        records: vec![ExecutionLogRecord {
            timestamp_unix_nanos: i64::try_from(sequence).expect("small sequence"),
            observed_timestamp_unix_nanos: i64::try_from(sequence).expect("small sequence"),
            stream_sequence: sequence,
            stream_id: stream.stream_id.clone(),
            action_key_id: stream.action_key_id.clone(),
            action_revision: stream.action_revision.clone(),
            runtime_language: stream.runtime_language.clone(),
            runtime_session_id: None,
            correlation: Some(
                ExecutionLogCorrelation::new(
                    ExecutionId::from("execution"),
                    AttemptId::from("attempt"),
                    1,
                )
                .expect("valid correlation"),
            ),
            severity: LogLevel::Info,
            message: "message".to_string(),
            attributes: BTreeMap::new(),
            trace_id: None,
            span_id: None,
        }],
        stream,
        batch_id: batch_id.to_string(),
        loss_ranges: Vec::new(),
        transition,
    }
}

fn config(root: &Path) -> FilesystemLogStoreConfig {
    FilesystemLogStoreConfig {
        root: root.to_path_buf(),
        ..FilesystemLogStoreConfig::default()
    }
}

fn stream_query(scope_name: &str) -> LogStreamQuery {
    LogStreamQuery {
        execution_scope: scope(scope_name),
        action_key_id: Some("action".to_string()),
        action_revision: Some("revision".to_string()),
        runtime_host_id: None,
        execution_id: Some(ExecutionId::from("execution")),
        attempt_id: Some(AttemptId::from("attempt")),
        cursor: None,
        limit: 10,
    }
}

fn record_query(stream_id: LogStreamId) -> LogRecordQuery {
    LogRecordQuery {
        stream_id,
        execution_id: Some(ExecutionId::from("execution")),
        attempt_id: Some(AttemptId::from("attempt")),
        cursor: None,
        limit: 10,
    }
}

#[test]
fn restart_replays_committed_batches_and_rebuilds_queries() {
    let root = TempRoot::new("restart");
    let committed = batch(
        "scope",
        "action",
        "revision",
        "host",
        "batch",
        1,
        Some(LogStreamTransition::Complete),
    );
    let store = FilesystemExecutionLogStore::new(config(&root.0)).expect("store");
    store
        .append_batch(committed.clone())
        .expect("committed batch");
    let expected_streams = store
        .list_streams(stream_query("scope"))
        .expect("stream query");
    let expected_records = store
        .list_records(record_query(committed.stream.stream_id.clone()))
        .expect("record query");
    drop(store);

    let recovered = FilesystemExecutionLogStore::new(config(&root.0)).expect("recovered store");
    assert_eq!(
        recovered
            .list_streams(stream_query("scope"))
            .expect("rebuilt stream query"),
        expected_streams
    );
    assert_eq!(
        recovered
            .list_records(record_query(committed.stream.stream_id.clone()))
            .expect("rebuilt record query"),
        expected_records
    );
    recovered
        .append_batch(committed.clone())
        .expect("identical replay after restart");
    assert_eq!(
        fs::read(ndjson_files(&root.0).pop().expect("stream file"))
            .expect("read stream file")
            .iter()
            .filter(|byte| **byte == b'\n')
            .count(),
        1
    );
    assert_eq!(
        recovered
            .list_records(record_query(committed.stream.stream_id))
            .expect("records after replay")
            .records
            .len(),
        1
    );
}

#[test]
fn recovery_projects_only_unterminated_streams_as_unknown() {
    for (name, transition, expected) in [
        ("opened", None, LogStreamCompleteness::Unknown),
        (
            "active",
            Some(LogStreamTransition::Active),
            LogStreamCompleteness::Unknown,
        ),
        (
            "complete",
            Some(LogStreamTransition::Complete),
            LogStreamCompleteness::Complete,
        ),
    ] {
        let root = TempRoot::new(name);
        let committed = batch(
            "scope", "action", "revision", "host", "batch", 1, transition,
        );
        let store = FilesystemExecutionLogStore::new(config(&root.0)).expect("store");
        store.append_batch(committed.clone()).expect("append");
        drop(store);

        let recovered = FilesystemExecutionLogStore::new(config(&root.0)).expect("recover");
        let summary = recovered
            .list_streams(stream_query("scope"))
            .expect("streams")
            .streams
            .pop()
            .expect("stream");
        assert_eq!(summary.completeness, expected, "{name}");

        if transition.is_none() {
            let mut continued = committed;
            continued.batch_id = "continued".into();
            continued.records[0].stream_sequence = 2;
            continued.records[0].timestamp_unix_nanos = 2;
            continued.records[0].observed_timestamp_unix_nanos = 2;
            recovered.append_batch(continued).expect("continue stream");
            let live = recovered
                .list_streams(stream_query("scope"))
                .expect("live streams")
                .streams
                .pop()
                .expect("live stream");
            assert_eq!(live.completeness, LogStreamCompleteness::Active);
        }
    }
}

#[test]
fn recovery_ignores_only_an_unterminated_final_fragment() {
    let root = TempRoot::new("fragment");
    let store = FilesystemExecutionLogStore::new(config(&root.0)).expect("store");
    let committed = batch("scope", "action", "revision", "host", "batch", 1, None);
    store
        .append_batch(committed.clone())
        .expect("committed batch");
    drop(store);

    let path = ndjson_files(&root.0).pop().expect("stream file");
    OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("open stream")
        .write_all(br#"{"stream":"truncated""#)
        .expect("write fragment");
    let recovered = FilesystemExecutionLogStore::new(config(&root.0)).expect("ignore fragment");
    let mut continued = committed.clone();
    continued.batch_id = "continued".to_string();
    continued.records[0].stream_sequence = 2;
    continued.records[0].timestamp_unix_nanos = 2;
    continued.records[0].observed_timestamp_unix_nanos = 2;
    recovered
        .append_batch(continued)
        .expect("append after repaired fragment");
    assert_eq!(
        recovered
            .list_records(record_query(committed.stream.stream_id.clone()))
            .expect("records")
            .records
            .len(),
        2
    );
    drop(recovered);

    let restarted =
        FilesystemExecutionLogStore::new(config(&root.0)).expect("restart after append");
    assert_eq!(
        restarted
            .list_records(record_query(committed.stream.stream_id))
            .expect("restarted records")
            .records
            .len(),
        2
    );
    drop(restarted);

    OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open stream")
        .write_all(b"\n")
        .expect("commit corruption");
    assert!(matches!(
        FilesystemExecutionLogStore::new(config(&root.0)),
        Err(LogStoreError::Corruption)
    ));
}

#[test]
fn recovery_rejects_an_envelope_outside_its_canonical_path() {
    let root = TempRoot::new("wrong-path");
    let store = FilesystemExecutionLogStore::new(config(&root.0)).expect("store");
    store
        .append_batch(batch(
            "scope", "action", "revision", "host", "batch", 1, None,
        ))
        .expect("append");
    drop(store);

    let canonical = ndjson_files(&root.0).pop().expect("canonical file");
    let wrong = root.0.join("wrong.ndjson");
    fs::copy(canonical, wrong).expect("copy envelope to wrong path");
    assert!(matches!(
        FilesystemExecutionLogStore::new(config(&root.0)),
        Err(LogStoreError::Corruption)
    ));
}

#[test]
fn recovery_rejects_a_stream_split_across_files() {
    let root = TempRoot::new("split-stream");
    let first = batch("scope", "action", "revision", "host", "first", 1, None);
    let store = FilesystemExecutionLogStore::new(config(&root.0)).expect("store");
    store.append_batch(first.clone()).expect("first batch");
    drop(store);

    let mut second = first;
    second.batch_id = "second".to_string();
    second.records[0].stream_sequence = 2;
    second.records[0].timestamp_unix_nanos = 2;
    second.records[0].observed_timestamp_unix_nanos = 2;
    let split_path = root
        .0
        .join(hex("scope"))
        .join("actions")
        .join(hex("action"))
        .join(format!("{}.ndjson", hex("zzzz")));
    let mut encoded = serde_json::to_vec(&second).expect("serialize second batch");
    encoded.push(b'\n');
    fs::write(split_path, encoded).expect("write split stream file");

    assert!(matches!(
        FilesystemExecutionLogStore::new(config(&root.0)),
        Err(LogStoreError::Corruption)
    ));
}

#[test]
fn provider_errors_are_stable_and_do_not_expose_backend_details() {
    let root = TempRoot::new("errors");
    fs::write(&root.0, b"not a directory").expect("create invalid root");
    let error = match FilesystemExecutionLogStore::new(config(&root.0)) {
        Err(error) => error,
        Ok(_) => panic!("file root must fail"),
    };
    assert_eq!(error, LogStoreError::Io);
    assert_eq!(error.to_string(), "log store I/O failed");
    assert!(!error
        .to_string()
        .contains(root.0.to_string_lossy().as_ref()));

    fs::remove_file(&root.0).expect("remove invalid root");
    fs::create_dir_all(&root.0).expect("create root");
    fs::write(root.0.join("bad.ndjson"), b"not json\n").expect("write corruption");
    let corruption = match FilesystemExecutionLogStore::new(config(&root.0)) {
        Err(error) => error,
        Ok(_) => panic!("corruption must fail"),
    };
    assert_eq!(corruption, LogStoreError::Corruption);
    assert_eq!(corruption.to_string(), "log store is corrupt");
}

#[test]
fn paths_hex_encode_identifiers_and_omit_revision() {
    let root = TempRoot::new("paths");
    let store = FilesystemExecutionLogStore::new(config(&root.0)).expect("store");
    store
        .append_batch(batch(
            "../scope",
            "../../action",
            "revision-must-not-be-a-path",
            "../host",
            "batch",
            1,
            None,
        ))
        .expect("append traversal-like identifiers");

    let relative = ndjson_files(&root.0)
        .pop()
        .expect("stream file")
        .strip_prefix(&root.0)
        .expect("relative path")
        .to_path_buf();
    assert_eq!(
        relative,
        PathBuf::from(hex("../scope"))
            .join("actions")
            .join(hex("../../action"))
            .join(format!("{}.ndjson", hex("../host")))
    );
    assert!(!relative
        .to_string_lossy()
        .contains("revision-must-not-be-a-path"));
}

#[test]
fn serialized_batches_are_bounded() {
    let root = TempRoot::new("bounded");
    assert_eq!(
        FilesystemLogStoreConfig::default().max_batch_bytes,
        1024 * 1024
    );
    let store = FilesystemExecutionLogStore::new(FilesystemLogStoreConfig {
        root: root.0.clone(),
        max_batch_bytes: 64,
    })
    .expect("store");
    assert!(matches!(
        store.append_batch(batch(
            "scope", "action", "revision", "host", "batch", 1, None
        )),
        Err(LogStoreError::InvalidBatch(_))
    ));
    assert!(ndjson_files(&root.0).is_empty());
}

fn ndjson_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_files(root, &mut files);
    files
}

fn collect_files(root: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(root).expect("read test directory") {
        let entry = entry.expect("directory entry");
        if entry.file_type().expect("file type").is_dir() {
            collect_files(&entry.path(), files);
        } else if entry
            .path()
            .extension()
            .is_some_and(|value| value == "ndjson")
        {
            files.push(entry.path());
        }
    }
}

fn hex(value: &str) -> String {
    value.bytes().map(|byte| format!("{byte:02x}")).collect()
}
