use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
};

use crate::{
    normalize_loss_ranges, projection::StoreProjection, ExecutionLogStore, LogBatch,
    LogProjectedRecordPage, LogProjectedRecordQuery, LogRecordPage, LogRecordQuery, LogStoreError,
    LogStreamPage, LogStreamQuery,
};

const DEFAULT_MAX_BATCH_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesystemLogStoreConfig {
    pub root: PathBuf,
    pub max_batch_bytes: usize,
}

impl Default for FilesystemLogStoreConfig {
    fn default() -> Self {
        Self {
            root: PathBuf::from(".ryvus/logs"),
            max_batch_bytes: DEFAULT_MAX_BATCH_BYTES,
        }
    }
}

pub struct FilesystemExecutionLogStore {
    root: PathBuf,
    max_batch_bytes: usize,
    state: Mutex<FilesystemState>,
}

struct FilesystemState {
    projection: StoreProjection,
    writable: bool,
}

impl FilesystemExecutionLogStore {
    pub fn new(config: FilesystemLogStoreConfig) -> Result<Self, LogStoreError> {
        if config.max_batch_bytes == 0 {
            return Err(LogStoreError::InvalidConfiguration(
                "filesystem batch limit must be greater than zero".to_string(),
            ));
        }
        fs::create_dir_all(&config.root).map_err(io_error)?;
        let projection = recover_projection(&config.root, config.max_batch_bytes)?;
        Ok(Self {
            root: config.root,
            max_batch_bytes: config.max_batch_bytes,
            state: Mutex::new(FilesystemState {
                projection,
                writable: true,
            }),
        })
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, FilesystemState>, LogStoreError> {
        self.state.lock().map_err(|_| LogStoreError::Unavailable)
    }

    fn stream_path(&self, batch: &LogBatch) -> PathBuf {
        self.root.join(relative_stream_path(batch))
    }
}

impl ExecutionLogStore for FilesystemExecutionLogStore {
    fn append_batch(&self, mut batch: LogBatch) -> Result<(), LogStoreError> {
        let mut state = self.lock_state()?;
        if !state.writable {
            return Err(LogStoreError::Io);
        }
        batch.loss_ranges = normalize_loss_ranges(batch.loss_ranges)?;
        let mut candidate = state.projection.clone();
        candidate.append_batch(batch.clone())?;
        if state.projection.contains_batch(&batch)? {
            return Ok(());
        }

        let envelope = serde_json::to_vec(&batch)
            .map_err(|_| LogStoreError::InvalidBatch("batch cannot be serialized".to_string()))?;
        if envelope.len() > self.max_batch_bytes {
            return Err(LogStoreError::InvalidBatch(format!(
                "serialized batch exceeds {} bytes",
                self.max_batch_bytes
            )));
        }

        let path = self.stream_path(&batch);
        let parent = path.parent().ok_or_else(|| {
            LogStoreError::InvalidConfiguration("filesystem root has no parent".to_string())
        })?;
        fs::create_dir_all(parent).map_err(io_error)?;
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(path)
            .map_err(io_error)?;
        let committed_len = file.metadata().map_err(io_error)?.len();
        if file
            .write_all(&envelope)
            .and_then(|()| file.write_all(b"\n"))
            .and_then(|()| file.flush())
            .is_err()
        {
            if file.set_len(committed_len).is_err() {
                state.writable = false;
            }
            return Err(LogStoreError::Io);
        }

        state.projection = candidate;
        Ok(())
    }

    fn list_streams(&self, query: LogStreamQuery) -> Result<LogStreamPage, LogStoreError> {
        self.lock_state()?.projection.list_streams(query)
    }

    fn list_records(&self, query: LogRecordQuery) -> Result<LogRecordPage, LogStoreError> {
        self.lock_state()?.projection.list_records(query)
    }

    fn list_projected_records(
        &self,
        query: LogProjectedRecordQuery,
    ) -> Result<LogProjectedRecordPage, LogStoreError> {
        self.lock_state()?.projection.list_projected_records(query)
    }
}

fn recover_projection(
    root: &Path,
    max_batch_bytes: usize,
) -> Result<StoreProjection, LogStoreError> {
    let mut files = Vec::new();
    collect_ndjson_files(root, &mut files)?;
    files.sort();

    let mut projection = StoreProjection::default();
    for path in files {
        recover_file(root, &path, max_batch_bytes, &mut projection)?;
    }
    for stream in projection.streams.values_mut() {
        stream.mark_recovered();
    }
    Ok(projection)
}

fn collect_ndjson_files(root: &Path, files: &mut Vec<PathBuf>) -> Result<(), LogStoreError> {
    for entry in fs::read_dir(root).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        let file_type = entry.file_type().map_err(io_error)?;
        if file_type.is_dir() {
            collect_ndjson_files(&entry.path(), files)?;
        } else if file_type.is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "ndjson")
        {
            files.push(entry.path());
        }
    }
    Ok(())
}

fn recover_file(
    root: &Path,
    path: &Path,
    max_batch_bytes: usize,
    projection: &mut StoreProjection,
) -> Result<(), LogStoreError> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(io_error)?;
    let mut reader = BufReader::new(file);
    let max_line_bytes = max_batch_bytes
        .checked_add(1)
        .ok_or(LogStoreError::CapacityOverflow)?;
    let max_line_bytes =
        u64::try_from(max_line_bytes).map_err(|_| LogStoreError::CapacityOverflow)?;
    let mut line = Vec::new();
    let mut committed_len = 0_u64;

    loop {
        line.clear();
        let read = reader
            .by_ref()
            .take(max_line_bytes)
            .read_until(b'\n', &mut line)
            .map_err(io_error)?;
        if read == 0 {
            break;
        }
        if !line.ends_with(b"\n") {
            if discard_fragment_tail(&mut reader)? {
                return Err(LogStoreError::Corruption);
            }
            reader
                .into_inner()
                .set_len(committed_len)
                .map_err(io_error)?;
            break;
        }
        committed_len = committed_len
            .checked_add(u64::try_from(read).map_err(|_| LogStoreError::CapacityOverflow)?)
            .ok_or(LogStoreError::CapacityOverflow)?;
        line.pop();
        let batch =
            serde_json::from_slice::<LogBatch>(&line).map_err(|_| LogStoreError::Corruption)?;
        let relative = path
            .strip_prefix(root)
            .map_err(|_| LogStoreError::Corruption)?;
        if relative != relative_stream_path(&batch) {
            return Err(LogStoreError::Corruption);
        }
        projection
            .append_batch(batch)
            .map_err(|_| LogStoreError::Corruption)?;
    }
    Ok(())
}

fn discard_fragment_tail(reader: &mut BufReader<File>) -> Result<bool, LogStoreError> {
    loop {
        let buffered = reader.fill_buf().map_err(io_error)?;
        if buffered.is_empty() {
            return Ok(false);
        }
        if let Some(index) = buffered.iter().position(|byte| *byte == b'\n') {
            reader.consume(index + 1);
            return Ok(true);
        }
        let consumed = buffered.len();
        reader.consume(consumed);
    }
}

fn hex_encode(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(value.len().saturating_mul(2));
    for byte in value.bytes() {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn relative_stream_path(batch: &LogBatch) -> PathBuf {
    PathBuf::from(hex_encode(batch.stream.stream_id.execution_scope.as_ref()))
        .join("actions")
        .join(hex_encode(&batch.stream.action_key_id))
        .join(format!(
            "{}.ndjson",
            hex_encode(batch.stream.stream_id.runtime_host_id.as_ref())
        ))
}

fn io_error(_: std::io::Error) -> LogStoreError {
    LogStoreError::Io
}
