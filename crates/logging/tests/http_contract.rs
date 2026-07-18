use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::SystemTime,
};

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    Router,
};
use ryvus_logging::{
    http::log_history_routes, ExecutionLogCorrelation, ExecutionLogRecord, ExecutionLogStore,
    FilesystemExecutionLogStore, FilesystemLogStoreConfig, InMemoryExecutionLogStore, LogBatch,
    LogLossCause, LogLossRange, LogProjectedRecordPage, LogProjectedRecordQuery, LogRecordPage,
    LogRecordQuery, LogStoreError, LogStreamId, LogStreamMetadata, LogStreamPage, LogStreamQuery,
    LogStreamTransition,
};
use ryvus_protocol::{
    AttemptId, ExecutionId, ExecutionScopeId, LogLevel, RuntimeHostId, RuntimeKind,
};
use serde_json::Value;
use tower::ServiceExt;

fn scope(value: &str) -> ExecutionScopeId {
    ExecutionScopeId::new(value).expect("valid scope")
}

fn stream(
    scope_name: &str,
    host: &str,
    action: &str,
    revision: &str,
    started: i64,
) -> LogStreamMetadata {
    LogStreamMetadata {
        stream_id: LogStreamId::new(scope(scope_name), RuntimeHostId::from(host)),
        action_key_id: action.into(),
        action_revision: revision.into(),
        runtime_language: RuntimeKind::Rust,
        started_at_unix_nanos: started,
    }
}

fn append(store: &dyn ExecutionLogStore, stream: &LogStreamMetadata, sequences: &[u64]) {
    store
        .append_batch(LogBatch {
            stream: stream.clone(),
            batch_id: format!(
                "batch-{}-{}",
                stream.stream_id.execution_scope, stream.started_at_unix_nanos
            ),
            records: sequences
                .iter()
                .map(|sequence| ExecutionLogRecord {
                    timestamp_unix_nanos: i64::try_from(*sequence).expect("small sequence"),
                    observed_timestamp_unix_nanos: i64::try_from(*sequence)
                        .expect("small sequence"),
                    stream_sequence: *sequence,
                    stream_id: stream.stream_id.clone(),
                    action_key_id: stream.action_key_id.clone(),
                    action_revision: stream.action_revision.clone(),
                    runtime_language: stream.runtime_language.clone(),
                    runtime_session_id: None,
                    correlation: Some(
                        ExecutionLogCorrelation::new(
                            ExecutionId::from("execution-1"),
                            AttemptId::from("attempt-1"),
                            1,
                        )
                        .expect("valid correlation"),
                    ),
                    severity: LogLevel::Info,
                    message: format!("record {sequence}"),
                    attributes: BTreeMap::new(),
                    trace_id: None,
                    span_id: None,
                })
                .collect(),
            loss_ranges: Vec::new(),
            transition: None,
        })
        .expect("append logs");
}

async fn get(app: Router, uri: &str) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .uri(uri)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (status, serde_json::from_slice(&body).expect("json"))
}

async fn run_contract(store: Arc<dyn ExecutionLogStore>) {
    let older = stream("scope-a", "host-a", "inventory", "r1", 1);
    let newer = stream("scope-a", "host-b", "orders", "r2", 2);
    let projected = stream("scope-a", "host-d", "inventory", "r1", 0);
    let foreign = stream("scope-b", "host-a", "inventory", "r1", 3);
    let foreign_older = stream("scope-b", "host-c", "inventory", "r1", 2);
    append(store.as_ref(), &older, &[1, 2]);
    append(store.as_ref(), &newer, &[1]);
    append(store.as_ref(), &projected, &[1]);
    append(store.as_ref(), &foreign, &[1, 2]);
    append(store.as_ref(), &foreign_older, &[1]);

    let app = log_history_routes(store.clone(), scope("scope-a"));
    let (status, first) = get(app.clone(), "/internal/logs/streams?limit=1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first["streams"][0]["runtime_host_id"], "host-b");
    assert_eq!(first["streams"][0]["started_at_unix_nanos"], "2");
    assert_eq!(first["streams"][0]["persisted_record_count"], "1");
    let cursor = first["next_cursor"].as_str().expect("stream cursor");
    assert_noncanonical_cursors(app.clone(), "/internal/logs/streams?cursor=", cursor).await;
    let (status, second) = get(
        app.clone(),
        &format!("/internal/logs/streams?limit=1&cursor={cursor}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(second["streams"][0]["runtime_host_id"], "host-a");

    let filters = "/internal/logs/streams?action_key_id=inventory&action_revision=r1&runtime_host_id=host-a&execution_id=execution-1&attempt_id=attempt-1";
    let (status, filtered) = get(app.clone(), filters).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(filtered["streams"].as_array().expect("streams").len(), 1);

    let projected_uri =
        "/internal/logs/projected-records?action_key_id=inventory&action_revision=r1&limit=2";
    let (status, projected_page) = get(app.clone(), projected_uri).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        projected_page["records"].as_array().expect("records").len(),
        2
    );
    assert_eq!(projected_page["records"][0]["runtime_host_id"], "host-d");
    assert_eq!(projected_page["records"][1]["stream_sequence"], "2");
    assert_eq!(projected_page["has_older"], true);
    let older_cursor = projected_page["older_cursor"]
        .as_str()
        .expect("older cursor");
    let (status, older_page) = get(
        app.clone(),
        &format!("{projected_uri}&older_cursor={older_cursor}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(older_page["records"][0]["stream_sequence"], "1");
    let (status, body) = get(
        app.clone(),
        &format!("/internal/logs/projected-records?action_key_id=inventory&action_revision=other&older_cursor={older_cursor}"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "log_invalid_cursor");
    let (status, body) = get(
        app.clone(),
        &format!("/internal/logs/projected-records?action_key_id=inventory&action_revision=r1&newer_cursor={older_cursor}"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "log_invalid_cursor");
    let (status, body) = get(
        log_history_routes(store.clone(), scope("scope-b")),
        &format!("{projected_uri}&older_cursor={older_cursor}"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "log_invalid_cursor");

    for uri in [
        "/internal/logs/projected-records?action_revision=r1",
        "/internal/logs/projected-records?action_key_id=inventory",
        "/internal/logs/projected-records?action_key_id=inventory&action_revision=r1&older_cursor=a&newer_cursor=b",
    ] {
        assert_eq!(get(app.clone(), uri).await.0, StatusCode::BAD_REQUEST);
    }

    let records = "/internal/logs/streams/host-a/records?limit=1&execution_id=execution-1&attempt_id=attempt-1";
    let (status, first_records) = get(app.clone(), records).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first_records["records"][0]["stream_sequence"], "1");
    assert_eq!(first_records["records"][0]["timestamp_unix_nanos"], "1");
    assert_eq!(first_records["records"][0]["runtime_host_id"], "host-a");
    let cursor = first_records["next_cursor"]
        .as_str()
        .expect("record cursor");
    assert_noncanonical_cursors(
        app.clone(),
        "/internal/logs/streams/host-a/records?cursor=",
        cursor,
    )
    .await;
    let (status, second_records) = get(
        app.clone(),
        &format!("/internal/logs/streams/host-a/records?limit=1&cursor={cursor}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(second_records["records"][0]["stream_sequence"], "2");

    let foreign_app = log_history_routes(store.clone(), scope("scope-b"));
    let (_, foreign_page) = get(foreign_app, "/internal/logs/streams?limit=1").await;
    let foreign_cursor = foreign_page["next_cursor"]
        .as_str()
        .unwrap_or_else(|| panic!("foreign stream requires another page for a cursor"));
    let (status, body) = get(
        app.clone(),
        &format!("/internal/logs/streams?cursor={foreign_cursor}"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "log_invalid_cursor");

    let (_, foreign_records) = get(
        log_history_routes(store.clone(), scope("scope-b")),
        "/internal/logs/streams/host-a/records?limit=1",
    )
    .await;
    let foreign_record_cursor = foreign_records["next_cursor"]
        .as_str()
        .expect("foreign record cursor");
    let (status, body) = get(
        app.clone(),
        &format!("/internal/logs/streams/host-a/records?cursor={foreign_record_cursor}"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "log_invalid_cursor");

    let (status, body) = get(app.clone(), "/internal/logs/streams?cursor=not-hex").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "log_invalid_cursor");
    for uri in [
        "/internal/logs/streams?limit=0",
        "/internal/logs/streams?limit=not-a-number",
    ] {
        let (status, body) = get(app.clone(), uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "log_invalid_query");
    }
    let (status, body) = get(app, "/internal/logs/streams/missing/records").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "log_stream_not_found");
}

async fn assert_noncanonical_cursors(app: Router, route: &str, canonical: &str) {
    for (name, cursor) in noncanonical_cursors(canonical) {
        let (status, body) = get(app.clone(), &format!("{route}{cursor}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name}");
        assert_eq!(body["error"], "log_invalid_cursor", "{name}");
    }
}

fn noncanonical_cursors(canonical: &str) -> Vec<(&'static str, String)> {
    let decoded = decode_hex(canonical);
    let json = String::from_utf8(decoded.clone()).expect("cursor JSON");
    let mut unknown_field: Value = serde_json::from_slice(&decoded).expect("cursor value");
    unknown_field
        .as_object_mut()
        .expect("cursor object")
        .insert("unknown".into(), Value::Bool(true));
    let mut unsupported_version: Value = serde_json::from_slice(&decoded).expect("cursor value");
    unsupported_version["version"] = Value::from(2);

    vec![
        ("uppercase hex", canonical.to_ascii_uppercase()),
        (
            "reformatted JSON",
            encode_hex(format!(" {json}").as_bytes()),
        ),
        (
            "unknown field",
            encode_hex(&serde_json::to_vec(&unknown_field).expect("unknown-field cursor")),
        ),
        (
            "unsupported version",
            encode_hex(
                &serde_json::to_vec(&unsupported_version).expect("unsupported-version cursor"),
            ),
        ),
        ("token beyond bound", "00".repeat(4_097)),
    ]
}

fn decode_hex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).expect("hex pair");
            u8::from_str_radix(pair, 16).expect("valid cursor hex")
        })
        .collect()
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

#[tokio::test]
async fn memory_http_contract() {
    run_contract(Arc::new(InMemoryExecutionLogStore::default())).await;
}

#[tokio::test]
async fn filesystem_http_contract() {
    let temp = TempDir::new("http-contract");
    let store = FilesystemExecutionLogStore::new(FilesystemLogStoreConfig {
        root: temp.0.clone(),
        ..FilesystemLogStoreConfig::default()
    })
    .expect("filesystem store");
    run_contract(Arc::new(store)).await;
}

#[tokio::test]
async fn recovered_unterminated_filesystem_stream_is_unknown_over_http() {
    let temp = TempDir::new("http-recovered-unknown");
    let metadata = stream("scope-a", "host-recovered", "inventory", "r1", 1);
    let store = FilesystemExecutionLogStore::new(FilesystemLogStoreConfig {
        root: temp.0.clone(),
        ..FilesystemLogStoreConfig::default()
    })
    .expect("filesystem store");
    append(&store, &metadata, &[1]);
    drop(store);

    let recovered = Arc::new(
        FilesystemExecutionLogStore::new(FilesystemLogStoreConfig {
            root: temp.0.clone(),
            ..FilesystemLogStoreConfig::default()
        })
        .expect("recovered store"),
    );
    let (status, body) = get(
        log_history_routes(recovered, scope("scope-a")),
        "/internal/logs/streams",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["streams"][0]["completeness"], "unknown");
}

#[tokio::test]
async fn response_dtos_preserve_integer_precision_as_decimal_strings() {
    let store = Arc::new(InMemoryExecutionLogStore::default());
    let metadata = stream("scope-a", "host-precision", "inventory", "r1", i64::MAX);
    let attributes = BTreeMap::from([
        (
            "minimum".into(),
            ryvus_logging::AttributeValue::I64(i64::MIN),
        ),
        (
            "maximum".into(),
            ryvus_logging::AttributeValue::I64(i64::MAX),
        ),
        (
            "bounds".into(),
            ryvus_logging::AttributeValue::I64Array(vec![i64::MIN, i64::MAX]),
        ),
        ("ratio".into(), ryvus_logging::AttributeValue::F64(1.5)),
    ]);
    store
        .append_batch(LogBatch {
            stream: metadata.clone(),
            batch_id: "precision".into(),
            records: vec![ExecutionLogRecord {
                timestamp_unix_nanos: i64::MIN,
                observed_timestamp_unix_nanos: i64::MAX,
                stream_sequence: u64::MAX,
                stream_id: metadata.stream_id.clone(),
                action_key_id: metadata.action_key_id.clone(),
                action_revision: metadata.action_revision.clone(),
                runtime_language: metadata.runtime_language.clone(),
                runtime_session_id: None,
                correlation: None,
                severity: LogLevel::Info,
                message: "precision".into(),
                attributes,
                trace_id: None,
                span_id: None,
            }],
            loss_ranges: vec![LogLossRange {
                first_sequence: 1,
                last_sequence: u64::MAX - 1,
                cause: LogLossCause::IngestionOverflow,
            }],
            transition: Some(LogStreamTransition::Incomplete),
        })
        .expect("append precision fixture");

    let app = log_history_routes(store, scope("scope-a"));
    let (status, streams) = get(app.clone(), "/internal/logs/streams").await;
    assert_eq!(status, StatusCode::OK);
    let summary = &streams["streams"][0];
    assert_eq!(summary["started_at_unix_nanos"], i64::MAX.to_string());
    assert_eq!(summary["last_sequence"], u64::MAX.to_string());
    assert_eq!(
        summary["ingestion_dropped_count"],
        (u64::MAX - 1).to_string()
    );
    assert_eq!(
        summary["loss_ranges"][0]["last_sequence"],
        (u64::MAX - 1).to_string()
    );

    let (status, records) = get(app, "/internal/logs/streams/host-precision/records").await;
    assert_eq!(status, StatusCode::OK);
    let record = &records["records"][0];
    assert_eq!(record["stream_sequence"], u64::MAX.to_string());
    assert_eq!(record["timestamp_unix_nanos"], i64::MIN.to_string());
    assert_eq!(
        record["observed_timestamp_unix_nanos"],
        i64::MAX.to_string()
    );
    assert_eq!(
        record["attributes"]["minimum"]["value"],
        i64::MIN.to_string()
    );
    assert_eq!(
        record["attributes"]["maximum"]["value"],
        i64::MAX.to_string()
    );
    assert_eq!(
        record["attributes"]["bounds"]["value"],
        serde_json::json!([i64::MIN.to_string(), i64::MAX.to_string()])
    );
    assert_eq!(record["attributes"]["ratio"]["value"], 1.5);
}

struct FailingStore(LogStoreError);

impl ExecutionLogStore for FailingStore {
    fn append_batch(&self, _: LogBatch) -> Result<(), LogStoreError> {
        Err(LogStoreError::Unavailable)
    }

    fn list_streams(&self, _: LogStreamQuery) -> Result<LogStreamPage, LogStoreError> {
        Err(if matches!(&self.0, LogStoreError::Corruption) {
            LogStoreError::Corruption
        } else {
            LogStoreError::Io
        })
    }

    fn list_records(&self, _: LogRecordQuery) -> Result<LogRecordPage, LogStoreError> {
        Err(LogStoreError::Corruption)
    }

    fn list_projected_records(
        &self,
        _: LogProjectedRecordQuery,
    ) -> Result<LogProjectedRecordPage, LogStoreError> {
        Err(LogStoreError::Corruption)
    }
}

#[tokio::test]
async fn provider_errors_are_stable_and_safe() {
    for error in [LogStoreError::Io, LogStoreError::Corruption] {
        let app = log_history_routes(Arc::new(FailingStore(error)), scope("scope-a"));
        let (status, body) = get(app.clone(), "/internal/logs/streams").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"], "log_provider_unavailable");
        assert!(!body.to_string().contains("corrupt"));
        assert!(!body.to_string().contains("I/O"));

        let (status, body) = get(
            app.clone(),
            "/internal/logs/streams/host-a/records?severity=error&search=database",
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"], "log_provider_unavailable");
        assert!(!body.to_string().contains("corrupt"));
        assert!(!body.to_string().contains("I/O"));

        let (status, body) = get(
            app,
            "/internal/logs/projected-records?action_key_id=inventory&action_revision=r1",
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"], "log_provider_unavailable");
    }
}

#[derive(Default)]
struct CapturingStore {
    stream_queries: Mutex<Vec<LogStreamQuery>>,
    record_queries: Mutex<Vec<LogRecordQuery>>,
    projected_queries: Mutex<Vec<LogProjectedRecordQuery>>,
}

impl ExecutionLogStore for CapturingStore {
    fn append_batch(&self, _: LogBatch) -> Result<(), LogStoreError> {
        Ok(())
    }

    fn list_streams(&self, query: LogStreamQuery) -> Result<LogStreamPage, LogStoreError> {
        self.stream_queries
            .lock()
            .expect("stream query lock")
            .push(query);
        Ok(LogStreamPage {
            streams: Vec::new(),
            next_cursor: None,
        })
    }

    fn list_records(&self, query: LogRecordQuery) -> Result<LogRecordPage, LogStoreError> {
        self.record_queries
            .lock()
            .expect("record query lock")
            .push(query);
        Ok(LogRecordPage {
            records: Vec::new(),
            next_cursor: None,
        })
    }

    fn list_projected_records(
        &self,
        query: LogProjectedRecordQuery,
    ) -> Result<LogProjectedRecordPage, LogStoreError> {
        self.projected_queries
            .lock()
            .expect("projected query lock")
            .push(query);
        Ok(LogProjectedRecordPage {
            records: Vec::new(),
            older_cursor: None,
            newer_cursor: None,
            has_older: false,
            has_newer: false,
        })
    }
}

#[tokio::test]
async fn severity_and_search_are_validated_and_forwarded() {
    let store = Arc::new(CapturingStore::default());
    let app = log_history_routes(store.clone(), scope("scope-a"));

    let (status, _) = get(
        app.clone(),
        "/internal/logs/streams?severity=error&search=DATABASE",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = get(
        app.clone(),
        "/internal/logs/streams/host-a/records?severity=error&search=DATABASE",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = get(
        app.clone(),
        "/internal/logs/projected-records?action_key_id=inventory&action_revision=r1&severity=error&search=DATABASE",
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    {
        let stream_queries = store.stream_queries.lock().expect("stream queries");
        assert_eq!(stream_queries[0].severity, Some(LogLevel::Error));
        assert_eq!(
            stream_queries[0].message_contains.as_deref(),
            Some("database")
        );
        let record_queries = store.record_queries.lock().expect("record queries");
        assert_eq!(record_queries[0].severity, Some(LogLevel::Error));
        assert_eq!(
            record_queries[0].message_contains.as_deref(),
            Some("database")
        );
        let projected_queries = store.projected_queries.lock().expect("projected queries");
        assert_eq!(projected_queries[0].severity, Some(LogLevel::Error));
        assert_eq!(
            projected_queries[0].message_contains.as_deref(),
            Some("database")
        );
    }

    for uri in [
        "/internal/logs/streams?severity=critical",
        "/internal/logs/streams?search=%20",
    ] {
        let (status, body) = get(app.clone(), uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "log_invalid_query");
    }
    let oversized = "a".repeat(257);
    let (status, body) = get(
        app,
        &format!("/internal/logs/streams/host-a/records?search={oversized}"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "log_invalid_query");
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("ryvus-logging-{name}-{nanos}"));
        std::fs::create_dir_all(&path).expect("temp dir");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
