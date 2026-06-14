use ryvus_execution::ExecutionRecord;

use crate::error::PersistenceResult;

pub trait ExecutionPersistence {
    fn save_execution(&self, record: &ExecutionRecord) -> PersistenceResult<()>;
}
