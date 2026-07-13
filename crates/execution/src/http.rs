use std::time::{Duration, Instant};

use ryvus_protocol::{AttemptId, InvocationRequest, InvocationResult, PROTOCOL_VERSION};

use crate::{
    ExecutionOptions, ExecutionResult, Executor, ExecutorError, ExecutorResult, RuntimeDisposition,
    RuntimeHandle, RuntimeLifecycle, RuntimeManager, RuntimeOutcome, RuntimeTarget,
};

pub struct HttpExecutor<M> {
    manager: M,
    lifecycle: RuntimeLifecycle,
}

impl<M> HttpExecutor<M> {
    pub fn new(manager: M) -> Self {
        Self {
            manager,
            lifecycle: RuntimeLifecycle::PerInvocation,
        }
    }

    pub fn with_lifecycle(mut self, lifecycle: RuntimeLifecycle) -> Self {
        self.lifecycle = lifecycle;
        self
    }
}

impl<M> Executor for HttpExecutor<M>
where
    M: RuntimeManager,
{
    fn invoke(
        &self,
        target: &RuntimeTarget,
        request: &InvocationRequest,
        options: &ExecutionOptions,
    ) -> ExecutorResult<ExecutionResult> {
        let started = Instant::now();
        if request.protocol_version != PROTOCOL_VERSION {
            return Err(ExecutorError::InvalidProtocolVersion {
                expected: PROTOCOL_VERSION.to_string(),
                actual: request.protocol_version.clone(),
            });
        }

        let handle =
            self.manager
                .acquire(target, &request.attempt(), self.lifecycle, options.timeout)?;
        let remaining = options.timeout.saturating_sub(started.elapsed());
        let invocation = self.invoke_acquired(&handle, request, remaining);
        let runtime_outcome = match &invocation {
            Ok(result) if result.status == ryvus_protocol::InvocationStatus::Failed => {
                RuntimeOutcome::HandlerFailure
            }
            Ok(_) => RuntimeOutcome::Success,
            Err(ExecutorError::ProcessTimedOut { .. }) => RuntimeOutcome::TimedOut,
            Err(_) => RuntimeOutcome::InfrastructureFailure,
        };
        let release = self.manager.release(handle, runtime_outcome);

        match (invocation, release) {
            (_, Ok(release)) if release.disposition == RuntimeDisposition::Cancelled => {
                Err(ExecutorError::RuntimeCancelled {
                    attempt: request.attempt(),
                })
            }
            (_, Ok(release)) if release.disposition == RuntimeDisposition::TimedOut => {
                Err(ExecutorError::RuntimeTimedOut {
                    attempt: request.attempt(),
                })
            }
            (Ok(invocation_result), Ok(release)) => Ok(ExecutionResult {
                invocation_result,
                stdout: release.exit.stdout,
                stderr: release.exit.stderr,
                duration: started.elapsed(),
                exit_code: release.exit.exit_code,
            }),
            (Err(error), Ok(_)) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Err(invocation), Err(release)) => Err(ExecutorError::InvocationAndRelease {
                invocation: invocation.to_string(),
                release: release.to_string(),
            }),
        }
    }

    fn cancel(&self, attempt_id: &AttemptId) -> ExecutorResult<bool> {
        self.manager.cancel(attempt_id)
    }

    fn shutdown(&self, grace: Duration) -> ExecutorResult<()> {
        self.manager.shutdown(grace)
    }
}

impl<M> HttpExecutor<M>
where
    M: RuntimeManager,
{
    fn invoke_acquired(
        &self,
        handle: &RuntimeHandle,
        request: &InvocationRequest,
        timeout: Duration,
    ) -> ExecutorResult<InvocationResult> {
        if timeout.is_zero() {
            return Err(ExecutorError::ProcessTimedOut {
                attempt: request.attempt(),
                command: handle.endpoint.clone(),
                timeout_ms: 0,
            });
        }

        let endpoint = handle.endpoint.clone();
        let invocation_url = format!("{}/invoke", endpoint.trim_end_matches('/'));
        let request_body = request.clone();
        let expected_attempt = request.attempt();
        let timeout_attempt = expected_attempt.clone();
        let result = std::thread::spawn(move || {
            let response = reqwest::blocking::Client::builder()
                .timeout(timeout)
                .build()?
                .post(invocation_url)
                .json(&request_body)
                .send()
                .map_err(|error| {
                    if error.is_timeout() {
                        ExecutorError::ProcessTimedOut {
                            attempt: timeout_attempt,
                            command: endpoint,
                            timeout_ms: timeout.as_millis(),
                        }
                    } else {
                        ExecutorError::Http(error)
                    }
                })?;
            let status = response.status();
            let body = response.text()?;
            if status.as_u16() == 409 {
                return Err(ExecutorError::RuntimeBusy);
            }
            if !status.is_success() {
                return Err(ExecutorError::HttpStatus {
                    status: status.as_u16(),
                    body,
                });
            }
            Ok(serde_json::from_str::<InvocationResult>(&body)?)
        })
        .join()
        .map_err(|_| ExecutorError::HttpWorkerPanicked)??;
        if result.protocol_version != PROTOCOL_VERSION {
            return Err(ExecutorError::InvalidProtocolVersion {
                expected: PROTOCOL_VERSION.to_string(),
                actual: result.protocol_version,
            });
        }
        if result.attempt() != expected_attempt {
            return Err(ExecutorError::AttemptIdentityMismatch {
                expected: expected_attempt,
                actual: result.attempt(),
            });
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    use serde_json::json;

    use super::*;
    use crate::{MockRuntimeManager, RuntimeExit, RuntimeRelease};

    #[test]
    fn releases_runtime_after_failed_handler_result() {
        let request = InvocationRequest::new(json!({}));
        let body = serde_json::to_string(&InvocationResult::failed(
            &request,
            ryvus_protocol::InvocationError::new("handler_error", "boom", false),
        ))
        .unwrap();
        let endpoint = one_response_server("200 OK", &body);
        let executor = executor_with_expected_release(endpoint, RuntimeOutcome::HandlerFailure);

        let result = executor
            .invoke(
                &RuntimeTarget::http("unused"),
                &request,
                &ExecutionOptions {
                    timeout: Duration::from_secs(1),
                },
            )
            .unwrap();

        assert_eq!(
            result.invocation_result.status,
            ryvus_protocol::InvocationStatus::Failed
        );
    }

    #[test]
    fn releases_runtime_after_malformed_response() {
        let endpoint = one_response_server("200 OK", "not-json");
        let executor =
            executor_with_expected_release(endpoint, RuntimeOutcome::InfrastructureFailure);
        let request = InvocationRequest::new(json!({}));

        assert!(executor
            .invoke(
                &RuntimeTarget::http("unused"),
                &request,
                &ExecutionOptions {
                    timeout: Duration::from_secs(1),
                },
            )
            .is_err());
    }

    #[test]
    fn releases_runtime_after_connection_failure() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let executor =
            executor_with_expected_release(endpoint, RuntimeOutcome::InfrastructureFailure);
        let request = InvocationRequest::new(json!({}));

        assert!(executor
            .invoke(
                &RuntimeTarget::http("unused"),
                &request,
                &ExecutionOptions {
                    timeout: Duration::from_millis(100),
                },
            )
            .is_err());
    }

    #[test]
    fn timeout_releases_runtime_as_timed_out() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            thread::sleep(Duration::from_millis(200));
        });
        let mut manager = MockRuntimeManager::new();
        manager
            .expect_acquire()
            .return_once(move |_, attempt, _, _| {
                Ok(RuntimeHandle::existing(attempt.clone(), endpoint))
            });
        manager
            .expect_release()
            .withf(|_, outcome| *outcome == RuntimeOutcome::TimedOut)
            .return_once(|_, _| {
                Ok(RuntimeRelease {
                    exit: RuntimeExit::default(),
                    disposition: RuntimeDisposition::TimedOut,
                })
            });
        let executor = HttpExecutor::new(manager);
        let request = InvocationRequest::new(json!({}));

        let error = executor
            .invoke(
                &RuntimeTarget::http("unused"),
                &request,
                &ExecutionOptions {
                    timeout: Duration::from_millis(30),
                },
            )
            .unwrap_err();

        assert!(matches!(error, ExecutorError::RuntimeTimedOut { .. }));
    }

    fn executor_with_expected_release(
        endpoint: String,
        expected_outcome: RuntimeOutcome,
    ) -> HttpExecutor<MockRuntimeManager> {
        let mut manager = MockRuntimeManager::new();
        manager
            .expect_acquire()
            .times(1)
            .returning(move |_, attempt, _, _| {
                Ok(RuntimeHandle::existing(attempt.clone(), endpoint.clone()))
            });
        manager
            .expect_release()
            .withf(move |_, outcome| *outcome == expected_outcome)
            .times(1)
            .returning(|_, _| {
                Ok(RuntimeRelease {
                    exit: RuntimeExit::default(),
                    disposition: RuntimeDisposition::Reusable,
                })
            });
        HttpExecutor::new(manager)
    }

    fn one_response_server(status: &str, body: &str) -> String {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let status = status.to_string();
        let body = body.to_string();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 8192];
            let _ = stream.read(&mut request);
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        endpoint
    }
}
