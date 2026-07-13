use std::time::Duration;
use std::time::SystemTime;

use ryvus_protocol::InvocationRequest;

use crate::{
    ExecutionOptions, ExecutionRecord, ExecutionTarget, Executor, ExecutorResult, RuntimeTarget,
};

pub struct RecordingExecutor<E> {
    inner: E,
}

impl<E> RecordingExecutor<E> {
    pub fn new(inner: E) -> Self {
        Self { inner }
    }
}

impl<E: Executor> RecordingExecutor<E> {
    pub fn invoke_recorded(
        &self,
        target: &RuntimeTarget,
        request: &InvocationRequest,
        options: &ExecutionOptions,
    ) -> ExecutorResult<ExecutionRecord> {
        let started_at = SystemTime::now();
        let result = self.inner.invoke(target, request, options)?;
        let finished_at = SystemTime::now();

        let execution_target = match target {
            RuntimeTarget::LocalProcess(target) => ExecutionTarget::Process {
                command: target.command.clone(),
                args: target.args.clone(),
                working_dir: target.working_dir.clone(),
                env: target.env.clone(),
            },
            RuntimeTarget::Http { endpoint } => ExecutionTarget::Http {
                method: "POST".to_string(),
                url: format!("{}/invoke", endpoint.trim_end_matches('/')),
            },
        };

        Ok(ExecutionRecord::new(
            request.clone(),
            execution_target,
            result,
            started_at,
            finished_at,
        ))
    }

    pub fn cancel(&self, invocation_id: &str) -> ExecutorResult<bool> {
        self.inner.cancel(invocation_id)
    }

    pub fn shutdown(&self, grace: Duration) -> ExecutorResult<()> {
        self.inner.shutdown(grace)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use ryvus_protocol::{InvocationRequest, InvocationStatus};

    use crate::{ExecutionTarget, Executor, RecordingExecutor, RuntimeTarget};

    #[test]
    fn invokes_and_returns_execution_record() {
        let request = InvocationRequest::new(json!({ "message": "hello" }));

        let target = RuntimeTarget::http("http://runtime.example");
        let executor = RecordingExecutor::new(SuccessExecutor);

        let record = executor
            .invoke_recorded(
                &target,
                &request,
                &crate::ExecutionOptions {
                    timeout: std::time::Duration::from_secs(3),
                },
            )
            .expect("recorded invocation should succeed");
        match &record.target {
            ExecutionTarget::Http { url, .. } => {
                assert_eq!(url, "http://runtime.example/invoke");
            }
            _ => panic!("expected HTTP target"),
        }

        assert_eq!(record.invocation_id, request.invocation_id);
        assert_eq!(record.request.invocation_id, request.invocation_id);
        assert_eq!(record.result.exit_code, None);
        assert_eq!(
            record.result.invocation_result.status,
            InvocationStatus::Success
        );
        assert_eq!(
            record.result.invocation_result.output,
            Some(json!({ "ok": true }))
        );
        assert!(record.finished_at >= record.started_at);
    }

    struct SuccessExecutor;

    impl Executor for SuccessExecutor {
        fn invoke(
            &self,
            _target: &RuntimeTarget,
            request: &InvocationRequest,
            _options: &crate::ExecutionOptions,
        ) -> crate::ExecutorResult<crate::ExecutionResult> {
            Ok(crate::ExecutionResult {
                invocation_result: ryvus_protocol::InvocationResult::success(
                    request.invocation_id.clone(),
                    json!({ "ok": true }),
                ),
                stdout: String::new(),
                stderr: String::new(),
                duration: std::time::Duration::ZERO,
                exit_code: None,
            })
        }
    }
}
