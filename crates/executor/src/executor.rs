use ryvus_protocol::{InvocationRequest, InvocationResult};

use crate::error::ExecutorResult;
use crate::target::ProcessTarget;
pub trait Executor {
    fn invoke(
        &self,
        target: &ProcessTarget,
        request: &InvocationRequest,
    ) -> ExecutorResult<InvocationResult>;
}
