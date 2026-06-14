use crate::contract::{InvocationRequest, InvocationResult};
use crate::error::ExecutorResult;

pub trait Executor {
    fn invoke(&self, request: InvocationRequest) -> ExecutorResult<InvocationResult>;
}
