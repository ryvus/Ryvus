use std::sync::Arc;

use crate::ExecutionRecord;

pub type ExecutionPersistenceResult<T> =
    Result<T, Box<dyn std::error::Error + Send + Sync + 'static>>;

pub trait ExecutionPersistence: Send + Sync {
    fn save_execution(&self, record: &ExecutionRecord) -> ExecutionPersistenceResult<()>;
}

impl<T> ExecutionPersistence for Arc<T>
where
    T: ExecutionPersistence + ?Sized,
{
    fn save_execution(&self, record: &ExecutionRecord) -> ExecutionPersistenceResult<()> {
        (**self).save_execution(record)
    }
}
