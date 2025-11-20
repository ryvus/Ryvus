use crate::{
    action_resolver::ActionResolver,
    config_resolver::{ConfigResolver, JsonPathConfigResolver},
    error::{EngineError, Result},
    hook_resolver::ActionHookResolver,
    mapper::mapper::Mapper,
    pipeline::action_executor::ActionExecutor,
    utils::{
        json::deep_merge,
        jsonpath_resolver::{build_jsonpath_context, resolve_jsonpaths},
    },
};

use ryvus_core::{
    environment::Environment,
    pipeline::hook::ActionHook,
    prelude::{
        pipeline::Pipeline, ActionResult, ExecutionContext, ExecutionStatus, PipelineHook,
        PipelineStep,
    },
};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::debug;

/// Executes a Pipeline of Actions with flow control.
/// Supports next_when, else, on_error, and cancel handling.
pub struct PipelineExecutor<'a, M: Mapper, HR: ActionHookResolver, AR: ActionResolver> {
    pub pipeline: Pipeline,
    pub mapper: Arc<M>,
    pub global_action_hooks: Vec<Arc<dyn ActionHook>>,
    pub global_pipeline_hooks: Vec<Arc<dyn PipelineHook>>,
    pub hook_resolver: &'a HR,
    pub action_resolver: &'a AR,
    pub cancel_token: CancellationToken,
}

impl<'a, M, HR, AR> PipelineExecutor<'a, M, HR, AR>
where
    M: Mapper + Send + Sync + 'static,
    HR: ActionHookResolver + Send + Sync + 'static,
    AR: ActionResolver + Send + Sync + 'static,
{
    pub fn new(
        pipeline: Pipeline,
        mapper: Arc<M>,
        global_action_hooks: Vec<Arc<dyn ActionHook>>,
        global_pipeline_hooks: Vec<Arc<dyn PipelineHook>>,
        hook_resolver: &'a HR,
        action_resolver: &'a AR,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            pipeline,
            mapper,
            global_action_hooks,
            global_pipeline_hooks,
            hook_resolver,
            action_resolver,
            cancel_token,
        }
    }

    /// Executes the pipeline based on dynamic routing.
    pub async fn execute(&self, input: Value) -> Result<ExecutionContext> {
        let _ = tracing_subscriber::fmt::try_init();

        debug!("Executing...");
        let mut exec_ctx = ExecutionContext::new(
            &self.pipeline.key,
            Environment::new("local", ryvus_core::environment::EnvironmentKind::Local),
        );
        exec_ctx.data.insert("payload".to_string(), input);

        debug!("Triggering start hooks");
        for hook in &self.global_pipeline_hooks {
            hook.start(&mut exec_ctx).await;
        }

        // Start at the first step in the pipeline
        let mut current_key = self
            .pipeline
            .steps
            .first()
            .ok_or_else(|| EngineError::Other("Pipeline has no steps".into()))?
            .key
            .clone();
        debug!("Current step key: {}", current_key);

        loop {
            // Check cancel token
            debug!("Check for cancellation:  {}", current_key);

            if self.cancel_token.is_cancelled() {
                debug!("Cancaling:  {}", current_key);

                for hook in &self.global_pipeline_hooks {
                    hook.canceled(&mut exec_ctx).await;
                }
                return Err(EngineError::Canceled);
            }

            // Find current step
            let step = self
                .pipeline
                .steps
                .iter()
                .find(|s| s.key == current_key)
                .ok_or_else(|| EngineError::Other(format!("Step '{}' not found", current_key)))?;

            debug!("Resolved step:  {}", step.key);

            // Execute current step
            let result = self.execute_action_step(step, &mut exec_ctx).await;
            debug!("Step: {} result {:?}", step.key, result);

            match result {
                Ok(action_result) => {
                    if action_result.status == ExecutionStatus::Failed {
                        exec_ctx.error = action_result
                            .message
                            .clone()
                            .or(Some("Step failed without message".into()));

                        // Handle on_error routing
                        if let Some(on_error) = &step.on_error {
                            current_key = on_error.clone();
                            continue;
                        }

                        // Trigger global hooks
                        for hook in &self.global_pipeline_hooks {
                            hook.failed(&mut exec_ctx).await;
                        }

                        // Stop pipeline here
                        return Err(EngineError::Action(
                            exec_ctx.error.clone().unwrap_or_default(),
                        ));
                    }

                    // Existing success flow
                    if let Some(next_key) = self.resolve_next_step(step, &exec_ctx)? {
                        current_key = next_key;
                    } else {
                        break;
                    }
                }

                Err(e) => {
                    // existing error handling remains
                    exec_ctx.error = Some(e.to_string());
                }
            }
        }

        for hook in &self.global_pipeline_hooks {
            hook.completed(&mut exec_ctx).await;
        }

        Ok(exec_ctx)
    }

    async fn execute_action_step(
        &self,
        step: &PipelineStep,
        ctx: &mut ExecutionContext,
    ) -> Result<ActionResult> {
        debug!("Executing step");

        match self.action_resolver.resolve(&step.action).await {
            Some(mut action) => {
                ctx.current_step = Some(step.clone());
                let mut step_config = step.config.clone();
                let config_mapper = JsonPathConfigResolver;
                // jsonpath evaluate or just plain text
                config_mapper.resolve(&mut step_config, ctx);

                action
                    .configure(step_config)
                    .await
                    .map_err(|e| EngineError::Other(e.to_string()))?;

                let mut mapped_runtime = ctx
                    .data
                    .get("payload")
                    .cloned()
                    .unwrap_or_else(|| json!({}));

                // allow jsonpaths inside runtime input too
                let ctx_json = build_jsonpath_context(ctx);
                resolve_jsonpaths(&mut mapped_runtime, &ctx_json);

                let merged_params = deep_merge(step.params.clone(), mapped_runtime);

                let executor = ActionExecutor::new(
                    step.key.clone(),
                    action,
                    self.global_action_hooks.clone(),
                    self.hook_resolver,
                    self.mapper.clone(),
                    self.cancel_token.clone(),
                    merged_params,
                );

                executor.execute(ctx).await
            }
            None => Err(EngineError::Action(format!(
                "Action '{}' not found",
                step.action
            ))),
        }
    }

    fn resolve_next_step(
        &self,
        step: &PipelineStep,
        ctx: &ExecutionContext,
    ) -> Result<Option<String>> {
        // Evaluate all conditional branches first
        for cond in &step.next_when {
            if self.evaluate_condition(&cond.when, ctx)? {
                return Ok(Some(cond.next.clone()));
            }
        }

        // If no condition matched, use the else path
        if let Some(else_key) = &step.otherwise {
            return Ok(Some(else_key.clone()));
        }

        // Default linear next
        Ok(step.next.clone())
    }

    fn evaluate_condition(&self, expr: &str, ctx: &ExecutionContext) -> Result<bool> {
        use serde_json::Value;

        let operators = ["==", "!=", ">=", "<=", ">", "<"];
        let (op, (left_raw, right_raw)) = operators
            .iter()
            .find_map(|&op| expr.split_once(op).map(|(l, r)| (op, (l.trim(), r.trim()))))
            .ok_or_else(|| EngineError::Other(format!("Invalid condition syntax: {expr}")))?;

        // Parse right-hand side as JSON if possible
        let right_val: Value = serde_json::from_str(right_raw)
            .unwrap_or_else(|_| json!(right_raw.trim_matches('"').trim_matches('\'')));

        // Build JSONPath context
        let ctx_json = build_jsonpath_context(ctx);

        // Resolve left-hand side
        let mut left_val = json!(left_raw);
        resolve_jsonpaths(&mut left_val, &ctx_json);

        // Compare
        Ok(match op {
            "==" => left_val == right_val,
            "!=" => left_val != right_val,
            ">" => compare_num(&left_val, &right_val, |a, b| a > b),
            ">=" => compare_num(&left_val, &right_val, |a, b| a >= b),
            "<" => compare_num(&left_val, &right_val, |a, b| a < b),
            "<=" => compare_num(&left_val, &right_val, |a, b| a <= b),
            _ => false,
        })
    }
}

// Helper for numeric comparisons
fn compare_num<F: Fn(f64, f64) -> bool>(a: &Value, b: &Value, cmp: F) -> bool {
    let Some(na) = a.as_f64() else { return false };
    let Some(nb) = b.as_f64() else { return false };
    cmp(na, nb)
}
