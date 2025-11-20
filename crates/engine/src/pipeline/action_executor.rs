use crate::error::{EngineError, Result};
use crate::hook_resolver::ActionHookResolver;
use crate::mapper::mapper::Mapper;
use chrono::Utc;
use ryvus_core::error::Error;
use ryvus_core::pipeline::hook::ActionHook;
use ryvus_core::prelude::{Action, ActionContext, ActionResult, ExecutionContext, ExecutionStatus};
use serde_json::Value;
use std::sync::Arc;
use tokio::select;
use tokio_util::sync::CancellationToken;

/// Executes an Action, applying hooks, using a Mapper for input, and respecting cancellation.
///
/// Hooks are merged from global and action-specific sources.
/// The Mapper determines what input the Action receives based on the current ExecutionContext.
pub struct ActionExecutor<'a, M: Mapper, HR: ActionHookResolver> {
    pub key: String,
    pub action: Box<dyn Action>,
    pub global_hooks: Vec<Arc<dyn ActionHook>>,
    pub hook_resolver: &'a HR,
    pub mapper: Arc<M>,
    pub cancel_token: CancellationToken,
    pub params: Value,
}

impl<'a, M: Mapper, HR: ActionHookResolver> ActionExecutor<'a, M, HR> {
    pub fn new(
        key: String,
        action: Box<dyn Action>,
        global_hooks: Vec<Arc<dyn ActionHook>>,
        hook_resolver: &'a HR,
        mapper: Arc<M>,
        cancel_token: CancellationToken,
        params: Value,
    ) -> Self {
        Self {
            key,
            action,
            global_hooks,
            hook_resolver,
            mapper,
            cancel_token,
            params,
        }
    }

    /// Executes the Action within the given ExecutionContext.
    ///
    /// - Resolves hooks (global + dynamic)
    /// - Uses the mapper to populate ActionContext input
    /// - Executes the action or cancels on request
    /// - Runs before/after/error hooks appropriately
    pub async fn execute(&self, exec_ctx: &mut ExecutionContext) -> Result<ActionResult> {
        // Resolve hooks: global + action-specific
        let mut hooks = self.global_hooks.clone();
        hooks.extend(self.hook_resolver.resolve(self.action.key()));
        // Prepare input

        let mut ctx = ActionContext::new(&self.key, self.params.clone());

        for hook in &hooks {
            hook.before(&mut ctx).await;
        }

        let started_at = Utc::now();
        let result = select! {
            _ = self.cancel_token.cancelled() => Err(EngineError::Canceled),
            res = self.action.execute(&mut ctx) => res.map_err(|e| EngineError::Action(e.to_string())),
        };

        let finished_at = Utc::now();
        let duration_ms = (finished_at - started_at).num_milliseconds().max(0) as u64;

        // Handle result lifecycle
        let action_key = self.action.key().to_string();
        let action_id = ryvus_core::utils::id::generate_id("action_result");

        let action_result = match result {
            Ok(mut value) => {
                value.action = Some(self.action.key().to_string());
                let value_json = serde_json::to_value(&value).map_err(|e| {
                    EngineError::Action(format!("Could not serialize action result: {}", e))
                })?;

                ctx.set_result(value_json.clone());
                value.key = self.key.to_string();
                for hook in &hooks {
                    hook.after(&mut ctx).await;
                }

                value
            }
            Err(e) => {
                for hook in &hooks {
                    hook.error(&mut ctx, &Error::Action(e.to_string())).await;
                }

                ActionResult {
                    id: action_id,
                    action: Some(action_key.clone()),
                    status: if matches!(e, EngineError::Canceled) {
                        ExecutionStatus::Canceled
                    } else {
                        ExecutionStatus::Failed
                    },
                    output: None,
                    message: Some(e.to_string()),
                    started_at: Some(started_at),
                    finished_at: Some(Utc::now()),
                    duration_ms: Some(duration_ms),
                    key: action_key.clone(),
                }
            }
        };

        // Store it in the execution context too
        exec_ctx.insert_result(action_key, action_result.clone());
        Ok(action_result)
    }
}
