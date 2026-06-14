use std::fs;
use std::path::{Path, PathBuf};

use ryvus_execution::ExecutionRecord;

use crate::error::PersistenceResult;
use crate::persistence::ExecutionPersistence;

#[derive(Debug, Clone)]
pub struct FilesystemExecutionPersistence {
    root: PathBuf,
}

impl FilesystemExecutionPersistence {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn execution_dir(&self, record: &ExecutionRecord) -> PathBuf {
        self.root.join("runs").join(&record.invocation_id)
    }

    fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> PersistenceResult<()> {
        let json = serde_json::to_string_pretty(value)?;
        fs::write(path, json)?;
        Ok(())
    }
}

impl ExecutionPersistence for FilesystemExecutionPersistence {
    fn save_execution(&self, record: &ExecutionRecord) -> PersistenceResult<()> {
        let dir = self.execution_dir(record);

        fs::create_dir_all(&dir)?;

        Self::write_json(&dir.join("record.json"), record)?;
        Self::write_json(&dir.join("request.json"), &record.request)?;
        Self::write_json(&dir.join("result.json"), &record.result)?;

        fs::write(dir.join("stdout.log"), &record.result.stdout)?;
        fs::write(dir.join("stderr.log"), &record.result.stderr)?;

        Ok(())
    }
}
