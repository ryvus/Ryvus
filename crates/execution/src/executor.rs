use std::sync::Arc;
use std::time::Duration;

use ryvus_protocol::InvocationRequest;

use crate::{error::ExecutorResult, ExecutionResult, ProcessTarget};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionOptions {
    pub timeout: Duration,
}

pub trait Executor: Send + Sync {
    fn invoke(
        &self,
        target: &ProcessTarget,
        request: &InvocationRequest,
        options: &ExecutionOptions,
    ) -> ExecutorResult<ExecutionResult>;

    fn cancel(&self, _invocation_id: &str) -> ExecutorResult<bool> {
        Ok(false)
    }
}

impl<T> Executor for Arc<T>
where
    T: Executor + ?Sized,
{
    fn invoke(
        &self,
        target: &ProcessTarget,
        request: &InvocationRequest,
        options: &ExecutionOptions,
    ) -> ExecutorResult<ExecutionResult> {
        self.as_ref().invoke(target, request, options)
    }

    fn cancel(&self, invocation_id: &str) -> ExecutorResult<bool> {
        self.as_ref().cancel(invocation_id)
    }
}
