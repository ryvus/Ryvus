use ryvus_protocol::ActionDefinition;

use ryvus_protocol::{AttemptId, ExecutionAttempt, ExecutionId, InvocationRequest};
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
    states: Mutex<HashMap<ExecutionId, ExecutionStatus>>,
}

#[derive(Debug, Clone)]
struct ExecutionStatus {
    state: ExecutionState,
    active_attempt_id: Option<AttemptId>,
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
        if request.attempt_number != 1 {
            return Err(ExecutionServiceError::InvalidInitialAttempt {
                attempt_number: request.attempt_number,
            });
        }

        let execution_id = request.execution_id.clone();
        self.set_state(&execution_id, ExecutionState::Pending);
        let target = match self.resolver.resolve(action) {
            Ok(target) => target,
            Err(error) => {
                self.set_state(&execution_id, ExecutionState::Failed);
                return Err(error.into());
            }
        };
        let mut delay = policy.retry.initial_delay;
        let mut attempt_request = request.clone();

        loop {
            let attempt = attempt_request.attempt();
            if !self.start_attempt(&attempt) {
                self.set_state(&execution_id, ExecutionState::Cancelled);
                return Err(ExecutionServiceError::CancellationRequested { execution_id });
            }
            let record = match self.executor.invoke_recorded(
                &target,
                &attempt_request,
                &ExecutionOptions {
                    timeout: policy.timeout,
                },
            ) {
                Ok(record) => record,
                Err(error) => {
                    let state = if self.execution_state(&execution_id)
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
                    self.finish_attempt(&attempt, state);
                    return Err(error.into());
                }
            };

            if let Err(error) = self.persistence.save_execution(&record) {
                self.finish_attempt(&attempt, ExecutionState::Failed);
                return Err(error.into());
            }

            if self.execution_state(&execution_id) == Some(ExecutionState::CancellationRequested) {
                self.finish_attempt(&attempt, ExecutionState::Cancelled);
                return Err(ExecutorError::RuntimeCancelled { attempt }.into());
            }

            let result = &record.result.invocation_result;
            if result.status == ryvus_protocol::InvocationStatus::Success {
                self.finish_attempt(&attempt, ExecutionState::Succeeded);
                return Ok(record);
            }

            let retryable = result.error.as_ref().is_some_and(|error| error.retryable);
            let attempts_remain = attempt.attempt_number < policy.retry.max_attempts;
            if !retryable || !attempts_remain {
                self.finish_attempt(&attempt, ExecutionState::Failed);
                return Ok(record);
            }

            self.finish_attempt(&attempt, ExecutionState::Pending);
            std::thread::sleep(delay);
            delay = delay.mul_f64(policy.retry.backoff);
            attempt_request = attempt_request.retry();

            if attempt_request.attempt_number > policy.retry.max_attempts {
                unreachable!("retry should only be created while attempts remain");
            }
        }
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

    pub fn cancel(&self, execution_id: &ExecutionId) -> ExecutionServiceResult<bool> {
        let active_attempt_id = {
            let mut states = self.states.lock().expect("execution states should lock");
            match states.get_mut(execution_id) {
                Some(status)
                    if matches!(
                        status.state,
                        ExecutionState::Pending | ExecutionState::Running
                    ) =>
                {
                    status.state = ExecutionState::CancellationRequested;
                    status.active_attempt_id.clone()
                }
                Some(status)
                    if matches!(
                        status.state,
                        ExecutionState::CancellationRequested | ExecutionState::Cancelled
                    ) =>
                {
                    return Ok(true);
                }
                Some(status)
                    if matches!(
                        status.state,
                        ExecutionState::Succeeded
                            | ExecutionState::Failed
                            | ExecutionState::TimedOut
                    ) =>
                {
                    return Ok(false);
                }
                None => return Ok(false),
                Some(_) => unreachable!("all execution states should be handled"),
            }
        };

        match active_attempt_id {
            Some(attempt_id) => self.executor.cancel(&attempt_id).map_err(Into::into),
            None => Ok(true),
        }
    }

    pub fn execution_state(&self, execution_id: &ExecutionId) -> Option<ExecutionState> {
        self.states
            .lock()
            .expect("execution states should lock")
            .get(execution_id)
            .map(|status| status.state)
    }

    pub fn shutdown(&self, grace: std::time::Duration) -> ExecutionServiceResult<()> {
        self.executor.shutdown(grace).map_err(Into::into)
    }

    fn set_state(&self, execution_id: &ExecutionId, state: ExecutionState) {
        // ponytail: terminal states remain in memory for local v0; add bounded persistence when run history is introduced.
        self.states
            .lock()
            .expect("execution states should lock")
            .entry(execution_id.clone())
            .and_modify(|status| status.state = state)
            .or_insert(ExecutionStatus {
                state,
                active_attempt_id: None,
            });
    }

    fn start_attempt(&self, attempt: &ExecutionAttempt) -> bool {
        let mut states = self.states.lock().expect("execution states should lock");
        let status = states
            .get_mut(&attempt.execution_id)
            .expect("execution state should exist before an attempt starts");
        if status.state == ExecutionState::CancellationRequested {
            false
        } else {
            status.state = ExecutionState::Running;
            status.active_attempt_id = Some(attempt.attempt_id.clone());
            true
        }
    }

    fn finish_attempt(&self, attempt: &ExecutionAttempt, state: ExecutionState) {
        let mut states = self.states.lock().expect("execution states should lock");
        let status = states
            .get_mut(&attempt.execution_id)
            .expect("execution state should exist while an attempt finishes");

        if status.active_attempt_id.as_ref() == Some(&attempt.attempt_id) {
            status.state = state;
            status.active_attempt_id = None;
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
        ActionDefinition, ActionKind, ApiAction, AttemptId, ExecutionAttempt, InvocationError,
        InvocationResult, InvocationStatus, RuntimeKind,
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
        let executor = FailsThenSucceeds::default();
        let attempts = Arc::clone(&executor.attempts);
        let persistence = RecordingPersistence::default();
        let persisted_attempts = Arc::clone(&persistence.attempts);
        let service = ExecutionService::new(StaticResolver, executor, persistence);
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
            service.execution_state(&request.execution_id),
            Some(ExecutionState::Succeeded)
        );
        let attempts = attempts.lock().unwrap();
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].attempt_number, 1);
        assert_eq!(attempts[1].attempt_number, 2);
        assert_eq!(attempts[0].execution_id, attempts[1].execution_id);
        assert_ne!(attempts[0].attempt_id, attempts[1].attempt_id);
        assert_eq!(*attempts, *persisted_attempts.lock().unwrap());
    }

    #[test]
    fn non_retryable_handler_failure_stops_after_one_attempt() {
        let executor = NonRetryableFailure::default();
        let attempts = Arc::clone(&executor.attempts);
        let service = ExecutionService::new(StaticResolver, executor, NoopPersistence);
        let request = InvocationRequest::new(json!({}));
        let policy = ExecutionPolicy {
            timeout: Duration::from_secs(3),
            retry: RetryPolicy {
                max_attempts: 3,
                initial_delay: Duration::ZERO,
                backoff: 1.0,
            },
        };

        let record = service.execute(&test_action(), &request, &policy).unwrap();

        assert_eq!(
            record.result.invocation_result.status,
            InvocationStatus::Failed
        );
        assert_eq!(attempts.lock().unwrap().len(), 1);
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
            timed_out.execution_state(&timeout_request.execution_id),
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
        let execution_id = cancel_request.execution_id.clone();
        let attempt_id = cancel_request.attempt_id.clone();
        let task_service = Arc::clone(&cancelled);
        let task =
            std::thread::spawn(move || task_service.execute(&action, &cancel_request, &policy));
        let (lock, changed) = &*blocking_state;
        let mut state = lock.lock().unwrap();
        while !state.started {
            state = changed.wait(state).unwrap();
        }
        drop(state);

        assert!(cancelled.cancel(&execution_id).unwrap());
        assert!(cancelled.cancel(&execution_id).unwrap());
        assert!(task.join().unwrap().is_err());
        assert_eq!(
            cancelled.execution_state(&execution_id),
            Some(ExecutionState::Cancelled)
        );
        assert_eq!(
            blocking_state.0.lock().unwrap().cancelled_attempt_id,
            Some(attempt_id)
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
    struct RecordingPersistence {
        attempts: Arc<Mutex<Vec<ExecutionAttempt>>>,
    }

    impl ExecutionPersistence for RecordingPersistence {
        fn save_execution(
            &self,
            record: &ExecutionRecord,
        ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            self.attempts.lock().unwrap().push(record.attempt.clone());
            Ok(())
        }
    }

    #[derive(Default)]
    struct FailsThenSucceeds {
        attempts: Arc<Mutex<Vec<ExecutionAttempt>>>,
    }

    impl Executor for FailsThenSucceeds {
        fn invoke(
            &self,
            _target: &RuntimeTarget,
            request: &InvocationRequest,
            _options: &ExecutionOptions,
        ) -> crate::ExecutorResult<ExecutionResult> {
            let mut attempts = self.attempts.lock().unwrap();
            attempts.push(request.attempt());
            let invocation_result = if attempts.len() == 1 {
                InvocationResult::failed(
                    request,
                    InvocationError::new("retryable", "try again", true),
                )
            } else {
                InvocationResult::success(request, json!({ "attempt": attempts.len() }))
            };

            Ok(ExecutionResult {
                invocation_result,
                stdout: String::new(),
                stderr: String::new(),
                duration: Duration::from_millis(1),
                exit_code: Some(0),
            })
        }
    }

    #[derive(Default)]
    struct NonRetryableFailure {
        attempts: Arc<Mutex<Vec<ExecutionAttempt>>>,
    }

    impl Executor for NonRetryableFailure {
        fn invoke(
            &self,
            _target: &RuntimeTarget,
            request: &InvocationRequest,
            _options: &ExecutionOptions,
        ) -> crate::ExecutorResult<ExecutionResult> {
            self.attempts.lock().unwrap().push(request.attempt());
            Ok(ExecutionResult {
                invocation_result: InvocationResult::failed(
                    request,
                    InvocationError::new("invalid", "do not retry", false),
                ),
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
                attempt: request.attempt(),
            })
        }
    }

    #[derive(Default)]
    struct BlockingExecutionState {
        started: bool,
        cancelled: bool,
        cancelled_attempt_id: Option<AttemptId>,
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
                attempt: request.attempt(),
            })
        }

        fn cancel(&self, attempt_id: &AttemptId) -> crate::ExecutorResult<bool> {
            let (lock, changed) = &*self.state;
            let mut state = lock.lock().unwrap();
            state.cancelled = true;
            state.cancelled_attempt_id = Some(attempt_id.clone());
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
