use std::{
    collections::{BTreeMap, VecDeque},
    io::{self, Write},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{SyncSender, TrySendError},
        Arc, Condvar, Mutex, MutexGuard,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use ryvus_logging::{
    normalize_loss_ranges, AttributeValue, ExecutionLogCorrelation, ExecutionLogRecord,
    ExecutionLogStore, LogBatch, LogLossCause, LogLossRange, LogStoreError, LogStreamMetadata,
    LogStreamTransition, RuntimeLogContext, SpanId, TraceId,
};
use ryvus_protocol::{AttemptId, ExecutionId, LogEvent, LogLevel, RuntimeHostId, RuntimeSessionId};
use serde_json::Value;
use thiserror::Error;

const DIAGNOSTIC_SLOTS: usize = 3;
const WARNING_KEY: &str = "ryvus.log.normalization.warning";
const INVALID_TRACE_KEY: &str = "ryvus.log.invalid_trace_context";
const STRINGIFIED_KEY: &str = "ryvus.log.stringified_attributes";
const MAX_LOG_IDENTITY_BYTES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogOverflowPolicy {
    DropNewest,
    DropOldest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogNormalizationLimits {
    pub max_message_bytes: usize,
    pub max_attributes: usize,
    pub max_attribute_key_bytes: usize,
    pub max_attribute_value_bytes: usize,
    pub max_record_bytes: usize,
}

impl Default for LogNormalizationLimits {
    fn default() -> Self {
        Self {
            max_message_bytes: 64 * 1024,
            max_attributes: 128,
            max_attribute_key_bytes: 256,
            max_attribute_value_bytes: 64 * 1024,
            max_record_bytes: 256 * 1024,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeLogWriterConfig {
    pub capacity: usize,
    pub batch_size: usize,
    pub flush_interval: Duration,
    pub retry_max_attempts: u32,
    pub retry_initial_backoff: Duration,
    pub retry_max_backoff: Duration,
    pub overflow_policy: LogOverflowPolicy,
    pub grace_period: Duration,
    pub cleanup_period: Duration,
    pub normalization: LogNormalizationLimits,
}

impl Default for RuntimeLogWriterConfig {
    fn default() -> Self {
        Self {
            capacity: 1024,
            batch_size: 64,
            flush_interval: Duration::from_millis(250),
            retry_max_attempts: 3,
            retry_initial_backoff: Duration::from_millis(25),
            retry_max_backoff: Duration::from_millis(250),
            overflow_policy: LogOverflowPolicy::DropNewest,
            grace_period: Duration::from_secs(3),
            cleanup_period: Duration::from_secs(1),
            normalization: LogNormalizationLimits::default(),
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RuntimeLogWriterError {
    #[error("invalid runtime log writer configuration: {0}")]
    InvalidConfiguration(&'static str),
    #[error("runtime log writer is not accepting records")]
    AdmissionClosed,
    #[error("runtime log stream sequence overflowed")]
    SequenceOverflow,
    #[error("invalid runtime log identity: {0}")]
    InvalidIdentity(&'static str),
    #[error("normalized runtime log cannot fit the configured record limit")]
    RecordTooLarge,
    #[error("runtime log writer synchronization is unavailable")]
    Synchronization,
    #[error("runtime log writer deadline expired")]
    DeadlineExpired,
    #[error("runtime log writer cleanup timed out")]
    CleanupTimeout,
    #[error("runtime log writer thread could not start")]
    ThreadStart,
}

pub struct RuntimeLogWriter {
    shared: Arc<Shared>,
    admission: Mutex<()>,
    sequence: AtomicU64,
    config: RuntimeLogWriterConfig,
    stream: LogStreamMetadata,
    handle: Mutex<Option<JoinHandle<()>>>,
}

struct Shared {
    state: Mutex<WriterState>,
    wake: Condvar,
    changed: Condvar,
}

struct WriterState {
    queue: VecDeque<ExecutionLogRecord>,
    known_loss: Vec<LogLossRange>,
    pending_loss: Vec<LogLossRange>,
    accepting: bool,
    draining: bool,
    flush_requested: bool,
    active: bool,
    shutdown: Option<Instant>,
    drain_deadline: Option<Instant>,
    terminal_accepted: bool,
    stopped: bool,
}

impl RuntimeLogWriter {
    pub fn new(
        store: Arc<dyn ExecutionLogStore>,
        context: RuntimeLogContext,
        runtime_host_id: RuntimeHostId,
        started_at_unix_nanos: i64,
        config: RuntimeLogWriterConfig,
        console: Option<SyncSender<ExecutionLogRecord>>,
    ) -> Result<Self, RuntimeLogWriterError> {
        let stream = LogStreamMetadata {
            stream_id: ryvus_logging::LogStreamId::new(
                context.execution_scope.clone(),
                runtime_host_id,
            ),
            action_key_id: context.action_key_id,
            action_revision: context.action_revision,
            runtime_language: context.runtime_language,
            started_at_unix_nanos,
        };
        validate_config(&config, &stream)?;
        let shared = Arc::new(Shared {
            state: Mutex::new(WriterState {
                queue: VecDeque::with_capacity(config.capacity),
                known_loss: Vec::new(),
                pending_loss: Vec::new(),
                accepting: true,
                draining: false,
                flush_requested: false,
                active: false,
                shutdown: None,
                drain_deadline: None,
                terminal_accepted: false,
                stopped: false,
            }),
            wake: Condvar::new(),
            changed: Condvar::new(),
        });
        let worker_shared = Arc::clone(&shared);
        let worker_config = config.clone();
        let worker_stream = stream.clone();
        let handle = thread::Builder::new()
            .name("ryvus-runtime-log-writer".to_string())
            .spawn(move || writer_loop(worker_shared, store, worker_stream, worker_config, console))
            .map_err(|_| RuntimeLogWriterError::ThreadStart)?;
        Ok(Self {
            shared,
            admission: Mutex::new(()),
            sequence: AtomicU64::new(0),
            config,
            stream,
            handle: Mutex::new(Some(handle)),
        })
    }

    pub fn enqueue(
        &self,
        event: LogEvent,
        observed_timestamp_unix_nanos: i64,
        runtime_session_id: Option<RuntimeSessionId>,
    ) -> Result<u64, RuntimeLogWriterError> {
        validate_event_identity(&event, runtime_session_id.as_ref())?;
        self.enqueue_event(
            event,
            observed_timestamp_unix_nanos,
            runtime_session_id,
            true,
        )
    }

    pub fn enqueue_lifecycle(
        &self,
        level: LogLevel,
        message: impl Into<String>,
        fields: Value,
        observed_timestamp_unix_nanos: i64,
        runtime_session_id: Option<RuntimeSessionId>,
    ) -> Result<u64, RuntimeLogWriterError> {
        validate_session_identity(runtime_session_id.as_ref())?;
        self.enqueue_event(
            LogEvent {
                execution_id: ExecutionId::from("runtime"),
                attempt_id: AttemptId::from("lifecycle"),
                attempt_number: 1,
                timestamp_unix_nanos: Some(observed_timestamp_unix_nanos),
                trace_id: None,
                span_id: None,
                level,
                message: message.into(),
                fields,
            },
            observed_timestamp_unix_nanos,
            runtime_session_id,
            false,
        )
    }

    fn enqueue_event(
        &self,
        event: LogEvent,
        observed_timestamp_unix_nanos: i64,
        runtime_session_id: Option<RuntimeSessionId>,
        correlated: bool,
    ) -> Result<u64, RuntimeLogWriterError> {
        let _admission = lock(&self.admission)?;
        let sequence = self
            .sequence
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(1)
            })
            .map_err(|_| RuntimeLogWriterError::SequenceOverflow)?
            .checked_add(1)
            .ok_or(RuntimeLogWriterError::SequenceOverflow)?;
        let mut record = normalize_log_event(
            event,
            observed_timestamp_unix_nanos,
            sequence,
            runtime_session_id,
            &self.stream,
            &self.config.normalization,
        );
        if !correlated {
            record.correlation = None;
        }
        if exceeds_serialized_limit(&record, self.config.normalization.max_record_bytes) {
            return Err(RuntimeLogWriterError::RecordTooLarge);
        }
        let mut state = lock(&self.shared.state)?;
        if !state.accepting {
            return Err(RuntimeLogWriterError::AdmissionClosed);
        }
        if state.queue.len() == self.config.capacity {
            match self.config.overflow_policy {
                LogOverflowPolicy::DropNewest => {
                    record_loss(&mut state, sequence, LogLossCause::IngestionOverflow)?;
                    self.shared.wake.notify_one();
                    return Ok(sequence);
                }
                LogOverflowPolicy::DropOldest => {
                    if let Some(dropped) = state.queue.pop_front() {
                        record_loss(
                            &mut state,
                            dropped.stream_sequence,
                            LogLossCause::IngestionOverflow,
                        )?;
                    }
                }
            }
        }
        state.queue.push_back(record);
        self.shared.wake.notify_one();
        Ok(sequence)
    }

    pub fn writer_known_loss(&self) -> Result<Vec<LogLossRange>, RuntimeLogWriterError> {
        Ok(lock(&self.shared.state)?.known_loss.clone())
    }

    pub(crate) fn configured_shutdown_duration(&self) -> Duration {
        self.config
            .grace_period
            .saturating_add(self.config.cleanup_period)
    }

    pub fn drain(&self, deadline: Instant) -> Result<(), RuntimeLogWriterError> {
        {
            let mut state = lock(&self.shared.state)?;
            state.accepting = false;
            state.draining = true;
            state.drain_deadline = Some(deadline);
            state.flush_requested = true;
            self.shared.wake.notify_one();
        }
        self.wait_until(deadline, |state| {
            state.queue.is_empty() && state.pending_loss.is_empty() && !state.active
        })?;
        let mut state = lock(&self.shared.state)?;
        state.draining = false;
        state.drain_deadline = None;
        self.shared.wake.notify_one();
        Ok(())
    }

    pub fn shutdown(&self, deadline: Instant) -> Result<(), RuntimeLogWriterError> {
        let now = Instant::now();
        let flush_deadline = now
            .checked_add(self.config.grace_period)
            .unwrap_or(deadline)
            .min(deadline);
        {
            let mut state = lock(&self.shared.state)?;
            state.accepting = false;
            state.draining = true;
            state.drain_deadline = Some(flush_deadline);
            state.flush_requested = true;
            state.shutdown = Some(flush_deadline);
            self.shared.wake.notify_one();
        }
        let cleanup_deadline = flush_deadline
            .checked_add(self.config.cleanup_period)
            .unwrap_or(deadline)
            .min(deadline);
        self.wait_until(cleanup_deadline, |state| state.stopped)
            .map_err(|error| match error {
                RuntimeLogWriterError::DeadlineExpired => RuntimeLogWriterError::CleanupTimeout,
                other => other,
            })?;
        let terminal_accepted = lock(&self.shared.state)?.terminal_accepted;
        let handle = lock(&self.handle)?.take();
        if let Some(handle) = handle {
            handle
                .join()
                .map_err(|_| RuntimeLogWriterError::Synchronization)?;
        }
        if terminal_accepted {
            Ok(())
        } else {
            Err(RuntimeLogWriterError::CleanupTimeout)
        }
    }

    fn wait_until(
        &self,
        deadline: Instant,
        done: impl Fn(&WriterState) -> bool,
    ) -> Result<(), RuntimeLogWriterError> {
        let mut state = lock(&self.shared.state)?;
        while !done(&state) {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or(RuntimeLogWriterError::DeadlineExpired)?;
            let (next, result) = self
                .shared
                .changed
                .wait_timeout(state, remaining)
                .map_err(|_| RuntimeLogWriterError::Synchronization)?;
            state = next;
            if result.timed_out() && !done(&state) {
                return Err(RuntimeLogWriterError::DeadlineExpired);
            }
        }
        Ok(())
    }
}

impl Drop for RuntimeLogWriter {
    fn drop(&mut self) {
        if let Ok(mut state) = self.shared.state.lock() {
            state.accepting = false;
            state.draining = true;
            state.drain_deadline = Some(Instant::now());
            state.shutdown = Some(Instant::now());
            self.shared.wake.notify_one();
        }
    }
}

pub fn normalize_log_event(
    event: LogEvent,
    observed_timestamp_unix_nanos: i64,
    stream_sequence: u64,
    runtime_session_id: Option<RuntimeSessionId>,
    stream: &LogStreamMetadata,
    limits: &LogNormalizationLimits,
) -> ExecutionLogRecord {
    let mut warnings = Vec::new();
    let message = truncate(&event.message, limits.max_message_bytes);
    if message.len() != event.message.len() {
        warnings.push("message_truncated");
    }
    let (trace_id, span_id, invalid_trace) =
        trace_context(&event, limits.max_attribute_value_bytes);
    if invalid_trace.is_some() {
        warnings.push("invalid_trace_context");
    }
    let mut stringified = Vec::new();
    let mut attributes =
        normalize_attributes(&event.fields, limits, &mut stringified, &mut warnings);
    let correlation =
        ExecutionLogCorrelation::new(event.execution_id, event.attempt_id, event.attempt_number)
            .ok();
    if correlation.is_none() {
        warnings.push("invalid_execution_correlation");
    }
    if !warnings.is_empty() {
        attributes.insert(
            WARNING_KEY.to_string(),
            AttributeValue::String(join_capped(&warnings, limits.max_attribute_value_bytes)),
        );
    }
    if let Some(invalid) = invalid_trace {
        attributes.insert(
            INVALID_TRACE_KEY.to_string(),
            AttributeValue::String(truncate(&invalid, limits.max_attribute_value_bytes)),
        );
    }
    if !stringified.is_empty() {
        attributes.insert(
            STRINGIFIED_KEY.to_string(),
            AttributeValue::StringArray(stringified),
        );
    }
    let mut record = ExecutionLogRecord {
        timestamp_unix_nanos: event
            .timestamp_unix_nanos
            .unwrap_or(observed_timestamp_unix_nanos),
        observed_timestamp_unix_nanos,
        stream_sequence,
        stream_id: stream.stream_id.clone(),
        action_key_id: stream.action_key_id.clone(),
        action_revision: stream.action_revision.clone(),
        runtime_language: stream.runtime_language.clone(),
        runtime_session_id,
        correlation,
        severity: event.level,
        message,
        attributes,
        trace_id,
        span_id,
    };
    enforce_record_limit(&mut record, limits.max_record_bytes);
    record
}

fn validate_config(
    config: &RuntimeLogWriterConfig,
    stream: &LogStreamMetadata,
) -> Result<(), RuntimeLogWriterError> {
    if config.capacity == 0 || config.batch_size == 0 || config.batch_size > config.capacity {
        return Err(RuntimeLogWriterError::InvalidConfiguration(
            "capacity and batch size must be non-zero and batch size must not exceed capacity",
        ));
    }
    if config.retry_max_attempts == 0 {
        return Err(RuntimeLogWriterError::InvalidConfiguration(
            "retry attempts must be non-zero",
        ));
    }
    if config.retry_initial_backoff > config.retry_max_backoff {
        return Err(RuntimeLogWriterError::InvalidConfiguration(
            "initial retry backoff must not exceed maximum backoff",
        ));
    }
    if config.flush_interval.is_zero() {
        return Err(RuntimeLogWriterError::InvalidConfiguration(
            "flush interval must be non-zero",
        ));
    }
    let limits = &config.normalization;
    if limits.max_message_bytes == 0
        || limits.max_attributes < DIAGNOSTIC_SLOTS
        || limits.max_attribute_key_bytes == 0
        || limits.max_attribute_value_bytes == 0
        || limits.max_record_bytes == 0
    {
        return Err(RuntimeLogWriterError::InvalidConfiguration(
            "normalization limits must reserve diagnostic capacity",
        ));
    }
    let diagnostic_key_bytes = [WARNING_KEY, INVALID_TRACE_KEY, STRINGIFIED_KEY]
        .iter()
        .map(|key| key.len())
        .max()
        .unwrap_or(0);
    if limits.max_attribute_key_bytes < diagnostic_key_bytes {
        return Err(RuntimeLogWriterError::InvalidConfiguration(
            "attribute key limit must fit normalization diagnostic keys",
        ));
    }
    let minimum = ExecutionLogRecord {
        timestamp_unix_nanos: i64::MIN,
        observed_timestamp_unix_nanos: i64::MIN,
        stream_sequence: u64::MAX,
        stream_id: stream.stream_id.clone(),
        action_key_id: stream.action_key_id.clone(),
        action_revision: stream.action_revision.clone(),
        runtime_language: stream.runtime_language.clone(),
        runtime_session_id: Some(RuntimeSessionId::from("s".repeat(MAX_LOG_IDENTITY_BYTES))),
        correlation: Some(
            ExecutionLogCorrelation::new(
                ExecutionId::from("e".repeat(MAX_LOG_IDENTITY_BYTES)),
                AttemptId::from("a".repeat(MAX_LOG_IDENTITY_BYTES)),
                u32::MAX,
            )
            .map_err(|_| {
                RuntimeLogWriterError::InvalidConfiguration("correlated record envelope is invalid")
            })?,
        ),
        severity: ryvus_protocol::LogLevel::Error,
        message: String::new(),
        attributes: BTreeMap::new(),
        trace_id: Some(TraceId::from([u8::MAX; 16])),
        span_id: Some(SpanId::from([u8::MAX; 8])),
    };
    if exceeds_serialized_limit(&minimum, limits.max_record_bytes) {
        return Err(RuntimeLogWriterError::InvalidConfiguration(
            "record limit cannot fit immutable stream metadata",
        ));
    }
    Ok(())
}

fn validate_event_identity(
    event: &LogEvent,
    runtime_session_id: Option<&RuntimeSessionId>,
) -> Result<(), RuntimeLogWriterError> {
    if event.execution_id.as_ref().len() > MAX_LOG_IDENTITY_BYTES
        || event.attempt_id.as_ref().len() > MAX_LOG_IDENTITY_BYTES
        || runtime_session_id
            .is_some_and(|session_id| session_id.as_ref().len() > MAX_LOG_IDENTITY_BYTES)
    {
        return Err(RuntimeLogWriterError::InvalidIdentity(
            "execution, attempt, and session identifiers must not exceed 256 bytes",
        ));
    }
    if !is_safe_identity(event.execution_id.as_ref())
        || !is_safe_identity(event.attempt_id.as_ref())
        || runtime_session_id.is_some_and(|session_id| !is_safe_identity(session_id.as_ref()))
    {
        return Err(RuntimeLogWriterError::InvalidIdentity(
            "execution, attempt, and present session identifiers must use non-empty unescaped ASCII",
        ));
    }
    if event.attempt_number == 0 {
        return Err(RuntimeLogWriterError::InvalidIdentity(
            "attempt number must be non-zero",
        ));
    }
    Ok(())
}

fn validate_session_identity(
    runtime_session_id: Option<&RuntimeSessionId>,
) -> Result<(), RuntimeLogWriterError> {
    if runtime_session_id.is_some_and(|session_id| {
        session_id.as_ref().len() > MAX_LOG_IDENTITY_BYTES || !is_safe_identity(session_id.as_ref())
    }) {
        return Err(RuntimeLogWriterError::InvalidIdentity(
            "present session identifiers must use at most 256 bytes of non-empty unescaped ASCII",
        ));
    }
    Ok(())
}

fn is_safe_identity(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'"' | b'\\'))
}

fn writer_loop(
    shared: Arc<Shared>,
    store: Arc<dyn ExecutionLogStore>,
    stream: LogStreamMetadata,
    config: RuntimeLogWriterConfig,
    console: Option<SyncSender<ExecutionLogRecord>>,
) {
    let mut batch_number = 0_u64;
    let mut retained_loss_batch: Option<(LogBatch, bool)> = None;
    loop {
        let (batch, terminal) = match retained_loss_batch.take() {
            Some(work) => {
                let mut state = match shared.state.lock() {
                    Ok(state) => state,
                    Err(_) => return,
                };
                if lifecycle_deadline(&state).is_some_and(|deadline| Instant::now() >= deadline) {
                    state.active = false;
                    state.stopped = true;
                    shared.changed.notify_all();
                    return;
                }
                state.active = true;
                work
            }
            None => {
                let Some(work) = next_batch(&shared, &stream, &config, &mut batch_number) else {
                    mark_stopped(&shared);
                    return;
                };
                work
            }
        };
        let records = batch.records.clone();
        let outcome = append_with_retry(&shared, store.as_ref(), &batch, &config);
        let mut state = match shared.state.lock() {
            Ok(state) => state,
            Err(_) => return,
        };
        state.active = false;
        match outcome {
            AppendOutcome::Accepted => {
                state.pending_loss = subtract_loss(&state.pending_loss, &batch.loss_ranges);
                if let Some(console) = &console {
                    for record in records {
                        match console.try_send(record) {
                            Ok(())
                            | Err(TrySendError::Full(_))
                            | Err(TrySendError::Disconnected(_)) => {}
                        }
                    }
                }
                if terminal {
                    state.terminal_accepted = true;
                    state.stopped = true;
                }
            }
            AppendOutcome::Deadline => {
                for record in records {
                    if record_loss(
                        &mut state,
                        record.stream_sequence,
                        LogLossCause::ProviderFailure,
                    )
                    .is_err()
                    {
                        break;
                    }
                }
                state.stopped = true;
            }
            AppendOutcome::Failed => {
                let records_were_empty = records.is_empty();
                for record in records {
                    if record_loss(
                        &mut state,
                        record.stream_sequence,
                        LogLossCause::ProviderFailure,
                    )
                    .is_err()
                    {
                        state.stopped = true;
                        shared.changed.notify_all();
                        return;
                    }
                }
                state.flush_requested = state.draining && !state.pending_loss.is_empty();
                if records_were_empty {
                    retained_loss_batch = Some((batch, terminal));
                    let sleep = lifecycle_deadline(&state)
                        .and_then(|deadline| deadline.checked_duration_since(Instant::now()))
                        .unwrap_or(config.retry_max_backoff)
                        .min(config.retry_max_backoff.max(Duration::from_millis(1)));
                    drop(state);
                    thread::sleep(sleep);
                    continue;
                }
            }
        }
        shared.changed.notify_all();
        if state.stopped {
            return;
        }
    }
}

fn next_batch(
    shared: &Shared,
    stream: &LogStreamMetadata,
    config: &RuntimeLogWriterConfig,
    batch_number: &mut u64,
) -> Option<(LogBatch, bool)> {
    let mut state = shared.state.lock().ok()?;
    loop {
        if lifecycle_deadline(&state).is_some_and(|deadline| Instant::now() >= deadline) {
            let remaining: Vec<_> = state.queue.drain(..).collect();
            for record in remaining {
                if record_loss(
                    &mut state,
                    record.stream_sequence,
                    LogLossCause::ProviderFailure,
                )
                .is_err()
                {
                    break;
                }
            }
            state.stopped = true;
            shared.changed.notify_all();
            return None;
        }
        let shutdown = state.shutdown;
        let ready = state.queue.len() >= config.batch_size
            || state.flush_requested
            || (state.draining && !state.pending_loss.is_empty())
            || shutdown.is_some();
        if ready {
            let record_count = state.queue.len().min(config.batch_size);
            let records: Vec<_> = state.queue.drain(..record_count).collect();
            let terminal = shutdown.is_some() && state.queue.is_empty();
            if records.is_empty() && state.pending_loss.is_empty() && !terminal {
                state.flush_requested = false;
                shared.changed.notify_all();
            } else {
                *batch_number = batch_number.checked_add(1)?;
                state.active = true;
                state.flush_requested = false;
                let transition = terminal.then_some(if state.known_loss.is_empty() {
                    LogStreamTransition::Complete
                } else {
                    LogStreamTransition::Incomplete
                });
                return Some((
                    LogBatch {
                        stream: stream.clone(),
                        batch_id: format!("runtime-{batch_number}"),
                        records,
                        loss_ranges: state.pending_loss.clone(),
                        transition,
                    },
                    terminal,
                ));
            }
        }
        if state.stopped {
            return None;
        }
        let wait = shutdown
            .and_then(|deadline| deadline.checked_duration_since(Instant::now()))
            .unwrap_or(config.flush_interval)
            .min(config.flush_interval);
        let (next, timeout) = shared.wake.wait_timeout(state, wait).ok()?;
        state = next;
        if timeout.timed_out() && !state.queue.is_empty() {
            state.flush_requested = true;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppendOutcome {
    Accepted,
    Failed,
    Deadline,
}

fn append_with_retry(
    shared: &Shared,
    store: &dyn ExecutionLogStore,
    batch: &LogBatch,
    config: &RuntimeLogWriterConfig,
) -> AppendOutcome {
    let mut backoff = config.retry_initial_backoff;
    for attempt in 1..=config.retry_max_attempts {
        if deadline_reached(shared) {
            return AppendOutcome::Deadline;
        }
        match store.append_batch(batch.clone()) {
            Ok(()) => return AppendOutcome::Accepted,
            Err(error) if is_transient(&error) && attempt < config.retry_max_attempts => {
                if wait_for_retry(shared, backoff) {
                    return AppendOutcome::Deadline;
                }
                backoff = backoff.saturating_mul(2).min(config.retry_max_backoff);
            }
            Err(_) => return AppendOutcome::Failed,
        }
    }
    AppendOutcome::Failed
}

fn deadline_reached(shared: &Shared) -> bool {
    shared.state.lock().map_or(true, |state| {
        lifecycle_deadline(&state).is_some_and(|deadline| Instant::now() >= deadline)
    })
}

fn wait_for_retry(shared: &Shared, backoff: Duration) -> bool {
    let started = Instant::now();
    let mut state = match shared.state.lock() {
        Ok(state) => state,
        Err(_) => return true,
    };
    loop {
        let now = Instant::now();
        if lifecycle_deadline(&state).is_some_and(|deadline| now >= deadline) {
            return true;
        }
        let remaining_backoff = backoff.saturating_sub(started.elapsed());
        if remaining_backoff.is_zero() {
            return false;
        }
        let wait = lifecycle_deadline(&state)
            .and_then(|deadline| deadline.checked_duration_since(now))
            .unwrap_or(remaining_backoff)
            .min(remaining_backoff);
        state = match shared.wake.wait_timeout(state, wait) {
            Ok((state, _)) => state,
            Err(_) => return true,
        };
    }
}

fn lifecycle_deadline(state: &WriterState) -> Option<Instant> {
    match (state.shutdown, state.drain_deadline) {
        (Some(shutdown), Some(drain)) => Some(shutdown.min(drain)),
        (Some(deadline), None) | (None, Some(deadline)) => Some(deadline),
        (None, None) => None,
    }
}

fn is_transient(error: &LogStoreError) -> bool {
    matches!(error, LogStoreError::Unavailable | LogStoreError::Io)
}

fn mark_stopped(shared: &Shared) {
    if let Ok(mut state) = shared.state.lock() {
        state.stopped = true;
        shared.changed.notify_all();
    }
}

fn add_loss(
    ranges: &mut Vec<LogLossRange>,
    sequence: u64,
    cause: LogLossCause,
) -> Result<(), RuntimeLogWriterError> {
    ranges.push(LogLossRange {
        first_sequence: sequence,
        last_sequence: sequence,
        cause,
    });
    *ranges = normalize_loss_ranges(std::mem::take(ranges))
        .map_err(|_| RuntimeLogWriterError::Synchronization)?;
    Ok(())
}

fn record_loss(
    state: &mut WriterState,
    sequence: u64,
    cause: LogLossCause,
) -> Result<(), RuntimeLogWriterError> {
    add_loss(&mut state.known_loss, sequence, cause)?;
    add_loss(&mut state.pending_loss, sequence, cause)
}

fn subtract_loss(current: &[LogLossRange], committed: &[LogLossRange]) -> Vec<LogLossRange> {
    let mut remaining = Vec::new();
    for range in current {
        let mut pieces = vec![range.clone()];
        for durable in committed
            .iter()
            .filter(|durable| durable.cause == range.cause)
        {
            pieces = pieces
                .into_iter()
                .flat_map(|piece| {
                    if durable.last_sequence < piece.first_sequence
                        || durable.first_sequence > piece.last_sequence
                    {
                        return vec![piece];
                    }
                    let mut split = Vec::with_capacity(2);
                    if durable.first_sequence > piece.first_sequence {
                        split.push(LogLossRange {
                            first_sequence: piece.first_sequence,
                            last_sequence: durable.first_sequence.saturating_sub(1),
                            cause: piece.cause,
                        });
                    }
                    if durable.last_sequence < piece.last_sequence {
                        split.push(LogLossRange {
                            first_sequence: durable.last_sequence.saturating_add(1),
                            last_sequence: piece.last_sequence,
                            cause: piece.cause,
                        });
                    }
                    split
                })
                .collect();
        }
        remaining.extend(pieces);
    }
    remaining
}

fn normalize_attributes(
    fields: &Value,
    limits: &LogNormalizationLimits,
    stringified: &mut Vec<String>,
    warnings: &mut Vec<&'static str>,
) -> BTreeMap<String, AttributeValue> {
    let Some(fields) = fields.as_object() else {
        warnings.push("fields_not_object");
        return BTreeMap::new();
    };
    let capacity = limits.max_attributes.saturating_sub(DIAGNOSTIC_SLOTS);
    let mut attributes = BTreeMap::new();
    for (key, value) in fields.iter().take(capacity) {
        let original_key_len = key.len();
        let key = truncate(key, limits.max_attribute_key_bytes);
        if key.len() != original_key_len {
            warnings.push("attribute_key_truncated");
        }
        let (value, was_stringified, was_truncated) =
            attribute_value(value, limits.max_attribute_value_bytes);
        if was_stringified {
            push_bounded_array_value(stringified, &key, limits.max_attribute_value_bytes);
        }
        if was_truncated {
            warnings.push("attribute_value_truncated");
        }
        attributes.entry(key).or_insert(value);
    }
    if fields.len() > capacity {
        warnings.push("attributes_dropped");
    }
    attributes
}

fn attribute_value(value: &Value, limit: usize) -> (AttributeValue, bool, bool) {
    match value {
        Value::String(value) => (
            AttributeValue::String(truncate(value, limit)),
            false,
            value.len() > limit,
        ),
        Value::Bool(value) => (AttributeValue::Bool(*value), false, false),
        Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                (AttributeValue::I64(value), false, false)
            } else {
                (
                    AttributeValue::F64(value.as_f64().unwrap_or_default()),
                    false,
                    false,
                )
            }
        }
        Value::Array(values) => array_attribute_value(value, values, limit),
        _ => {
            let (serialized, truncated) = canonical_json_capped(value, limit);
            (AttributeValue::String(serialized), true, truncated)
        }
    }
}

fn array_attribute_value(
    original: &Value,
    values: &[Value],
    limit: usize,
) -> (AttributeValue, bool, bool) {
    if values.len() > limit.max(1) {
        let (serialized, _) = canonical_json_capped(original, limit);
        return (AttributeValue::String(serialized), true, true);
    }
    if values.iter().all(Value::is_string) {
        let mut remaining = limit;
        let mut normalized = Vec::new();
        for value in values.iter().filter_map(Value::as_str) {
            if remaining == 0 {
                break;
            }
            let value = truncate(value, remaining.saturating_sub(1));
            remaining = remaining.saturating_sub(value.len().saturating_add(1));
            normalized.push(value);
        }
        let truncated = normalized.len() != values.len()
            || normalized.iter().zip(values).any(|(normalized, original)| {
                original
                    .as_str()
                    .is_some_and(|original| normalized.len() != original.len())
            });
        return (AttributeValue::StringArray(normalized), false, truncated);
    }
    if values.iter().all(Value::is_boolean) {
        if values.len() > limit / 5 {
            return stringified_array(original, limit);
        }
        return (
            AttributeValue::BoolArray(values.iter().filter_map(Value::as_bool).collect()),
            false,
            false,
        );
    }
    if values.iter().all(|value| value.as_i64().is_some()) {
        if values.len() > limit / 20 {
            return stringified_array(original, limit);
        }
        return (
            AttributeValue::I64Array(values.iter().filter_map(Value::as_i64).collect()),
            false,
            false,
        );
    }
    if values.iter().all(|value| value.as_f64().is_some()) {
        if values.len() > limit / 24 {
            return stringified_array(original, limit);
        }
        return (
            AttributeValue::F64Array(values.iter().filter_map(Value::as_f64).collect()),
            false,
            false,
        );
    }
    stringified_array(original, limit)
}

fn stringified_array(value: &Value, limit: usize) -> (AttributeValue, bool, bool) {
    let (serialized, truncated) = canonical_json_capped(value, limit);
    (AttributeValue::String(serialized), true, truncated)
}

fn push_bounded_array_value(values: &mut Vec<String>, value: &str, limit: usize) {
    let used = values
        .iter()
        .fold(0_usize, |used, value| used.saturating_add(value.len() + 1));
    let remaining = limit.saturating_sub(used);
    if remaining > 0 {
        values.push(truncate(value, remaining.saturating_sub(1)));
    }
}

fn canonical_json_capped(value: &Value, limit: usize) -> (String, bool) {
    let mut writer = CappedWriter::new(limit);
    let result = serde_json::to_writer(&mut writer, value);
    let truncated = result.is_err() || writer.truncated;
    (writer.into_string(), truncated)
}

fn trace_context(
    event: &LogEvent,
    diagnostic_limit: usize,
) -> (Option<TraceId>, Option<SpanId>, Option<String>) {
    let trace = event
        .trace_id
        .as_deref()
        .and_then(parse_hex_id::<16>)
        .map(TraceId::from);
    let span = event
        .span_id
        .as_deref()
        .and_then(parse_hex_id::<8>)
        .map(SpanId::from);
    let valid = match (&event.trace_id, &event.span_id, trace, span) {
        (None, None, _, _) => return (None, None, None),
        (Some(_), None, Some(trace), _) => return (Some(trace), None, None),
        (Some(_), Some(_), Some(trace), Some(span)) => return (Some(trace), Some(span), None),
        (Some(_), Some(_), Some(trace), None) => (Some(trace), None),
        _ => (None, None),
    };
    let mut original = String::with_capacity(diagnostic_limit.min(64));
    push_capped(&mut original, "trace_id=", diagnostic_limit);
    push_capped(
        &mut original,
        event.trace_id.as_deref().unwrap_or("<none>"),
        diagnostic_limit,
    );
    push_capped(&mut original, ",span_id=", diagnostic_limit);
    push_capped(
        &mut original,
        event.span_id.as_deref().unwrap_or("<none>"),
        diagnostic_limit,
    );
    (valid.0, valid.1, Some(original))
}

fn enforce_record_limit(record: &mut ExecutionLogRecord, limit: usize) {
    while exceeds_serialized_limit(record, limit) {
        if !shrink_record(record) {
            break;
        }
    }
}

fn shrink_record(record: &mut ExecutionLogRecord) -> bool {
    if let Some(key) = record
        .attributes
        .keys()
        .find(|key| !is_diagnostic(key))
        .cloned()
    {
        record.attributes.remove(&key);
        return true;
    }
    if !record.message.is_empty() {
        record.message = truncate(&record.message, record.message.len() / 2);
        return true;
    }
    if [STRINGIFIED_KEY, INVALID_TRACE_KEY, WARNING_KEY]
        .into_iter()
        .any(|key| shrink_diagnostic(&mut record.attributes, key))
    {
        return true;
    }
    false
}

fn shrink_diagnostic(attributes: &mut BTreeMap<String, AttributeValue>, key: &str) -> bool {
    let Some(value) = attributes.get_mut(key) else {
        return false;
    };
    match value {
        AttributeValue::String(value) if value.len() > 1 => {
            *value = truncate(value, value.len() / 2);
        }
        AttributeValue::StringArray(values) if !values.is_empty() => {
            values.pop();
        }
        _ => {
            attributes.remove(key);
        }
    }
    true
}

fn exceeds_serialized_limit(record: &ExecutionLogRecord, limit: usize) -> bool {
    let mut writer = CappedWriter::new(limit);
    serde_json::to_writer(&mut writer, record).is_err() || writer.truncated
}

fn is_diagnostic(key: &str) -> bool {
    matches!(key, WARNING_KEY | INVALID_TRACE_KEY | STRINGIFIED_KEY)
}

fn truncate(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes.min(value.len());
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    value[..end].to_string()
}

fn join_capped(values: &[&str], limit: usize) -> String {
    let mut joined = String::with_capacity(limit.min(64));
    for (index, value) in values.iter().enumerate() {
        if index != 0 {
            push_capped(&mut joined, ",", limit);
        }
        push_capped(&mut joined, value, limit);
        if joined.len() == limit {
            break;
        }
    }
    joined
}

fn push_capped(target: &mut String, value: &str, limit: usize) {
    let remaining = limit.saturating_sub(target.len());
    if remaining > 0 {
        let value = truncate(value, remaining);
        target.push_str(&value);
    }
}

fn parse_hex_id<const N: usize>(value: &str) -> Option<[u8; N]> {
    if value.len() != N * 2 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut bytes = [0_u8; N];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let pair = std::str::from_utf8(pair).ok()?;
        bytes[index] = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(bytes)
}

struct CappedWriter {
    bytes: Vec<u8>,
    limit: usize,
    truncated: bool,
}

impl CappedWriter {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(limit.min(1024)),
            limit,
            truncated: false,
        }
    }

    fn into_string(mut self) -> String {
        while std::str::from_utf8(&self.bytes).is_err() {
            self.bytes.pop();
        }
        String::from_utf8(self.bytes).unwrap_or_default()
    }
}

impl Write for CappedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let remaining = self.limit.saturating_sub(self.bytes.len());
        if bytes.len() <= remaining {
            self.bytes.extend_from_slice(bytes);
            return Ok(bytes.len());
        }
        self.bytes.extend_from_slice(&bytes[..remaining]);
        self.truncated = true;
        Err(io::Error::new(io::ErrorKind::WriteZero, "limit reached"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, RuntimeLogWriterError> {
    mutex
        .lock()
        .map_err(|_| RuntimeLogWriterError::Synchronization)
}
