use ryvus_protocol::{AttemptId, ExecutionId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq)]
pub struct FlowSpec {
    #[serde(default)]
    pub flows: Vec<FlowDefinition>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct FlowDefinition {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub steps: Vec<FlowStep>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct FlowStep {
    pub key: String,
    pub action: String,
    #[serde(default, flatten)]
    pub policy: ryvus_protocol::ActionExecutionPolicy,
    #[serde(default)]
    pub params: Value,
    #[serde(default)]
    pub config: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    #[serde(default)]
    pub next_when: Vec<ConditionalNext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otherwise: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<FlowEndStatus>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ConditionalNext {
    pub when: String,
    pub next: String,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FlowEndStatus {
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FlowExecutionStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FlowStepStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Skipped,
    Cancelled,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct FlowStepLog {
    pub level: String,
    pub message: String,
    pub fields: Value,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct FlowStepExecution {
    pub key: String,
    pub action: String,
    pub status: FlowStepStatus,
    #[serde(default = "default_attempts")]
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<ExecutionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<AttemptId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_number: Option<u32>,
    pub input: Value,
    pub output: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub logs: Vec<FlowStepLog>,
}

fn default_attempts() -> u32 {
    1
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct FlowExecution {
    pub id: String,
    pub flow_key: String,
    pub status: FlowExecutionStatus,
    pub input: Value,
    pub output: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub steps: Vec<FlowStepExecution>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct StartFlowResponse {
    pub id: String,
    pub flow_key: String,
    pub status: FlowExecutionStatus,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn parses_flow_spec_wrapper() {
        let spec: FlowSpec = serde_json::from_value(json!({
            "flows": [
                {
                    "key": "billing_workflow",
                    "description": "Billing flow",
                    "version": "0.1.0",
                    "steps": [
                        {
                            "key": "receive_invoice",
                            "action": "billing/receive_invoice",
                            "next": "billing_failure_handler"
                        },
                        {
                            "key": "billing_failure_handler",
                            "action": "billing/failure_handler",
                            "end": "failed"
                        }
                    ]
                }
            ]
        }))
        .expect("flow spec should parse");

        assert_eq!(spec.flows[0].key, "billing_workflow");
        assert_eq!(spec.flows[0].steps[1].end, Some(FlowEndStatus::Failed));
    }

    #[test]
    fn parses_single_current_example_flow_shape() {
        let flow: FlowDefinition = serde_json::from_str(include_str!(
            "../../../../my-project/src/modules/petstore/flows/restock/restock.flows.json"
        ))
        .expect("current example flow should parse");

        assert_eq!(flow.key, "restock_flow");
        assert_eq!(flow.steps[0].key, "restock");
    }

    #[test]
    fn flow_step_defaults_policy_and_attempts() {
        let flow: FlowDefinition = serde_json::from_value(json!({
            "key": "billing",
            "steps": [{ "key": "charge", "action": "charge" }]
        }))
        .expect("flow should parse");

        assert_eq!(flow.steps[0].policy.timeout, "3s");
        assert_eq!(flow.steps[0].policy.retry.max_attempts, 1);

        let step: FlowStepExecution = serde_json::from_value(json!({
            "key": "charge",
            "action": "charge",
            "status": "succeeded",
            "input": {},
            "output": {}
        }))
        .expect("step execution should parse");

        assert_eq!(step.attempts, 1);
    }
}
