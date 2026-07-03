use std::time::SystemTime;

use ryvus_protocol::InvocationRequest;

use crate::{ExecutionRecord, ExecutionTarget, Executor, ExecutorResult, ProcessTarget};

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
        target: &ProcessTarget,
        request: &InvocationRequest,
    ) -> ExecutorResult<ExecutionRecord> {
        let started_at = SystemTime::now();
        let result = self.inner.invoke(target, request)?;
        let finished_at = SystemTime::now();

        let execution_target = ExecutionTarget::Process {
            command: target.command.clone(),
            args: target.args.clone(),
            working_dir: target.working_dir.clone(),
            env: target.env.clone(),
        };

        Ok(ExecutionRecord::new(
            request.clone(),
            execution_target,
            result,
            started_at,
            finished_at,
        ))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use ryvus_protocol::{InvocationRequest, InvocationStatus};

    use crate::{ExecutionTarget, LocalProcessExecutor, ProcessTarget, RecordingExecutor};

    #[test]
    fn invokes_and_returns_execution_record() {
        let request = InvocationRequest::new(json!({ "message": "hello" }));

        let target = ProcessTarget::new("sh").args([
            "-c",
            "cat >/dev/null && echo '{\"type\":\"result\",\"result\":{\"protocol_version\":\"ryvus.invoke.v1\",\"invocation_id\":\"inv_test\",\"status\":\"success\",\"output\":{\"ok\":true},\"error\":null}}'",
        ]);

        let executor = RecordingExecutor::new(LocalProcessExecutor::new());

        let record = executor
            .invoke_recorded(&target, &request)
            .expect("recorded invocation should succeed");
        match &record.target {
            ExecutionTarget::Process { command, .. } => {
                assert_eq!(command, "sh");
            }
            _ => panic!("expected process target"),
        }

        assert_eq!(record.invocation_id, request.invocation_id);
        assert_eq!(record.request.invocation_id, request.invocation_id);
        assert_eq!(record.result.exit_code, Some(0));
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
}
