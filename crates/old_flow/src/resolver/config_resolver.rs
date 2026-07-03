use crate::{prelude::PipelineDefinition, resolver::variable::VariableResolver};
use serde_json::Value;

/// Resolves `$VAR` and `secret:$VAR` placeholders in-place,
/// and returns all resolved *secret values* for later masking.
pub fn resolve_config(
    pipeline: &mut PipelineDefinition,
    resolver: &dyn VariableResolver,
) -> Vec<String> {
    let mut secrets = Vec::new();

    for step in &mut pipeline.steps {
        resolve_value(&mut step.config, resolver, &mut secrets);
        resolve_value(&mut step.params, resolver, &mut secrets);
    }

    secrets
}

fn resolve_value(value: &mut Value, resolver: &dyn VariableResolver, secrets: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for v in map.values_mut() {
                resolve_value(v, resolver, secrets);
            }
        }
        Value::Array(arr) => {
            for v in arr.iter_mut() {
                resolve_value(v, resolver, secrets);
            }
        }
        Value::String(s) => {
            if let Some(name) = s.strip_prefix("secret:$") {
                if let Some(val) = resolver.resolve(name.trim()) {
                    secrets.push(val.clone());
                    *value = Value::String(val);
                }
            } else if let Some(name) = s.strip_prefix('$') {
                if let Some(val) = resolver.resolve(name.trim()) {
                    *value = Value::String(val);
                }
            }
        }
        _ => {}
    }
}
