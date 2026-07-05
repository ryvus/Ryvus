use std::collections::{HashMap, HashSet};

use ryvus_protocol::ActionDefinition;

use crate::{
    error::{FlowError, FlowResult},
    model::{FlowDefinition, FlowSpec, FlowStep},
};

pub fn validate_flow_spec(spec: &FlowSpec) -> FlowResult<()> {
    let mut flow_keys = HashSet::new();

    for flow in &spec.flows {
        if flow.key.trim().is_empty() {
            return Err(FlowError::InvalidFlow {
                flow: "<empty>".to_string(),
                message: "flow key is required".to_string(),
            });
        }

        if !flow_keys.insert(flow.key.as_str()) {
            return Err(FlowError::InvalidFlow {
                flow: flow.key.clone(),
                message: "duplicate flow key".to_string(),
            });
        }

        validate_flow(flow)?;
    }

    Ok(())
}

pub fn validate_flow_actions<'a>(
    spec: &FlowSpec,
    actions: impl IntoIterator<Item = &'a ActionDefinition>,
) -> FlowResult<()> {
    let action_keys = actions
        .into_iter()
        .flat_map(|action| {
            let source = action.source.display().to_string().replace('\\', "/");
            [
                action.entrypoint.clone(),
                action.name.clone().unwrap_or_default(),
                format!("{source}::{}", action.entrypoint),
            ]
        })
        .filter(|key| !key.is_empty())
        .collect::<HashSet<_>>();

    for flow in &spec.flows {
        for step in &flow.steps {
            if !action_keys.contains(&step.action) {
                return Err(FlowError::ActionNotFound {
                    action: step.action.clone(),
                });
            }
        }
    }

    Ok(())
}

fn validate_flow(flow: &FlowDefinition) -> FlowResult<()> {
    if flow.steps.is_empty() {
        return Err(FlowError::InvalidFlow {
            flow: flow.key.clone(),
            message: "at least one step is required".to_string(),
        });
    }

    let mut step_keys = HashSet::new();

    for step in &flow.steps {
        if step.key.trim().is_empty() {
            return Err(step_error(flow, step, "step key is required"));
        }

        if step.action.trim().is_empty() {
            return Err(step_error(flow, step, "step action is required"));
        }

        if !step_keys.insert(step.key.as_str()) {
            return Err(step_error(flow, step, "duplicate step key"));
        }

        ryvus_execution::ExecutionPolicy::from_action_policy(&step.policy)
            .map_err(|error| step_error(flow, step, &error.to_string()))?;

        for branch in &step.next_when {
            validate_condition(&flow.key, step, &branch.when)?;
        }
    }

    let steps = flow
        .steps
        .iter()
        .map(|step| (step.key.as_str(), step))
        .collect::<HashMap<_, _>>();

    for step in &flow.steps {
        validate_reference(flow, step, "next", step.next.as_deref(), &steps)?;
        validate_reference(flow, step, "otherwise", step.otherwise.as_deref(), &steps)?;
        validate_reference(flow, step, "on_error", step.on_error.as_deref(), &steps)?;

        for branch in &step.next_when {
            validate_reference(flow, step, "next_when.next", Some(&branch.next), &steps)?;
        }
    }

    Ok(())
}

fn validate_reference(
    flow: &FlowDefinition,
    step: &FlowStep,
    field: &str,
    reference: Option<&str>,
    steps: &HashMap<&str, &FlowStep>,
) -> FlowResult<()> {
    let Some(reference) = reference else {
        return Ok(());
    };

    if steps.contains_key(reference) {
        return Ok(());
    }

    Err(step_error(
        flow,
        step,
        &format!("{field} references missing step '{reference}'"),
    ))
}

fn validate_condition(flow_key: &str, step: &FlowStep, condition: &str) -> FlowResult<()> {
    let condition = condition.trim();
    let operators = ["==", "!=", ">=", "<=", ">", "<"];

    if !condition.is_empty() {
        for operator in operators {
            if let Some((left, _right)) = condition.split_once(operator) {
                if left.trim().starts_with("$.") {
                    return Ok(());
                }
                break;
            }
        }
    }

    Err(FlowError::InvalidStep {
        flow: flow_key.to_string(),
        step: step.key.clone(),
        message: format!("invalid condition syntax '{condition}'"),
    })
}

fn step_error(flow: &FlowDefinition, step: &FlowStep, message: &str) -> FlowError {
    FlowError::InvalidStep {
        flow: flow.key.clone(),
        step: step.key.clone(),
        message: message.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use ryvus_protocol::{ActionDefinition, ActionKind, ApiAction, RuntimeKind};
    use serde_json::json;

    use super::*;

    #[test]
    fn validates_current_example_flow_files() {
        let restock: FlowDefinition = serde_json::from_str(include_str!(
            "../../../../my-project/src/modules/petstore/flows/restock/restock.flows.json"
        ))
        .expect("restock flow should parse");
        let billing: FlowDefinition = serde_json::from_str(include_str!(
            "../../../../my-project/src/modules/billing/flows/billing_workflow/billing_workflow.flows.json"
        ))
        .expect("billing flow should parse");
        let spec = FlowSpec {
            flows: vec![restock, billing],
        };

        validate_flow_spec(&spec).expect("example flows should validate");
    }

    #[test]
    fn rejects_duplicate_step_keys() {
        let spec: FlowSpec = serde_json::from_value(json!({
            "flows": [
                {
                    "key": "duplicate",
                    "steps": [
                        { "key": "same", "action": "first" },
                        { "key": "same", "action": "second" }
                    ]
                }
            ]
        }))
        .unwrap();

        assert!(matches!(
            validate_flow_spec(&spec),
            Err(FlowError::InvalidStep { .. })
        ));
    }

    #[test]
    fn rejects_missing_step_reference() {
        let spec: FlowSpec = serde_json::from_value(json!({
            "flows": [
                {
                    "key": "missing_reference",
                    "steps": [
                        { "key": "start", "action": "start", "next": "missing" }
                    ]
                }
            ]
        }))
        .unwrap();

        assert!(matches!(
            validate_flow_spec(&spec),
            Err(FlowError::InvalidStep { .. })
        ));
    }

    #[test]
    fn rejects_invalid_condition_syntax() {
        let spec: FlowSpec = serde_json::from_value(json!({
            "flows": [
                {
                    "key": "invalid_condition",
                    "steps": [
                        {
                            "key": "start",
                            "action": "start",
                            "next_when": [
                                { "when": "status", "next": "end" }
                            ]
                        },
                        { "key": "end", "action": "finish" }
                    ]
                }
            ]
        }))
        .unwrap();

        assert!(matches!(
            validate_flow_spec(&spec),
            Err(FlowError::InvalidStep { message, .. }) if message.contains("invalid condition syntax")
        ));
    }

    #[test]
    fn rejects_condition_with_non_jsonpath_left_operand() {
        let spec: FlowSpec = serde_json::from_value(json!({
            "flows": [
                {
                    "key": "invalid_condition",
                    "steps": [
                        {
                            "key": "start",
                            "action": "start",
                            "next_when": [
                                { "when": "status == \"paid\"", "next": "end" }
                            ]
                        },
                        { "key": "end", "action": "finish" }
                    ]
                }
            ]
        }))
        .unwrap();

        assert!(matches!(
            validate_flow_spec(&spec),
            Err(FlowError::InvalidStep { message, .. }) if message.contains("invalid condition syntax")
        ));
    }

    #[test]
    fn validates_step_actions_against_catalog_names_entrypoints_and_keys() {
        let spec: FlowSpec = serde_json::from_value(json!({
            "flows": [
                {
                    "key": "actions",
                    "steps": [
                        { "key": "by_name", "action": "named_action", "next": "by_entrypoint" },
                        { "key": "by_entrypoint", "action": "handler", "next": "by_key" },
                        { "key": "by_key", "action": "src/action.py::handler" }
                    ]
                }
            ]
        }))
        .unwrap();

        let action = ActionDefinition {
            runtime: RuntimeKind::Python,
            kind: ActionKind::Api(ApiAction {
                method: "POST".to_string(),
                path: "/action".to_string(),
                request_schema: None,
                response_schema: None,
                query_params: Vec::new(),
            }),
            source: "src/action.py".into(),
            entrypoint: "handler".to_string(),
            name: Some("named_action".to_string()),
            policy: ryvus_protocol::ActionExecutionPolicy::default(),
        };

        validate_flow_actions(&spec, [&action]).expect("actions should resolve");
    }

    #[test]
    fn rejects_unknown_action_reference() {
        let spec: FlowSpec = serde_json::from_value(json!({
            "flows": [
                {
                    "key": "missing_action",
                    "steps": [
                        { "key": "start", "action": "does_not_exist" }
                    ]
                }
            ]
        }))
        .unwrap();

        let action = ActionDefinition {
            runtime: RuntimeKind::Python,
            kind: ActionKind::Api(ApiAction {
                method: "POST".to_string(),
                path: "/action".to_string(),
                request_schema: None,
                response_schema: None,
                query_params: Vec::new(),
            }),
            source: "src/action.py".into(),
            entrypoint: "handler".to_string(),
            name: Some("named_action".to_string()),
            policy: ryvus_protocol::ActionExecutionPolicy::default(),
        };

        assert!(matches!(
            validate_flow_actions(&spec, [&action]),
            Err(FlowError::ActionNotFound { action }) if action == "does_not_exist"
        ));
    }
}
