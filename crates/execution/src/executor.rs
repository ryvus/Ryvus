use std::sync::Arc;
use std::time::Duration;

use ryvus_protocol::{AttemptId, InvocationRequest};

use crate::{error::ExecutorResult, ExecutionResult, RuntimeTarget};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionOptions {
    pub timeout: Duration,
}

pub trait Executor: Send + Sync {
    fn invoke(
        &self,
        target: &RuntimeTarget,
        request: &InvocationRequest,
        options: &ExecutionOptions,
    ) -> ExecutorResult<ExecutionResult>;

    fn cancel(&self, _attempt_id: &AttemptId) -> ExecutorResult<bool> {
        Ok(false)
    }

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

    fn cancel(&self, attempt_id: &AttemptId) -> ExecutorResult<bool> {
        self.as_ref().cancel(attempt_id)
    }

    fn shutdown(&self, grace: Duration) -> ExecutorResult<()> {
        self.as_ref().shutdown(grace)
    }
}
