use ryvus_execution::ExecutionRecord;

use crate::{ExecutionPersistence, PersistenceResult};

#[derive(Debug, Clone, Default)]
pub struct ConsoleExecutionPersistence;

impl ExecutionPersistence for ConsoleExecutionPersistence {
    fn save_execution(&self, _record: &ExecutionRecord) -> PersistenceResult<()> {
        Ok(())
    }
}
