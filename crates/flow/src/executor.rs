use std::{collections::HashMap, sync::Arc};

use ryvus_execution::{
    ExecutionPersistence, ExecutionPolicy, ExecutionRecord, ExecutionService, Executor,
    RuntimeResolver,
};
use ryvus_protocol::{
    ActionDefinition, InvocationContext, InvocationEvent, InvocationMessage, InvocationRequest,
    InvocationStatus, LogLevel, PROTOCOL_VERSION,
};
use serde_json::{json, Value};

use crate::{
    error::{FlowError, FlowResult},
    jsonpath::{evaluate_condition, resolve_jsonpaths, FlowContext},
    model::{
        FlowDefinition, FlowEndStatus, FlowExecution, FlowExecutionStatus, FlowSpec, FlowStep,
        FlowStepExecution, FlowStepLog, FlowStepStatus, StartFlowResponse,
    },
    store::FlowStateStore,
    validation::validate_flow_spec,
};

pub trait FlowStepExecutor: Send + Sync + 'static {
    fn execute_flow_step(
        &self,
        action: &ActionDefinition,
        request: &InvocationRequest,
        policy: &ExecutionPolicy,
    ) -> FlowResult<ExecutionRecord>;

    fn cancel_flow_step(&self, _invocation_id: &str) -> FlowResult<bool> {
        Ok(false)
    }
}

impl<RR, E, EP> FlowStepExecutor for ExecutionService<RR, E, EP>
where
    RR: RuntimeResolver + Send + Sync + 'static,
    E: Executor + Send + Sync + 'static,
    EP: ExecutionPersistence + Send + Sync + 'static,
{
    fn execute_flow_step(
        &self,
        action: &ActionDefinition,
        request: &InvocationRequest,
        policy: &ExecutionPolicy,
    ) -> FlowResult<ExecutionRecord> {
        self.execute(action, request, policy)
            .map_err(|error| FlowError::ExecutionFailed {
                action: action_key(action),
                message: error.to_string(),
            })
    }

    fn cancel_flow_step(&self, invocation_id: &str) -> FlowResult<bool> {
        self.cancel(invocation_id)
            .map_err(|error| FlowError::ExecutionFailed {
                action: invocation_id.to_string(),
                message: error.to_string(),
            })
    }
}

#[derive(Clone)]
pub struct FlowService<S, E> {
    runner: Arc<FlowRunner<S, E>>,
}

impl<S, E> FlowService<S, E>
where
    S: FlowStateStore,
    E: FlowStepExecutor,
{
    pub fn new(
        spec: FlowSpec,
        actions: Vec<ActionDefinition>,
        store: Arc<S>,
        executor: Arc<E>,
    ) -> FlowResult<Self> {
        validate_flow_spec(&spec)?;
        Ok(Self {
            runner: Arc::new(FlowRunner::new(spec, actions, store, executor)),
        })
    }

    pub fn list_flows(&self) -> Vec<FlowDefinition> {
        self.runner.spec.flows.clone()
    }

    pub fn start_flow(&self, key: &str, input: Value) -> FlowResult<StartFlowResponse> {
        self.runner.start_flow(key, input)
    }

    pub fn get_run(&self, id: &str) -> FlowResult<FlowExecution> {
        self.runner.store.get(id)
    }

    pub fn list_runs(&self) -> FlowResult<Vec<FlowExecution>> {
        self.runner.store.list()
    }

    pub fn cancel_run(&self, id: &str) -> FlowResult<FlowExecution> {
        self.runner.cancel_run(id)
    }

    pub fn retry_failed_step(&self, id: &str, step_key: &str) -> FlowResult<FlowExecution> {
        self.runner.retry_failed_step(id, step_key)
    }
}

pub struct FlowRunner<S, E> {
    spec: FlowSpec,
    actions: Vec<ActionDefinition>,
    store: Arc<S>,
    executor: Arc<E>,
}

impl<S, E> FlowRunner<S, E>
where
    S: FlowStateStore,
    E: FlowStepExecutor,
{
    pub fn new(
        spec: FlowSpec,
        actions: Vec<ActionDefinition>,
        store: Arc<S>,
        executor: Arc<E>,
    ) -> Self {
        Self {
            spec,
            actions,
            store,
            executor,
        }
    }

    pub fn start_flow(&self, key: &str, input: Value) -> FlowResult<StartFlowResponse> {
        let flow = self
            .spec
            .flows
            .iter()
            .find(|flow| flow.key == key)
            .cloned()
            .ok_or_else(|| FlowError::FlowNotFound {
                key: key.to_string(),
            })?;
        let id = uuid::Uuid::new_v4().to_string();

        self.store.create(FlowExecution {
            id: id.clone(),
            flow_key: flow.key.clone(),
            status: FlowExecutionStatus::Queued,
            input: input.clone(),
            output: Value::Null,
            error: None,
            steps: Vec::new(),
        })?;

        let store = Arc::clone(&self.store);
        let executor = Arc::clone(&self.executor);
        let actions = self.actions.clone();
        let run_id = id.clone();
        let error_store = Arc::clone(&store);
        let error_id = id.clone();

        tokio::spawn(async move {
            match tokio::task::spawn_blocking(move || {
                run_flow_steps(
                    &run_id,
                    &flow,
                    input,
                    &actions,
                    store.as_ref(),
                    executor.as_ref(),
                )
            })
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    let _ = error_store.update_status(
                        &error_id,
                        FlowExecutionStatus::Failed,
                        Value::Null,
                        Some(error.to_string()),
                    );
                }
                Err(error) => {
                    let _ = error_store.update_status(
                        &error_id,
                        FlowExecutionStatus::Failed,
                        Value::Null,
                        Some(format!("flow task failed: {error}")),
                    );
                }
            }
        });

        Ok(StartFlowResponse {
            id,
            flow_key: key.to_string(),
            status: FlowExecutionStatus::Queued,
        })
    }

    pub fn cancel_run(&self, id: &str) -> FlowResult<FlowExecution> {
        if let Some(invocation_id) = self.store.active_invocation(id)? {
            let _ = self.executor.cancel_flow_step(&invocation_id);
        }

        self.store.cancel(id)
    }

    pub fn retry_failed_step(&self, id: &str, step_key: &str) -> FlowResult<FlowExecution> {
        let execution = self.store.get(id)?;

        if execution.status == FlowExecutionStatus::Cancelled {
            return Err(FlowError::InvalidFlow {
                flow: execution.flow_key,
                message: "cancelled flow runs cannot be retried".to_string(),
            });
        }

        if execution.status != FlowExecutionStatus::Failed {
            return Err(FlowError::InvalidFlow {
                flow: execution.flow_key,
                message: "only failed flow runs can retry failed steps".to_string(),
            });
        }

        let failed_step_index = execution
            .steps
            .iter()
            .rposition(|step| step.key == step_key)
            .ok_or_else(|| FlowError::InvalidStep {
                flow: execution.flow_key.clone(),
                step: step_key.to_string(),
                message: "step was not recorded in this run".to_string(),
            })?;
        let failed_step = &execution.steps[failed_step_index];

        if failed_step.status != FlowStepStatus::Failed {
            return Err(FlowError::InvalidStep {
                flow: execution.flow_key.clone(),
                step: step_key.to_string(),
                message: "latest step entry is not failed".to_string(),
            });
        }

        let flow = self
            .spec
            .flows
            .iter()
            .find(|flow| flow.key == execution.flow_key)
            .cloned()
            .ok_or_else(|| FlowError::FlowNotFound {
                key: execution.flow_key.clone(),
            })?;

        let mut context = FlowContext::new(execution.input.clone());
        for step in execution.steps.iter().take(failed_step_index) {
            context.record_step(
                &step.key,
                flow_step_status_label(step.status),
                step.output.clone(),
                step.error.clone(),
            );
        }

        run_flow_steps_from(
            id,
            &flow,
            step_key,
            failed_step.input.clone(),
            context,
            &self.actions,
            self.store.as_ref(),
            self.executor.as_ref(),
        )?;

        self.store.get(id)
    }
}

fn run_flow_steps<S, E>(
    run_id: &str,
    flow: &FlowDefinition,
    input: Value,
    actions: &[ActionDefinition],
    store: &S,
    executor: &E,
) -> FlowResult<()>
where
    S: FlowStateStore,
    E: FlowStepExecutor,
{
    let context = FlowContext::new(input.clone());
    let start_step = flow
        .steps
        .first()
        .ok_or_else(|| FlowError::InvalidFlow {
            flow: flow.key.clone(),
            message: "at least one step is required".to_string(),
        })?
        .key
        .clone();

    run_flow_steps_from(
        run_id,
        flow,
        &start_step,
        input,
        context,
        actions,
        store,
        executor,
    )
}

fn run_flow_steps_from<S, E>(
    run_id: &str,
    flow: &FlowDefinition,
    start_step: &str,
    input: Value,
    mut context: FlowContext,
    actions: &[ActionDefinition],
    store: &S,
    executor: &E,
) -> FlowResult<()>
where
    S: FlowStateStore,
    E: FlowStepExecutor,
{
    store.update_status(run_id, FlowExecutionStatus::Running, Value::Null, None)?;
    let steps = flow
        .steps
        .iter()
        .map(|step| (step.key.as_str(), step))
        .collect::<HashMap<_, _>>();
    let mut current = steps
        .get(start_step)
        .copied()
        .ok_or_else(|| FlowError::InvalidStep {
            flow: flow.key.clone(),
            step: start_step.to_string(),
            message: "retry start step was not found".to_string(),
        })?;
    let mut step_input = input;

    loop {
        let action = resolve_action(actions, &current.action)?;
        let context_json = context.as_json();
        let mut params = current.params.clone();
        let mut config = current.config.clone();
        resolve_jsonpaths(&mut params, &context_json)?;
        resolve_jsonpaths(&mut config, &context_json)?;

        let request = flow_request(
            &flow.key,
            run_id,
            current,
            step_input.clone(),
            params,
            config,
        );
        let policy = ExecutionPolicy::from_action_policy(&current.policy).map_err(|error| {
            FlowError::InvalidStep {
                flow: flow.key.clone(),
                step: current.key.clone(),
                message: error.to_string(),
            }
        })?;
        store.set_active_invocation(run_id, Some(request.invocation_id.clone()))?;
        let record = match executor.execute_flow_step(action, &request, &policy) {
            Ok(record) => record,
            Err(_error) if store.is_cancelled(run_id)? => {
                store.set_active_invocation(run_id, None)?;
                store.push_step(
                    run_id,
                    FlowStepExecution {
                        key: current.key.clone(),
                        action: current.action.clone(),
                        status: FlowStepStatus::Cancelled,
                        attempts: 1,
                        invocation_id: Some(request.invocation_id),
                        input: step_input,
                        output: Value::Null,
                        error: None,
                        logs: Vec::new(),
                    },
                )?;
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        store.set_active_invocation(run_id, None)?;
        if store.is_cancelled(run_id)? {
            store.push_step(
                run_id,
                FlowStepExecution {
                    key: current.key.clone(),
                    action: current.action.clone(),
                    status: FlowStepStatus::Cancelled,
                    attempts: 1,
                    invocation_id: Some(request.invocation_id),
                    input: step_input,
                    output: Value::Null,
                    error: None,
                    logs: Vec::new(),
                },
            )?;
            return Ok(());
        }
        let logs = flow_step_logs(&record);
        let result = record.result.invocation_result;
        let output = result.output.clone().unwrap_or(Value::Null);
        let error = result.error.as_ref().map(|error| error.message.clone());
        let step_status = match result.status {
            InvocationStatus::Success => FlowStepStatus::Succeeded,
            InvocationStatus::Failed => FlowStepStatus::Failed,
        };

        store.push_step(
            run_id,
            FlowStepExecution {
                key: current.key.clone(),
                action: current.action.clone(),
                status: step_status,
                attempts: current.policy.retry.max_attempts,
                invocation_id: Some(result.invocation_id.clone()),
                input: step_input.clone(),
                output: output.clone(),
                error: error.clone(),
                logs,
            },
        )?;

        context.record_step(
            &current.key,
            flow_step_status_label(step_status),
            output.clone(),
            error.clone(),
        );

        if step_status == FlowStepStatus::Failed {
            if let Some(next) = &current.on_error {
                current = steps
                    .get(next.as_str())
                    .copied()
                    .ok_or_else(|| missing_step(flow, current, next))?;
                step_input = json!({ "error": error, "previous": output });
                continue;
            }

            store.update_status(
                run_id,
                FlowExecutionStatus::Failed,
                Value::Null,
                error.or_else(|| Some("flow step failed".to_string())),
            )?;
            return Ok(());
        }

        if let Some(end) = current.end {
            let status = match end {
                FlowEndStatus::Succeeded => FlowExecutionStatus::Succeeded,
                FlowEndStatus::Failed => FlowExecutionStatus::Failed,
            };
            store.update_status(run_id, status, output, None)?;
            return Ok(());
        }

        let Some(next) = next_step(current, &context.as_json())? else {
            store.update_status(run_id, FlowExecutionStatus::Succeeded, output, None)?;
            return Ok(());
        };

        current = steps
            .get(next)
            .copied()
            .ok_or_else(|| missing_step(flow, current, next))?;
        step_input = output;
    }
}

fn flow_request(
    flow_key: &str,
    run_id: &str,
    step: &FlowStep,
    input: Value,
    params: Value,
    config: Value,
) -> InvocationRequest {
    InvocationRequest {
        protocol_version: PROTOCOL_VERSION.to_string(),
        invocation_id: uuid::Uuid::new_v4().to_string(),
        event: input,
        context: InvocationContext {
            metadata: json!({
                "flow": {
                    "flow_key": flow_key,
                    "execution_id": run_id,
                    "step_key": step.key,
                },
                "params": params,
                "config": config,
            }),
        },
    }
}

fn flow_step_logs(record: &ExecutionRecord) -> Vec<FlowStepLog> {
    record
        .result
        .stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<InvocationMessage>(line.trim()).ok())
        .filter_map(|message| match message {
            InvocationMessage::Event {
                event: InvocationEvent::Log(log),
            } if log.invocation_id == record.invocation_id => Some(FlowStepLog {
                level: log_level_label(log.level).to_string(),
                message: log.message,
                fields: log.fields,
            }),
            _ => None,
        })
        .collect()
}

fn log_level_label(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Trace => "trace",
        LogLevel::Debug => "debug",
        LogLevel::Info => "info",
        LogLevel::Warn => "warn",
        LogLevel::Error => "error",
    }
}

fn flow_step_status_label(status: FlowStepStatus) -> &'static str {
    match status {
        FlowStepStatus::Pending => "pending",
        FlowStepStatus::Running => "running",
        FlowStepStatus::Succeeded => "succeeded",
        FlowStepStatus::Failed => "failed",
        FlowStepStatus::Skipped => "skipped",
        FlowStepStatus::Cancelled => "cancelled",
    }
}

fn next_step<'a>(step: &'a FlowStep, context: &Value) -> FlowResult<Option<&'a str>> {
    for branch in &step.next_when {
        if evaluate_condition(&branch.when, context)? {
            return Ok(Some(&branch.next));
        }
    }

    Ok(step.otherwise.as_deref().or(step.next.as_deref()))
}

fn resolve_action<'a>(
    actions: &'a [ActionDefinition],
    selector: &str,
) -> FlowResult<&'a ActionDefinition> {
    actions
        .iter()
        .find(|action| {
            action.entrypoint == selector
                || action.name.as_deref() == Some(selector)
                || action_key(action) == selector
        })
        .ok_or_else(|| FlowError::ActionNotFound {
            action: selector.to_string(),
        })
}

fn action_key(action: &ActionDefinition) -> String {
    format!("{}::{}", action.source.display(), action.entrypoint)
}

fn missing_step(flow: &FlowDefinition, step: &FlowStep, next: &str) -> FlowError {
    FlowError::InvalidStep {
        flow: flow.key.clone(),
        step: step.key.clone(),
        message: format!("references missing step '{next}'"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant, SystemTime};

    use ryvus_execution::{ExecutionResult, ExecutionTarget};
    use ryvus_protocol::{ActionKind, ApiAction, InvocationError, RuntimeKind};
    use serde_json::json;

    use crate::{model::ConditionalNext, InMemoryFlowStateStore};

    use super::*;

    #[tokio::test]
    async fn starts_async_and_records_successful_steps() {
        let (service, executor) = test_service(flow_spec(false));

        let start = service
            .start_flow("billing", json!({ "invoice": "inv_1" }))
            .expect("flow should start");

        assert_eq!(start.status, FlowExecutionStatus::Queued);
        let execution = wait_for_status(&service, &start.id, FlowExecutionStatus::Succeeded).await;
        assert_eq!(execution.steps.len(), 2);
        assert_eq!(execution.steps[0].key, "charge");
        assert_eq!(execution.steps[0].logs[0].level, "info");
        assert_eq!(execution.steps[0].logs[0].message, "charged invoice");
        assert_eq!(execution.steps[1].key, "receipt");
        assert_eq!(execution.output["receipt_sent"], true);

        let requests = executor.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].event["invoice"], "inv_1");
        assert_eq!(
            requests[0].context.metadata["flow"],
            json!({
                "flow_key": "billing",
                "execution_id": start.id,
                "step_key": "charge",
            })
        );
        assert_eq!(
            requests[0].context.metadata["params"],
            json!({ "invoice": "inv_1" })
        );
        assert_eq!(requests[0].context.metadata["config"], json!({}));
        assert_eq!(requests[1].event["status"], "paid");
    }

    #[tokio::test]
    async fn error_handler_can_mark_flow_failed_after_successful_handler_step() {
        let (service, executor) = test_service(flow_spec(true));

        let start = service
            .start_flow("billing", json!({ "invoice": "inv_1" }))
            .expect("flow should start");

        let execution = wait_for_status(&service, &start.id, FlowExecutionStatus::Failed).await;
        assert_eq!(execution.steps[0].status, FlowStepStatus::Failed);
        assert_eq!(execution.steps[1].key, "failure_handler");
        assert_eq!(execution.steps[1].status, FlowStepStatus::Succeeded);

        let requests = executor.requests();
        assert_eq!(requests[1].event["error"], "payment was declined");
        assert_eq!(requests[1].event["previous"]["status"], "declined");
        assert_eq!(
            requests[1].context.metadata["params"],
            json!({ "failed_status": "declined" })
        );
    }

    #[tokio::test]
    async fn retries_failed_step_in_same_run() {
        let executor = Arc::new(FlakyChargeExecutor::default());
        let service = FlowService::new(
            flow_spec(true),
            vec![
                api_action("charge"),
                api_action("decline_charge"),
                api_action("receipt"),
                api_action("failure_handler"),
            ],
            Arc::new(InMemoryFlowStateStore::default()),
            Arc::clone(&executor),
        )
        .expect("service should build");

        let start = service
            .start_flow("billing", json!({ "invoice": "inv_1" }))
            .expect("flow should start");
        wait_for_status(&service, &start.id, FlowExecutionStatus::Failed).await;

        let retried = service
            .retry_failed_step(&start.id, "charge")
            .expect("failed step should retry");

        assert_eq!(retried.status, FlowExecutionStatus::Succeeded);
        assert_eq!(
            retried
                .steps
                .iter()
                .filter(|step| step.key == "charge")
                .count(),
            2
        );
        assert_eq!(retried.output["receipt_sent"], true);
    }

    fn test_service(
        spec: FlowSpec,
    ) -> (
        FlowService<InMemoryFlowStateStore, RecordingFlowExecutor>,
        Arc<RecordingFlowExecutor>,
    ) {
        let executor = Arc::new(RecordingFlowExecutor::default());
        let service = FlowService::new(
            spec,
            vec![
                api_action("charge"),
                api_action("decline_charge"),
                api_action("receipt"),
                api_action("failure_handler"),
            ],
            Arc::new(InMemoryFlowStateStore::default()),
            Arc::clone(&executor),
        )
        .expect("service should build");

        (service, executor)
    }

    fn flow_spec(charge_fails: bool) -> FlowSpec {
        FlowSpec {
            flows: vec![FlowDefinition {
                key: "billing".to_string(),
                description: None,
                version: None,
                steps: vec![
                    FlowStep {
                        key: "charge".to_string(),
                        action: if charge_fails {
                            "decline_charge"
                        } else {
                            "charge"
                        }
                        .to_string(),
                        policy: ryvus_protocol::ActionExecutionPolicy::default(),
                        params: json!({ "invoice": "$.input.invoice" }),
                        config: json!({}),
                        next: None,
                        next_when: vec![ConditionalNext {
                            when: "$.output.status == \"paid\"".to_string(),
                            next: "receipt".to_string(),
                        }],
                        otherwise: None,
                        on_error: Some("failure_handler".to_string()),
                        end: None,
                    },
                    FlowStep {
                        key: "receipt".to_string(),
                        action: "receipt".to_string(),
                        policy: ryvus_protocol::ActionExecutionPolicy::default(),
                        params: json!({}),
                        config: json!({}),
                        next: None,
                        next_when: Vec::new(),
                        otherwise: None,
                        on_error: None,
                        end: None,
                    },
                    FlowStep {
                        key: "failure_handler".to_string(),
                        action: "failure_handler".to_string(),
                        policy: ryvus_protocol::ActionExecutionPolicy::default(),
                        params: json!({ "failed_status": "$.output.status" }),
                        config: json!({}),
                        next: None,
                        next_when: Vec::new(),
                        otherwise: None,
                        on_error: None,
                        end: Some(FlowEndStatus::Failed),
                    },
                ],
            }],
        }
    }

    async fn wait_for_status<E>(
        service: &FlowService<InMemoryFlowStateStore, E>,
        id: &str,
        status: FlowExecutionStatus,
    ) -> FlowExecution
    where
        E: FlowStepExecutor,
    {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let execution = service.get_run(id).expect("run should exist");
            if execution.status == status {
                return execution;
            }
            assert!(
                Instant::now() < deadline,
                "flow did not reach expected status"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn api_action(entrypoint: &str) -> ActionDefinition {
        ActionDefinition {
            runtime: RuntimeKind::Python,
            kind: ActionKind::Api(ApiAction {
                method: "POST".to_string(),
                path: format!("/{entrypoint}"),
                consumes: vec!["application/json".to_string()],
                produces: vec!["application/json".to_string()],
                request_schema: None,
                response_schema: None,
                query_params: Vec::new(),
                authorizer: None,
            }),
            source: format!("src/{entrypoint}.py").into(),
            entrypoint: entrypoint.to_string(),
            name: Some(entrypoint.to_string()),
            policy: ryvus_protocol::ActionExecutionPolicy::default(),
        }
    }

    #[derive(Default)]
    struct RecordingFlowExecutor {
        requests: Mutex<Vec<InvocationRequest>>,
    }

    impl FlowStepExecutor for RecordingFlowExecutor {
        fn execute_flow_step(
            &self,
            action: &ActionDefinition,
            request: &InvocationRequest,
            _policy: &ryvus_execution::ExecutionPolicy,
        ) -> FlowResult<ExecutionRecord> {
            self.requests
                .lock()
                .expect("requests should lock")
                .push(request.clone());

            if action.entrypoint == "charge" && request.event["invoice"] == "inv_1" {
                return Ok(execution_record_with_stdout(
                    request,
                    ryvus_protocol::InvocationResult {
                        protocol_version: PROTOCOL_VERSION.to_string(),
                        invocation_id: request.invocation_id.clone(),
                        status: InvocationStatus::Success,
                        output: Some(json!({ "status": "paid" })),
                        error: None,
                    },
                    &format!(
                        "{}\n",
                        json!({
                            "type": "event",
                            "event": {
                                "type": "log",
                                "invocation_id": request.invocation_id,
                                "level": "info",
                                "message": "charged invoice",
                                "fields": {}
                            }
                        })
                    ),
                ));
            }

            if action.entrypoint == "decline_charge" {
                return Ok(execution_record(
                    request,
                    ryvus_protocol::InvocationResult {
                        protocol_version: PROTOCOL_VERSION.to_string(),
                        invocation_id: request.invocation_id.clone(),
                        status: InvocationStatus::Failed,
                        output: Some(json!({ "status": "declined" })),
                        error: Some(InvocationError::new(
                            "payment_declined",
                            "payment was declined",
                            false,
                        )),
                    },
                ));
            }

            let output = match action.entrypoint.as_str() {
                "receipt" => json!({ "receipt_sent": true }),
                "failure_handler" => json!({ "handled": true }),
                _ => {
                    return Ok(execution_record(
                        request,
                        ryvus_protocol::InvocationResult::failed(
                            request.invocation_id.clone(),
                            InvocationError::new("failed", "step failed", false),
                        ),
                    ));
                }
            };

            Ok(execution_record(
                request,
                ryvus_protocol::InvocationResult {
                    protocol_version: PROTOCOL_VERSION.to_string(),
                    invocation_id: request.invocation_id.clone(),
                    status: InvocationStatus::Success,
                    output: Some(output),
                    error: None,
                },
            ))
        }
    }

    #[derive(Default)]
    struct FlakyChargeExecutor {
        charge_attempts: Mutex<u32>,
    }

    impl FlowStepExecutor for FlakyChargeExecutor {
        fn execute_flow_step(
            &self,
            action: &ActionDefinition,
            request: &InvocationRequest,
            _policy: &ryvus_execution::ExecutionPolicy,
        ) -> FlowResult<ExecutionRecord> {
            if action.entrypoint == "decline_charge" {
                let mut attempts = self.charge_attempts.lock().unwrap();
                *attempts += 1;

                if *attempts == 1 {
                    return Ok(execution_record(
                        request,
                        ryvus_protocol::InvocationResult {
                            protocol_version: PROTOCOL_VERSION.to_string(),
                            invocation_id: request.invocation_id.clone(),
                            status: InvocationStatus::Failed,
                            output: Some(json!({ "status": "declined" })),
                            error: Some(InvocationError {
                                code: "payment_declined".to_string(),
                                message: "payment was declined".to_string(),
                                details: Value::Null,
                                retryable: true,
                            }),
                        },
                    ));
                }

                return Ok(execution_record(
                    request,
                    ryvus_protocol::InvocationResult {
                        protocol_version: PROTOCOL_VERSION.to_string(),
                        invocation_id: request.invocation_id.clone(),
                        status: InvocationStatus::Success,
                        output: Some(json!({ "status": "paid" })),
                        error: None,
                    },
                ));
            }

            if action.entrypoint == "receipt" {
                return Ok(execution_record(
                    request,
                    ryvus_protocol::InvocationResult {
                        protocol_version: PROTOCOL_VERSION.to_string(),
                        invocation_id: request.invocation_id.clone(),
                        status: InvocationStatus::Success,
                        output: Some(json!({ "receipt_sent": true })),
                        error: None,
                    },
                ));
            }

            Ok(execution_record(
                request,
                ryvus_protocol::InvocationResult {
                    protocol_version: PROTOCOL_VERSION.to_string(),
                    invocation_id: request.invocation_id.clone(),
                    status: InvocationStatus::Success,
                    output: Some(json!({ "handled": true })),
                    error: None,
                },
            ))
        }
    }

    impl RecordingFlowExecutor {
        fn requests(&self) -> Vec<InvocationRequest> {
            self.requests.lock().expect("requests should lock").clone()
        }
    }

    fn execution_record(
        request: &InvocationRequest,
        invocation_result: ryvus_protocol::InvocationResult,
    ) -> ExecutionRecord {
        execution_record_with_stdout(request, invocation_result, "")
    }

    fn execution_record_with_stdout(
        request: &InvocationRequest,
        invocation_result: ryvus_protocol::InvocationResult,
        stdout: &str,
    ) -> ExecutionRecord {
        let now = SystemTime::now();
        ExecutionRecord::new(
            request.clone(),
            ExecutionTarget::Process {
                command: "test".to_string(),
                args: Vec::new(),
                working_dir: None,
                env: Default::default(),
            },
            ExecutionResult {
                invocation_result,
                stdout: stdout.to_string(),
                stderr: String::new(),
                duration: Duration::from_millis(1),
                exit_code: Some(0),
            },
            now,
            now,
        )
    }
}
