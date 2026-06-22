use std::sync::Arc;

use ryvus_execution::ExecutionRecord;

use crate::PersistenceResult;

pub trait ExecutionPersistence: Send + Sync {
    fn save_execution(&self, record: &ExecutionRecord) -> PersistenceResult<()>;
}

impl<T> ExecutionPersistence for Arc<T>
where
    T: ExecutionPersistence + ?Sized,
{
    fn save_execution(&self, record: &ExecutionRecord) -> PersistenceResult<()> {
        (**self).save_execution(record)
    }
}
