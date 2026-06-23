use ryvus_execution::ExecutionRecord;
use ryvus_executor::{Executor, RecordingExecutor, RuntimeResolver};
use ryvus_protocol::ActionDefinition;

use ryvus_persistence::ExecutionPersistence;
use ryvus_protocol::InvocationRequest;

use crate::error::ExecutionServiceResult;

pub struct ExecutionService<RR, E, EP> {
    resolver: RR,
    executor: RecordingExecutor<E>,
    persistence: EP,
}

impl<RR, E, EP> ExecutionService<RR, E, EP>
where
    RR: RuntimeResolver,
    E: Executor,
    EP: ExecutionPersistence,
{
    pub fn new(resolver: RR, executor: E, persistence: EP) -> Self {
        Self {
            resolver,
            executor: RecordingExecutor::new(executor),
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
