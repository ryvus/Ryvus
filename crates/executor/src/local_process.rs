use std::io::Write;
use std::process::{Command, Stdio};

use ryvus_protocol::{InvocationRequest, InvocationResult, PROTOCOL_VERSION};

use crate::error::{ExecutorError, ExecutorResult};
use crate::executor::Executor;
use crate::target::ProcessTarget;

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
    ) -> ExecutorResult<InvocationResult> {
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

        let result: InvocationResult = serde_json::from_slice(&output.stdout)?;

        if result.protocol_version != PROTOCOL_VERSION {
            return Err(ExecutorError::InvalidProtocolVersion {
                expected: PROTOCOL_VERSION.to_string(),
                actual: result.protocol_version,
            });
        }

        Ok(result)
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
            "cat >/dev/null && echo '{\"protocol_version\":\"ryvus.invoke.v1\",\"invocation_id\":\"inv_test\",\"status\":\"success\",\"output\":{\"ok\":true},\"error\":null}'",
        ]);

        let executor = LocalProcessExecutor::new();

        let result = executor
            .invoke(&target, &request)
            .expect("process should succeed");

        assert_eq!(result.invocation_id, "inv_test");
        assert_eq!(result.status, InvocationStatus::Success);
        assert_eq!(result.output, Some(json!({ "ok": true })));
    }
}
