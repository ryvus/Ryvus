use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use crate::error::{ExecutorError, ExecutorResult};
use crate::executor::Executor;
use crate::target::ProcessTarget;
use crate::{ConsoleInvocationEventSink, InvocationEventSink};
use ryvus_execution::ExecutionResult;
use ryvus_protocol::{InvocationMessage, InvocationRequest, InvocationResult, PROTOCOL_VERSION};

#[derive(Clone)]
pub struct LocalProcessExecutor {
    event_sink: Arc<dyn InvocationEventSink>,
    timeout: Duration,
}

impl std::fmt::Debug for LocalProcessExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalProcessExecutor").finish()
    }
}

impl Default for LocalProcessExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalProcessExecutor {
    pub fn new() -> Self {
        Self {
            event_sink: Arc::new(ConsoleInvocationEventSink),
            timeout: Duration::from_secs(30),
        }
    }

    pub fn with_event_sink(event_sink: Arc<dyn InvocationEventSink>) -> Self {
        Self {
            event_sink,
            timeout: Duration::from_secs(30),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
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
        command.envs(&target.env);

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

        loop {
            if child.try_wait()?.is_some() {
                break;
            }

            if started.elapsed() >= self.timeout {
                let _ = child.kill();
                let _ = child.wait_with_output();
                return Err(ExecutorError::ProcessTimedOut {
                    command: target.command.clone(),
                    timeout_ms: self.timeout.as_millis(),
                });
            }

            std::thread::sleep(Duration::from_millis(10));
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
                InvocationMessage::Event { event } => {
                    self.event_sink.record(&event);
                }
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

    #[test]
    fn times_out_long_running_process() {
        let request = InvocationRequest::new(json!({ "message": "hello" }));

        let target = ProcessTarget::new("sh").args(["-c", "cat >/dev/null && sleep 1"]);

        let executor =
            LocalProcessExecutor::new().with_timeout(std::time::Duration::from_millis(20));

        let error = executor
            .invoke(&target, &request)
            .expect_err("process should time out");

        assert!(matches!(
            error,
            crate::ExecutorError::ProcessTimedOut { .. }
        ));
    }
}
