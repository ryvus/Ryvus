use jsonpath_rust::JsonPath;
use serde_json::{json, Value};

use crate::{FlowError, FlowResult};

#[derive(Debug, Clone)]
pub struct FlowContext {
    pub input: Value,
    pub latest_output: Value,
    pub steps: serde_json::Map<String, Value>,
}

impl FlowContext {
    pub fn new(input: Value) -> Self {
        Self {
            input,
            latest_output: Value::Null,
            steps: serde_json::Map::new(),
        }
    }

    pub fn record_step(&mut self, key: &str, status: &str, output: Value, error: Option<String>) {
        self.latest_output = output.clone();
        self.steps.insert(
            key.to_string(),
            json!({
                "status": status,
                "output": output,
                "error": error,
            }),
        );
    }

    pub fn as_json(&self) -> Value {
        json!({
            "input": self.input,
            "output": self.latest_output,
            "steps": self.steps,
        })
    }
}

pub fn resolve_jsonpaths(value: &mut Value, context: &Value) -> FlowResult<()> {
    match value {
        Value::Object(map) => {
            for value in map.values_mut() {
                resolve_jsonpaths(value, context)?;
            }
        }
        Value::Array(items) => {
            for value in items {
                resolve_jsonpaths(value, context)?;
            }
        }
        Value::String(raw) => {
            if let Some(escaped) = raw.strip_prefix("$$.") {
                *value = Value::String(format!("$.{escaped}"));
                return Ok(());
            }
            if !raw.starts_with("$.") {
                return Ok(());
            }

            *value = query_first(raw, context)?.unwrap_or_else(|| Value::String(raw.clone()));
        }
        _ => {}
    }

    Ok(())
}

pub fn evaluate_condition(condition: &str, context: &Value) -> FlowResult<bool> {
    let operators = ["==", "!=", ">=", "<=", ">", "<"];
    let Some((operator, (left, right))) = operators.iter().find_map(|operator| {
        condition
            .split_once(operator)
            .map(|parts| (*operator, parts))
    }) else {
        return Err(FlowError::JsonPath {
            expression: condition.to_string(),
            message: "condition must contain one of ==, !=, >=, <=, >, <".to_string(),
        });
    };

    let left = query_operand(left.trim(), context, true)?.unwrap_or(Value::Null);
    let right = query_operand(right.trim(), context, false)?.unwrap_or(Value::Null);

    Ok(match operator {
        "==" => left == right,
        "!=" => left != right,
        ">" => compare_numbers(&left, &right, |left, right| left > right),
        ">=" => compare_numbers(&left, &right, |left, right| left >= right),
        "<" => compare_numbers(&left, &right, |left, right| left < right),
        "<=" => compare_numbers(&left, &right, |left, right| left <= right),
        _ => false,
    })
}

fn query_first(expression: &str, context: &Value) -> FlowResult<Option<Value>> {
    if !expression.starts_with("$.") {
        return Ok(Some(Value::String(expression.to_string())));
    }

    context
        .query_with_path(expression)
        .map(|matches| matches.first().cloned().map(|value| value.val().clone()))
        .map_err(|error| FlowError::JsonPath {
            expression: expression.to_string(),
            message: error.to_string(),
        })
}

fn query_operand(
    expression: &str,
    context: &Value,
    require_jsonpath: bool,
) -> FlowResult<Option<Value>> {
    if expression.starts_with("$.") {
        return context
            .query_with_path(expression)
            .map(|matches| matches.first().cloned().map(|value| value.val().clone()))
            .map_err(|error| FlowError::JsonPath {
                expression: expression.to_string(),
                message: error.to_string(),
            });
    }

    if require_jsonpath {
        return Err(FlowError::JsonPath {
            expression: expression.to_string(),
            message: "left side of condition must be a JSONPath expression".to_string(),
        });
    }

    Ok(Some(
        serde_json::from_str::<Value>(expression).unwrap_or_else(|_| {
            Value::String(expression.trim_matches('"').trim_matches('\'').to_string())
        }),
    ))
}

fn compare_numbers(left: &Value, right: &Value, compare: impl Fn(f64, f64) -> bool) -> bool {
    let Some(left) = left.as_f64() else {
        return false;
    };
    let Some(right) = right.as_f64() else {
        return false;
    };

    compare(left, right)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn resolves_jsonpaths_in_values() {
        let mut context = FlowContext::new(json!({ "order_id": "ord_123" }));
        context.record_step("charge", "succeeded", json!({ "amount": 42 }), None);
        let mut value = json!({
            "order": "$.input.order_id",
            "amount": "$.steps.charge.output.amount",
            "literal": "$$.input.order_id"
        });

        resolve_jsonpaths(&mut value, &context.as_json()).expect("jsonpaths should resolve");

        assert_eq!(value["order"], "ord_123");
        assert_eq!(value["amount"], 42);
        assert_eq!(value["literal"], "$.input.order_id");
    }

    #[test]
    fn evaluates_supported_conditions() {
        let context = json!({
            "output": { "status": "paid", "amount": 42 },
            "steps": {
                "charge": {
                    "output": { "status": "paid", "amount": 42 }
                }
            }
        });

        assert!(evaluate_condition("$.output.status == \"paid\"", &context).unwrap());
        assert!(evaluate_condition("$.output.status != \"failed\"", &context).unwrap());
        assert!(evaluate_condition("$.output.amount > 40", &context).unwrap());
        assert!(evaluate_condition("$.output.amount >= 42", &context).unwrap());
        assert!(evaluate_condition("$.output.amount < 50", &context).unwrap());
        assert!(evaluate_condition("$.output.amount <= 42", &context).unwrap());
        assert!(
            evaluate_condition("$.output.status == $.steps.charge.output.status", &context)
                .unwrap()
        );
    }

    #[test]
    fn failed_step_output_becomes_latest_output() {
        let mut context = FlowContext::new(json!({ "invoice": "inv_1" }));
        context.record_step(
            "charge",
            "failed",
            json!({ "status": "declined" }),
            Some("payment was declined".to_string()),
        );
        let mut value = json!({
            "latest": "$.output.status",
            "step": "$.steps.charge.output.status",
        });

        resolve_jsonpaths(&mut value, &context.as_json()).expect("jsonpaths should resolve");

        assert_eq!(value["latest"], "declined");
        assert_eq!(value["step"], "declined");
    }

    #[test]
    fn rejects_non_jsonpath_left_operand() {
        let context = json!({ "output": { "status": "paid" } });

        let error = evaluate_condition("status == \"paid\"", &context).unwrap_err();

        assert!(matches!(error, FlowError::JsonPath { expression, .. } if expression == "status"));
    }
}
