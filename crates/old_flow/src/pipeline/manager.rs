use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

use crate::{context::FlowContext, error::FlowError, store::StateStore};
use ryvus_core::prelude::pipeline::{Pipeline, PipelineStep};

/// -----------------------------
/// Step Definition
/// -----------------------------
#[derive(Clone, Debug, Deserialize, Serialize, Default)]
pub struct StepDefinition {
    pub key: String,
    pub action: String,

    #[serde(default)]
    pub params: serde_json::Value,

    #[serde(default = "empty_json_object")]
    pub config: Value,

    #[serde(default)]
    pub retry: Option<RetryConfig>,

    #[serde(default)]
    pub hooks: Vec<HookDefinition>,

    /// Linear next step
    #[serde(default)]
    pub next: Option<String>,

    /// Conditional branches
    #[serde(default)]
    pub next_when: Vec<ConditionalNextDef>,

    /// Fallback if no condition matches
    #[serde(default)]
    pub otherwise: Option<String>,

    /// Fallback if this step fails
    #[serde(default)]
    pub on_error: Option<String>,
}

fn empty_json_object() -> Value {
    serde_json::json!({})
}

/// Conditional branch definition
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ConditionalNextDef {
    pub when: String,
    pub next: String,
}

/// Retry configuration (future feature)
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RetryConfig {
    pub max_attempts: u32,
    pub delay: u64, // ms
}

/// Hook definition
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HookDefinition {
    #[serde(rename = "type")]
    pub hook_type: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

/// -----------------------------
/// Pipeline Definition
/// -----------------------------
#[derive(Clone, Debug, Deserialize, Serialize, Default)]
pub struct PipelineDefinition {
    pub key: String,
    pub description: Option<String>,
    pub version: Option<String>,
    pub steps: Vec<StepDefinition>,

    #[serde(default)]
    pub pipeline_hooks: Vec<HookDefinition>,
}

/// -----------------------------
/// Flow Pipeline Manager
/// -----------------------------
pub struct FlowPipelineManager<S: StateStore> {
    store: Arc<S>,
    pipelines: Vec<PipelineDefinition>,
}

impl<S: StateStore> FlowPipelineManager<S> {
    pub fn new(store: S) -> Self {
        Self {
            store: Arc::new(store),
            pipelines: Vec::new(),
        }
    }

    pub fn register(&mut self, pipeline: PipelineDefinition) {
        self.pipelines.push(pipeline);
    }

    pub fn get(&self, pipeline_key: &str) -> Option<&PipelineDefinition> {
        self.pipelines.iter().find(|p| p.key == pipeline_key)
    }

    pub async fn start(&self, pipeline_key: &str) -> Result<(), FlowError> {
        let Some(def) = self.get(pipeline_key) else {
            return Err(FlowError::PipelineNotFound(pipeline_key.to_string()));
        };

        let ctx = FlowContext::new(def.clone(), self.store.clone());
        ctx.run().await
    }
}

/// -----------------------------
/// Conversion to Ryvus Pipeline
/// -----------------------------
impl TryFrom<PipelineDefinition> for Pipeline {
    type Error = String;

    fn try_from(def: PipelineDefinition) -> Result<Self, Self::Error> {
        if def.steps.is_empty() {
            return Err(format!("Pipeline '{}' has no steps", def.key));
        }

        let all_keys: Vec<String> = def.steps.iter().map(|s| s.key.clone()).collect();

        let steps = def
            .steps
            .into_iter()
            .map(|s| {
                if s.action.trim().is_empty() {
                    return Err(format!("Step '{}' is missing an action", s.key));
                }

                // Create builder
                let mut step_builder = PipelineStep::builder(&s.key, &s.action)
                    .params(s.params.clone())
                    .config(s.config.clone());

                // Apply conditional branches
                for cond in &s.next_when {
                    step_builder = step_builder.when(cond.when.clone(), cond.next.clone());
                }

                // Apply otherwise
                if let Some(otherwise) = &s.otherwise {
                    step_builder = step_builder.otherwise(otherwise.clone());
                }

                // Apply error fallback
                if let Some(on_error) = &s.on_error {
                    step_builder = step_builder.on_error(on_error.clone());
                }

                // Apply linear next
                if let Some(next) = &s.next {
                    step_builder = step_builder.next(next.clone());
                }

                Ok(step_builder.build())
            })
            .collect::<Result<Vec<_>, _>>()?;

        // Basic validation: ensure referenced steps exist
        for step in &steps {
            for cond in &step.next_when {
                if !all_keys.contains(&cond.next) {
                    return Err(format!(
                        "Step '{}' references undefined step '{}' in 'when' condition",
                        step.key, cond.next
                    ));
                }
            }
            if let Some(ref next) = step.next {
                if !next.is_empty() && !all_keys.contains(next) {
                    return Err(format!(
                        "Step '{}' references undefined 'next' step '{}'",
                        step.key, next
                    ));
                }
            }
            if let Some(ref otherwise) = step.otherwise {
                if !all_keys.contains(otherwise) {
                    return Err(format!(
                        "Step '{}' references undefined 'otherwise' step '{}'",
                        step.key, otherwise
                    ));
                }
            }
            if let Some(ref on_error) = step.on_error {
                if !all_keys.contains(on_error) {
                    return Err(format!(
                        "Step '{}' references undefined 'on_error' step '{}'",
                        step.key, on_error
                    ));
                }
            }
        }

        Ok(Pipeline::builder(def.key).steps(steps).build())
    }
}
