use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use crate::{
    model::{FlowExecution, FlowExecutionStatus, FlowStepExecution},
    FlowError, FlowResult,
};

pub trait FlowStateStore: Send + Sync + 'static {
    fn create(&self, execution: FlowExecution) -> FlowResult<()>;
    fn get(&self, id: &str) -> FlowResult<FlowExecution>;
    fn list(&self) -> FlowResult<Vec<FlowExecution>>;
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
                    invocation_id: Some("invocation_1".to_string()),
                    input: json!({ "invoice": "inv_1" }),
                    output: json!({ "received": true }),
                    error: None,
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
