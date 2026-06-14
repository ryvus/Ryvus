use ryvus_execution::ExecutionRecord;
use ryvus_executor::{ActionDefinition, Executor, LocalRuntimeResolver, RecordingExecutor};
use ryvus_persistence::ExecutionPersistence;
use ryvus_protocol::InvocationRequest;

use crate::error::ExecutionServiceResult;

pub struct ExecutionService<E, P> {
    resolver: LocalRuntimeResolver,
    executor: RecordingExecutor<E>,
    persistence: P,
}

impl<E, P> ExecutionService<E, P>
where
    E: Executor,
    P: ExecutionPersistence,
{
    pub fn new(
        resolver: LocalRuntimeResolver,
        executor: RecordingExecutor<E>,
        persistence: P,
    ) -> Self {
        Self {
            resolver,
            executor,
            persistence,
        }
    }

    pub fn execute(
        &self,
        action: &ActionDefinition,
        request: &InvocationRequest,
    ) -> ExecutionServiceResult<ExecutionRecord> {
        let target = self.resolver.resolve(action)?;
        let record = self.executor.invoke_recorded(&target, request)?;

        self.persistence.save_execution(&record)?;

        Ok(record)
    }

    pub fn execute_event(
        &self,
        action: &ActionDefinition,
        event: serde_json::Value,
    ) -> ExecutionServiceResult<ExecutionRecord> {
        let request = InvocationRequest::new(event);
        self.execute(action, &request)
    }
}
