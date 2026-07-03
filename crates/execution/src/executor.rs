use std::sync::Arc;

use ryvus_protocol::InvocationRequest;

use crate::{error::ExecutorResult, ExecutionResult, ProcessTarget};

pub trait Executor: Send + Sync {
    fn invoke(
        &self,
        target: &ProcessTarget,
        request: &InvocationRequest,
    ) -> ExecutorResult<ExecutionResult>;
}

impl<T> Executor for Arc<T>
where
    T: Executor + ?Sized,
{
    fn invoke(
        &self,
        target: &ProcessTarget,
        request: &InvocationRequest,
    ) -> ExecutorResult<ExecutionResult> {
        self.as_ref().invoke(target, request)
    }
}
