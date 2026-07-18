use std::sync::Arc;
use std::time::Duration;

use ryvus_logging::RuntimeLogContext;
use ryvus_protocol::InvocationRequest;

use crate::{error::ExecutorResult, ExecutionResult, RuntimeTarget};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionOptions {
    pub timeout: Duration,
    pub log_context: RuntimeLogContext,
}

pub trait Executor: Send + Sync {
    fn invoke(
        &self,
        target: &RuntimeTarget,
        request: &InvocationRequest,
        options: &ExecutionOptions,
    ) -> ExecutorResult<ExecutionResult>;

    fn shutdown(&self, _grace: Duration) -> ExecutorResult<()> {
        Ok(())
    }
}

impl<T> Executor for Arc<T>
where
    T: Executor + ?Sized,
{
    fn invoke(
        &self,
        target: &RuntimeTarget,
        request: &InvocationRequest,
        options: &ExecutionOptions,
    ) -> ExecutorResult<ExecutionResult> {
        self.as_ref().invoke(target, request, options)
    }

    fn shutdown(&self, grace: Duration) -> ExecutorResult<()> {
        self.as_ref().shutdown(grace)
    }
}
