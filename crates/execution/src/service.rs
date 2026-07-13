use ryvus_protocol::ActionDefinition;

use ryvus_protocol::InvocationRequest;
use std::{collections::HashMap, sync::Mutex};

use crate::{
    ExecutionOptions, ExecutionPersistence, ExecutionRecord, ExecutionServiceError,
    ExecutionServiceResult, ExecutionState, Executor, ExecutorError, RecordingExecutor,
    RuntimeResolver,
};

#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionPolicy {
    pub timeout: std::time::Duration,
    pub retry: RetryPolicy,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub initial_delay: std::time::Duration,
    pub backoff: f64,
}

impl ExecutionPolicy {
    pub fn from_action_policy(
        policy: &ryvus_protocol::ActionExecutionPolicy,
    ) -> ExecutionServiceResult<Self> {
        if policy.retry.max_attempts == 0 {
            return Err(ExecutionServiceError::InvalidPolicy(
                "retry.max_attempts must be greater than 0".to_string(),
            ));
        }

        if policy.retry.backoff <= 0.0 {
            return Err(ExecutionServiceError::InvalidPolicy(
                "retry.backoff must be greater than 0".to_string(),
            ));
        }

        Ok(Self {
            timeout: parse_duration(&policy.timeout)?,
            retry: RetryPolicy {
                max_attempts: policy.retry.max_attempts,
                initial_delay: parse_duration(&policy.retry.initial_delay)?,
                backoff: policy.retry.backoff,
            },
        })
    }
}

fn parse_duration(value: &str) -> ExecutionServiceResult<std::time::Duration> {
    let (number, unit) = if let Some(number) = value.strip_suffix("ms") {
        (number, "ms")
    } else if let Some(number) = value.strip_suffix('s') {
        (number, "s")
    } else if let Some(number) = value.strip_suffix('m') {
        (number, "m")
    } else {
        return Err(ExecutionServiceError::InvalidPolicy(format!(
            "unsupported duration '{value}'"
        )));
    };

    let amount = number
        .parse::<u64>()
        .map_err(|_| ExecutionServiceError::InvalidPolicy(format!("invalid duration '{value}'")))?;

    if amount == 0 {
        return Err(ExecutionServiceError::InvalidPolicy(format!(
            "duration '{value}' must be greater than zero"
        )));
    }

    Ok(match unit {
        "ms" => std::time::Duration::from_millis(amount),
        "s" => std::time::Duration::from_secs(amount),
        "m" => std::time::Duration::from_secs(amount * 60),
        _ => unreachable!("unit is checked above"),
    })
}

pub struct ExecutionService<RR, E, EP> {
    resolver: RR,
    executor: RecordingExecutor<E>,
    persistence: EP,
    states: Mutex<HashMap<String, ExecutionState>>,
}

impl<RR, E, EP> ExecutionService<RR, E, EP>
where
    RR: RuntimeResolver,
    E: Executor,
    EP: ExecutionPersistence,
{
    pub fn new(resolver: RR, executor: E, persistence: EP) -> Self {
        Self {
            resolver,
            executor: RecordingExecutor::new(executor),
            persistence,
            states: Mutex::new(HashMap::new()),
        }
    }

    pub fn execute(
        &self,
        action: &ActionDefinition,
        request: &InvocationRequest,
        policy: &ExecutionPolicy,
    ) -> ExecutionServiceResult<ExecutionRecord> {
        self.set_state(&request.invocation_id, ExecutionState::Pending);
        let target = match self.resolver.resolve(action) {
            Ok(target) => target,
            Err(error) => {
                self.set_state(&request.invocation_id, ExecutionState::Failed);
                return Err(error.into());
            }
        };
        let mut delay = policy.retry.initial_delay;
        let mut last_record = None;

        for attempt in 1..=policy.retry.max_attempts {
            if !self.start_attempt(&request.invocation_id) {
                self.set_state(&request.invocation_id, ExecutionState::Cancelled);
                return Err(ExecutorError::RuntimeCancelled {
                    invocation_id: request.invocation_id.clone(),
                }
                .into());
            }
            let record = match self.executor.invoke_recorded(
                &target,
                request,
                &ExecutionOptions {
                    timeout: policy.timeout,
                },
            ) {
                Ok(record) => record,
                Err(error) => {
                    let state = if self.execution_state(&request.invocation_id)
                        == Some(ExecutionState::CancellationRequested)
                        || matches!(error, ExecutorError::RuntimeCancelled { .. })
                    {
                        ExecutionState::Cancelled
                    } else if matches!(
                        error,
                        ExecutorError::ProcessTimedOut { .. }
                            | ExecutorError::RuntimeTimedOut { .. }
                    ) {
                        ExecutionState::TimedOut
                    } else {
                        ExecutionState::Failed
                    };
                    self.set_state(&request.invocation_id, state);
                    return Err(error.into());
                }
            };
            let failed =
                record.result.invocation_result.status == ryvus_protocol::InvocationStatus::Failed;

            if let Err(error) = self.persistence.save_execution(&record) {
                self.set_state(&request.invocation_id, ExecutionState::Failed);
                return Err(error.into());
            }

            if !failed {
                self.set_state(&request.invocation_id, ExecutionState::Succeeded);
                return Ok(record);
            }

            last_record = Some(record);

            if attempt < policy.retry.max_attempts {
                if self.execution_state(&request.invocation_id)
                    == Some(ExecutionState::CancellationRequested)
                {
                    self.set_state(&request.invocation_id, ExecutionState::Cancelled);
                    return Err(ExecutorError::RuntimeCancelled {
                        invocation_id: request.invocation_id.clone(),
                    }
                    .into());
                }
                std::thread::sleep(delay);
                delay = delay.mul_f64(policy.retry.backoff);
            }
        }

        self.set_state(&request.invocation_id, ExecutionState::Failed);
        Ok(last_record.expect("at least one execution attempt should run"))
    }

    pub fn execute_event(
        &self,
        action: &ActionDefinition,
        event: serde_json::Value,
    ) -> ExecutionServiceResult<ExecutionRecord> {
        let request = InvocationRequest::new(event);
        let policy = ExecutionPolicy::from_action_policy(&action.policy)?;
        self.execute(action, &request, &policy)
    }

    pub fn cancel(&self, invocation_id: &str) -> ExecutionServiceResult<bool> {
        {
            let mut states = self.states.lock().expect("execution states should lock");
            match states.get(invocation_id).copied() {
                Some(ExecutionState::Pending | ExecutionState::Running) => {
                    states.insert(
                        invocation_id.to_string(),
                        ExecutionState::CancellationRequested,
                    );
                }
                Some(ExecutionState::CancellationRequested | ExecutionState::Cancelled) => {
                    return Ok(true);
                }
                Some(
                    ExecutionState::Succeeded | ExecutionState::Failed | ExecutionState::TimedOut,
                )
                | None => return Ok(false),
            }
        }

        let _ = self.executor.cancel(invocation_id)?;
        Ok(true)
    }

    pub fn execution_state(&self, invocation_id: &str) -> Option<ExecutionState> {
        self.states
            .lock()
            .expect("execution states should lock")
            .get(invocation_id)
            .copied()
    }

    pub fn shutdown(&self, grace: std::time::Duration) -> ExecutionServiceResult<()> {
        self.executor.shutdown(grace).map_err(Into::into)
    }

    fn set_state(&self, invocation_id: &str, state: ExecutionState) {
        // ponytail: terminal states remain in memory for local v0; add bounded persistence when run history is introduced.
        self.states
            .lock()
            .expect("execution states should lock")
            .insert(invocation_id.to_string(), state);
    }

    fn start_attempt(&self, invocation_id: &str) -> bool {
        let mut states = self.states.lock().expect("execution states should lock");
        if states.get(invocation_id) == Some(&ExecutionState::CancellationRequested) {
            false
        } else {
            states.insert(invocation_id.to_string(), ExecutionState::Running);
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Condvar, Mutex},
        time::Duration,
    };

    use ryvus_protocol::{
        ActionDefinition, ActionKind, ApiAction, InvocationResult, InvocationStatus, RuntimeKind,
        PROTOCOL_VERSION,
    };
    use serde_json::json;

    use crate::{ExecutionPersistence, ExecutionResult, Executor, RuntimeTarget};

    use super::*;

    #[test]
    fn parses_policy_duration_units() {
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("2m").unwrap(), Duration::from_secs(120));
    }

    #[test]
    fn rejects_invalid_policy_values() {
        assert!(parse_duration("0s").is_err());
        assert!(parse_duration("1h").is_err());

        let policy = ryvus_protocol::ActionExecutionPolicy {
            timeout: "3s".to_string(),
            retry: ryvus_protocol::ActionRetryPolicy {
                max_attempts: 0,
                initial_delay: "1s".to_string(),
                backoff: 2.0,
            },
        };

        assert!(ExecutionPolicy::from_action_policy(&policy).is_err());
    }

    #[test]
    fn retries_until_success() {
        let service = ExecutionService::new(
            StaticResolver,
            FailsThenSucceeds::default(),
            NoopPersistence,
        );
        let action = test_action();
        let request = InvocationRequest::new(json!({}));
        let policy = ExecutionPolicy {
            timeout: Duration::from_secs(3),
            retry: RetryPolicy {
                max_attempts: 2,
                initial_delay: Duration::from_millis(1),
                backoff: 1.0,
            },
        };

        let record = service.execute(&action, &request, &policy).unwrap();

        assert_eq!(
            record.result.invocation_result.status,
            InvocationStatus::Success
        );
        assert_eq!(
            service.execution_state(&request.invocation_id),
            Some(ExecutionState::Succeeded)
        );
    }

    #[test]
    fn timeout_and_cancellation_have_distinct_terminal_states() {
        let action = test_action();
        let policy = ExecutionPolicy {
            timeout: Duration::from_millis(10),
            retry: RetryPolicy {
                max_attempts: 1,
                initial_delay: Duration::ZERO,
                backoff: 1.0,
            },
        };

        let timed_out = ExecutionService::new(StaticResolver, TimedOutExecutor, NoopPersistence);
        let timeout_request = InvocationRequest::new(json!({}));
        assert!(timed_out
            .execute(&action, &timeout_request, &policy)
            .is_err());
        assert_eq!(
            timed_out.execution_state(&timeout_request.invocation_id),
            Some(ExecutionState::TimedOut)
        );

        let blocking = BlockingExecutor::default();
        let blocking_state = Arc::clone(&blocking.state);
        let cancelled = Arc::new(ExecutionService::new(
            StaticResolver,
            blocking,
            NoopPersistence,
        ));
        let cancel_request = InvocationRequest::new(json!({}));
        let invocation_id = cancel_request.invocation_id.clone();
        let task_service = Arc::clone(&cancelled);
        let task =
            std::thread::spawn(move || task_service.execute(&action, &cancel_request, &policy));
        let (lock, changed) = &*blocking_state;
        let mut state = lock.lock().unwrap();
        while !state.started {
            state = changed.wait(state).unwrap();
        }
        drop(state);

        assert!(cancelled.cancel(&invocation_id).unwrap());
        assert!(cancelled.cancel(&invocation_id).unwrap());
        assert!(task.join().unwrap().is_err());
        assert_eq!(
            cancelled.execution_state(&invocation_id),
            Some(ExecutionState::Cancelled)
        );
    }

    #[derive(Clone)]
    struct StaticResolver;

    impl RuntimeResolver for StaticResolver {
        fn resolve(&self, _action: &ActionDefinition) -> crate::ExecutorResult<RuntimeTarget> {
            Ok(RuntimeTarget::http("http://runtime.test"))
        }
    }

    struct NoopPersistence;

    impl ExecutionPersistence for NoopPersistence {
        fn save_execution(
            &self,
            _record: &ExecutionRecord,
        ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct FailsThenSucceeds {
        attempts: Mutex<u32>,
    }

    impl Executor for FailsThenSucceeds {
        fn invoke(
            &self,
            _target: &RuntimeTarget,
            request: &InvocationRequest,
            _options: &ExecutionOptions,
        ) -> crate::ExecutorResult<ExecutionResult> {
            let mut attempts = self.attempts.lock().unwrap();
            *attempts += 1;
            let status = if *attempts == 1 {
                InvocationStatus::Failed
            } else {
                InvocationStatus::Success
            };

            Ok(ExecutionResult {
                invocation_result: InvocationResult {
                    protocol_version: PROTOCOL_VERSION.to_string(),
                    invocation_id: request.invocation_id.clone(),
                    status,
                    output: Some(json!({ "attempt": *attempts })),
                    error: None,
                },
                stdout: String::new(),
                stderr: String::new(),
                duration: Duration::from_millis(1),
                exit_code: Some(0),
            })
        }
    }

    struct TimedOutExecutor;

    impl Executor for TimedOutExecutor {
        fn invoke(
            &self,
            _target: &RuntimeTarget,
            request: &InvocationRequest,
            _options: &ExecutionOptions,
        ) -> crate::ExecutorResult<ExecutionResult> {
            Err(ExecutorError::RuntimeTimedOut {
                invocation_id: request.invocation_id.clone(),
            })
        }
    }

    #[derive(Default)]
    struct BlockingExecutionState {
        started: bool,
        cancelled: bool,
    }

    #[derive(Default)]
    struct BlockingExecutor {
        state: Arc<(Mutex<BlockingExecutionState>, Condvar)>,
    }

    impl Executor for BlockingExecutor {
        fn invoke(
            &self,
            _target: &RuntimeTarget,
            request: &InvocationRequest,
            _options: &ExecutionOptions,
        ) -> crate::ExecutorResult<ExecutionResult> {
            let (lock, changed) = &*self.state;
            let mut state = lock.lock().unwrap();
            state.started = true;
            changed.notify_all();
            while !state.cancelled {
                state = changed.wait(state).unwrap();
            }
            Err(ExecutorError::RuntimeCancelled {
                invocation_id: request.invocation_id.clone(),
            })
        }

        fn cancel(&self, _invocation_id: &str) -> crate::ExecutorResult<bool> {
            let (lock, changed) = &*self.state;
            let mut state = lock.lock().unwrap();
            state.cancelled = true;
            changed.notify_all();
            Ok(true)
        }
    }

    fn test_action() -> ActionDefinition {
        ActionDefinition {
            runtime: RuntimeKind::Python,
            kind: ActionKind::Api(ApiAction {
                method: "POST".to_string(),
                path: "/test".to_string(),
                consumes: vec!["application/json".to_string()],
                produces: vec!["application/json".to_string()],
                request_schema: None,
                response_schema: None,
                query_params: Vec::new(),
                authorizer: None,
            }),
            source: "src/test.py".into(),
            entrypoint: "test".to_string(),
            name: Some("test".to_string()),
            policy: ryvus_protocol::ActionExecutionPolicy::default(),
        }
    }
}
