use std::io::Write;
use std::process::{Command, Stdio};

use crate::error::{ExecutorError, ExecutorResult};
use crate::executor::Executor;
use crate::target::ProcessTarget;
use ryvus_execution::ExecutionResult;
use ryvus_protocol::{
    InvocationEvent, InvocationMessage, InvocationRequest, InvocationResult, PROTOCOL_VERSION,
};

#[derive(Debug, Clone, Default)]
pub struct LocalProcessExecutor;

impl LocalProcessExecutor {
    pub fn new() -> Self {
        Self
    }
}

impl Executor for LocalProcessExecutor {
    fn invoke(
        &self,
        target: &ProcessTarget,
        request: &InvocationRequest,
    ) -> ExecutorResult<ExecutionResult> {
        use std::time::Instant;

        let started = Instant::now();

        if request.protocol_version != PROTOCOL_VERSION {
            return Err(ExecutorError::InvalidProtocolVersion {
                expected: PROTOCOL_VERSION.to_string(),
                actual: request.protocol_version.clone(),
            });
        }

        let request_json = serde_json::to_vec(&request)?;

        let mut command = Command::new(&target.command);
        command.args(&target.args);

        if let Some(working_dir) = &target.working_dir {
            command.current_dir(working_dir);
        }

        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(&request_json)?;
        }

        let output = child.wait_with_output()?;

        if !output.status.success() {
            return Err(ExecutorError::ProcessFailed {
                command: target.command.clone(),
                exit_code: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let mut invocation_result: Option<InvocationResult> = None;

        for line in stdout.lines() {
            let line = line.trim();

            if line.is_empty() {
                continue;
            }

            let message: InvocationMessage = serde_json::from_str(line)?;

            match message {
                InvocationMessage::Event { event } => match event {
                    InvocationEvent::Log(log) => {
                        tracing::info!(
                            invocation_id = %log.invocation_id,
                            fields = %log.fields,
                            "{}", log.message
                        );
                    }
                    InvocationEvent::Metric(metric) => {
                        tracing::info!(
                            invocation_id = %metric.invocation_id,
                            name = %metric.name,
                            value = metric.value,
                            unit = %metric.unit,
                            "action metric"
                        );
                    }
                },
                InvocationMessage::Result { result } => {
                    invocation_result = Some(result);
                }
            }
        }

        let invocation_result = invocation_result.ok_or(ExecutorError::MissingInvocationResult)?;

        let duration = started.elapsed();

        Ok(ExecutionResult {
            invocation_result,
            stdout,
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            duration,
            exit_code: output.status.code(),
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use ryvus_protocol::{InvocationRequest, InvocationStatus};

    use crate::executor::Executor;
    use crate::local_process::LocalProcessExecutor;
    use crate::target::ProcessTarget;

    #[test]
    fn invokes_process_that_returns_invocation_result() {
        let request = InvocationRequest::new(json!({ "message": "hello" }));

        let target = ProcessTarget::new("sh").args([
            "-c",
            "cat >/dev/null && echo '{\"type\":\"result\",\"result\":{\"protocol_version\":\"ryvus.invoke.v1\",\"invocation_id\":\"inv_test\",\"status\":\"success\",\"output\":{\"ok\":true},\"error\":null}}'",
        ]);

        let executor = LocalProcessExecutor::new();

        let result = executor
            .invoke(&target, &request)
            .expect("process should succeed");

        assert_eq!(result.invocation_result.invocation_id, "inv_test");
        assert_eq!(result.invocation_result.status, InvocationStatus::Success);
        assert_eq!(result.invocation_result.output, Some(json!({ "ok": true })));
        assert_eq!(result.exit_code, Some(0));
    }

    #[test]
    fn invokes_process_that_emits_log_event_then_result() {
        let request = InvocationRequest::new(json!({ "message": "hello" }));

        let target = ProcessTarget::new("sh").args([
            "-c",
            "cat >/dev/null && \
             echo '{\"type\":\"event\",\"event\":{\"type\":\"log\",\"invocation_id\":\"inv_test\",\"level\":\"info\",\"message\":\"hello log\",\"fields\":{}}}' && \
             echo '{\"type\":\"result\",\"result\":{\"protocol_version\":\"ryvus.invoke.v1\",\"invocation_id\":\"inv_test\",\"status\":\"success\",\"output\":{\"ok\":true},\"error\":null}}'",
        ]);

        let executor = LocalProcessExecutor::new();

        let result = executor
            .invoke(&target, &request)
            .expect("process should succeed");

        assert_eq!(result.invocation_result.invocation_id, "inv_test");
        assert_eq!(result.invocation_result.status, InvocationStatus::Success);
        assert_eq!(result.invocation_result.output, Some(json!({ "ok": true })));
        assert_eq!(result.exit_code, Some(0));
    }
}
