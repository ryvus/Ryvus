use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use crate::{
    model::{FlowExecution, FlowExecutionStatus, FlowStepExecution},
    FlowError, FlowResult,
};
use ryvus_protocol::ExecutionId;

pub trait FlowStateStore: Send + Sync + 'static {
    fn create(&self, execution: FlowExecution) -> FlowResult<()>;
    fn get(&self, id: &str) -> FlowResult<FlowExecution>;
    fn list(&self) -> FlowResult<Vec<FlowExecution>>;
    fn cancel(&self, id: &str) -> FlowResult<FlowExecution>;
    fn is_cancelled(&self, id: &str) -> FlowResult<bool>;
    fn set_active_execution(&self, id: &str, execution_id: Option<ExecutionId>) -> FlowResult<()>;
    fn active_execution(&self, id: &str) -> FlowResult<Option<ExecutionId>>;
    fn update_status(
        &self,
        id: &str,
        status: FlowExecutionStatus,
        output: serde_json::Value,
        error: Option<String>,
    ) -> FlowResult<()>;
    fn push_step(&self, id: &str, step: FlowStepExecution) -> FlowResult<()>;
}

#[derive(Debug, Clone, Default)]
pub struct InMemoryFlowStateStore {
    executions: Arc<Mutex<HashMap<String, FlowExecution>>>,
    active_executions: Arc<Mutex<HashMap<String, ExecutionId>>>,
}

impl FlowStateStore for InMemoryFlowStateStore {
    fn create(&self, execution: FlowExecution) -> FlowResult<()> {
        self.executions
            .lock()
            .expect("flow executions should lock")
            .insert(execution.id.clone(), execution);
        Ok(())
    }

    fn get(&self, id: &str) -> FlowResult<FlowExecution> {
        self.executions
            .lock()
            .expect("flow executions should lock")
            .get(id)
            .cloned()
            .ok_or_else(|| FlowError::RunNotFound { id: id.to_string() })
    }

    fn list(&self) -> FlowResult<Vec<FlowExecution>> {
        let mut executions = self
            .executions
            .lock()
            .expect("flow executions should lock")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        executions.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(executions)
    }

    fn cancel(&self, id: &str) -> FlowResult<FlowExecution> {
        let mut executions = self.executions.lock().expect("flow executions should lock");
        let execution = executions
            .get_mut(id)
            .ok_or_else(|| FlowError::RunNotFound { id: id.to_string() })?;

        if matches!(
            execution.status,
            FlowExecutionStatus::Succeeded
                | FlowExecutionStatus::Failed
                | FlowExecutionStatus::Cancelled
        ) {
            return Ok(execution.clone());
        }

        execution.status = FlowExecutionStatus::Cancelled;
        execution.error = None;
        Ok(execution.clone())
    }

    fn is_cancelled(&self, id: &str) -> FlowResult<bool> {
        Ok(self.get(id)?.status == FlowExecutionStatus::Cancelled)
    }

    fn set_active_execution(&self, id: &str, execution_id: Option<ExecutionId>) -> FlowResult<()> {
        if !self
            .executions
            .lock()
            .expect("flow executions should lock")
            .contains_key(id)
        {
            return Err(FlowError::RunNotFound { id: id.to_string() });
        }

        let mut active = self
            .active_executions
            .lock()
            .expect("active executions should lock");

        if let Some(execution_id) = execution_id {
            active.insert(id.to_string(), execution_id);
        } else {
            active.remove(id);
        }

        Ok(())
    }

    fn active_execution(&self, id: &str) -> FlowResult<Option<ExecutionId>> {
        if !self
            .executions
            .lock()
            .expect("flow executions should lock")
            .contains_key(id)
        {
            return Err(FlowError::RunNotFound { id: id.to_string() });
        }

        Ok(self
            .active_executions
            .lock()
            .expect("active executions should lock")
            .get(id)
            .cloned())
    }

    fn update_status(
        &self,
        id: &str,
        status: FlowExecutionStatus,
        output: serde_json::Value,
        error: Option<String>,
    ) -> FlowResult<()> {
        let mut executions = self.executions.lock().expect("flow executions should lock");
        let execution = executions
            .get_mut(id)
            .ok_or_else(|| FlowError::RunNotFound { id: id.to_string() })?;
        execution.status = status;
        execution.output = output;
        execution.error = error;
        Ok(())
    }

    fn push_step(&self, id: &str, step: FlowStepExecution) -> FlowResult<()> {
        let mut executions = self.executions.lock().expect("flow executions should lock");
        let execution = executions
            .get_mut(id)
            .ok_or_else(|| FlowError::RunNotFound { id: id.to_string() })?;
        execution.steps.push(step);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::model::{FlowStepExecution, FlowStepStatus};

    use super::*;

    #[test]
    fn stores_and_updates_flow_execution() {
        let store = InMemoryFlowStateStore::default();
        store
            .create(FlowExecution {
                id: "flowrun_1".to_string(),
                flow_key: "billing".to_string(),
                status: FlowExecutionStatus::Queued,
                input: json!({ "invoice": "inv_1" }),
                output: json!(null),
                error: None,
                steps: Vec::new(),
            })
            .expect("execution should be stored");

        store
            .push_step(
                "flowrun_1",
                FlowStepExecution {
                    key: "receive_invoice".to_string(),
                    action: "billing/receive_invoice".to_string(),
                    status: FlowStepStatus::Succeeded,
                    attempts: 1,
                    execution_id: Some(ExecutionId::from("execution_1")),
                    attempt_id: Some(ryvus_protocol::AttemptId::from("attempt_1")),
                    attempt_number: Some(1),
                    input: json!({ "invoice": "inv_1" }),
                    output: json!({ "received": true }),
                    error: None,
                    logs: Vec::new(),
                },
            )
            .expect("step should be stored");
        store
            .update_status(
                "flowrun_1",
                FlowExecutionStatus::Succeeded,
                json!({ "received": true }),
                None,
            )
            .expect("status should update");

        let execution = store.get("flowrun_1").expect("execution should load");
        assert_eq!(execution.status, FlowExecutionStatus::Succeeded);
        assert_eq!(execution.steps[0].key, "receive_invoice");
    }

    #[test]
    fn lists_executions_in_id_order() {
        let store = InMemoryFlowStateStore::default();
        store
            .create(FlowExecution {
                id: "flowrun_b".to_string(),
                flow_key: "billing".to_string(),
                status: FlowExecutionStatus::Queued,
                input: json!(null),
                output: json!(null),
                error: None,
                steps: Vec::new(),
            })
            .expect("execution should be stored");
        store
            .create(FlowExecution {
                id: "flowrun_a".to_string(),
                flow_key: "billing".to_string(),
                status: FlowExecutionStatus::Queued,
                input: json!(null),
                output: json!(null),
                error: None,
                steps: Vec::new(),
            })
            .expect("execution should be stored");

        let executions = store.list().expect("executions should list");
        assert_eq!(executions[0].id, "flowrun_a");
        assert_eq!(executions[1].id, "flowrun_b");
    }
}
