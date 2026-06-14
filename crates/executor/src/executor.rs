use ryvus_execution::ExecutionResult;
use ryvus_protocol::InvocationRequest;

use crate::error::ExecutorResult;
use crate::target::ProcessTarget;
pub trait Executor {
    fn invoke(
        &self,
        target: &ProcessTarget,
        request: &InvocationRequest,
    ) -> ExecutorResult<ExecutionResult>;
}
