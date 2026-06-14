use std::io::Write;
use std::process::{Command, Stdio};

use crate::contract::{InvocationRequest, InvocationResult, PROTOCOL_VERSION};
use crate::error::{ExecutorError, ExecutorResult};
use crate::executor::Executor;

#[derive(Debug, Clone)]
pub struct LocalProcessExecutor {
    command: String,
    args: Vec<String>,
}

impl LocalProcessExecutor {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            args: Vec::new(),
        }
    }

    pub fn with_args(
        command: impl Into<String>,
        args: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            command: command.into(),
            args: args.into_iter().map(Into::into).collect(),
        }
    }
}

impl Executor for LocalProcessExecutor {
    fn invoke(&self, request: InvocationRequest) -> ExecutorResult<InvocationResult> {
        if request.protocol_version != PROTOCOL_VERSION {
            return Err(ExecutorError::InvalidProtocolVersion {
                expected: PROTOCOL_VERSION.to_string(),
                actual: request.protocol_version,
            });
        }

        let request_json = serde_json::to_vec(&request)?;

        let mut child = Command::new(&self.command)
            .args(&self.args)
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
                command: self.command.clone(),
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

    use crate::{Executor, InvocationRequest, InvocationStatus, LocalProcessExecutor};

    #[test]
    fn invokes_process_that_returns_invocation_result() {
        let request = InvocationRequest::new(json!({ "message": "hello" }));
        let executor = LocalProcessExecutor::with_args(
            "sh",
            [
                "-c",
                "cat >/dev/null && echo '{\"protocol_version\":\"ryvus.invoke.v1\",\"invocation_id\":\"inv_test\",\"status\":\"success\",\"output\":{\"ok\":true},\"error\":null,\"metadata\":{}}'",
            ],
        );

        let result = executor.invoke(request).expect("process should succeed");

        assert_eq!(result.invocation_id, "inv_test");
        assert_eq!(result.status, InvocationStatus::Success);
        assert_eq!(result.output, Some(json!({ "ok": true })));
    }
}
