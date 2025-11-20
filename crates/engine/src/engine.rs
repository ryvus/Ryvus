use crate::action_resolver::{ActionResolver, DefaultActionResolver};
use crate::cancellation::CancellationListener;
use crate::error::{EngineError, Result};
use crate::hook_resolver::{
    ActionHookResolver, DefaultActionHookResolver, DefaultPipelineHookResolver,
    PipelineHookResolver,
};
use crate::mapper::mapper::{DefaultMapper, Mapper};
use crate::pipeline::pipeline_executor::PipelineExecutor;

use chrono::Utc;
use ryvus_core::action::result::{ExecutionMetrics, ExecutionResult};
use ryvus_core::pipeline::hook::ActionHook;
use ryvus_core::prelude::pipeline::Pipeline;
use ryvus_core::prelude::{Action, ActionContext, ExecutionStatus, PipelineHook};

use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::debug;

/// ------------------------------------------------------
/// Engine definition with ActionResolver support
/// ------------------------------------------------------
pub struct Engine<
    M = DefaultMapper,
    HR = DefaultActionHookResolver,
    PHR = DefaultPipelineHookResolver,
    AR = DefaultActionResolver,
> where
    M: Mapper + Send + Sync + 'static,
    HR: ActionHookResolver + Send + Sync + 'static,
    PHR: PipelineHookResolver + Send + Sync + 'static,
    AR: ActionResolver + Send + Sync + 'static,
{
    pub mapper: Arc<M>,
    pub global_action_hooks: Vec<Arc<dyn ActionHook>>,
    pub global_pipeline_hooks: Vec<Arc<dyn PipelineHook>>,
    pub action_hook_resolver: Arc<HR>,
    pub pipeline_hook_resolver: Arc<PHR>,
    pub action_resolver: Box<AR>,
    pub cancel_listener: Option<CancellationListener>,
}

impl<M, HR, PHR, AR> Engine<M, HR, PHR, AR>
where
    M: Mapper + Send + Sync + 'static,
    HR: ActionHookResolver + Send + Sync + 'static,
    PHR: PipelineHookResolver + Send + Sync + 'static,
    AR: ActionResolver + Send + Sync + 'static,
{
    pub fn new(
        mapper: M,
        hook_resolver: HR,
        pipeline_hook_resolver: PHR,
        action_resolver: AR,
    ) -> Self {
        Self {
            mapper: Arc::new(mapper),
            global_action_hooks: Vec::new(),
            global_pipeline_hooks: Vec::new(),
            action_hook_resolver: Arc::new(hook_resolver),
            pipeline_hook_resolver: Arc::new(pipeline_hook_resolver),
            action_resolver: Box::new(action_resolver),
            cancel_listener: None,
        }
    }

    pub fn with_mapper<NewM>(self, mapper: NewM) -> Engine<NewM, HR, PHR, AR>
    where
        NewM: Mapper + Send + Sync + 'static,
    {
        Engine {
            mapper: Arc::new(mapper),
            global_action_hooks: self.global_action_hooks,
            global_pipeline_hooks: self.global_pipeline_hooks,
            action_hook_resolver: self.action_hook_resolver,
            pipeline_hook_resolver: self.pipeline_hook_resolver,
            action_resolver: self.action_resolver,
            cancel_listener: self.cancel_listener,
        }
    }

    pub fn with_global_hooks(
        mut self,
        action_hooks: Vec<Arc<dyn ActionHook>>,
        pipeline_hooks: Vec<Arc<dyn PipelineHook>>,
    ) -> Self {
        self.global_action_hooks = action_hooks;
        self.global_pipeline_hooks = pipeline_hooks;
        self
    }

    pub fn with_action_hook_resolver<NewHR>(
        self,
        action_hook_resolver: NewHR,
    ) -> Engine<M, NewHR, PHR, AR>
    where
        NewHR: ActionHookResolver + Send + Sync + 'static,
    {
        Engine {
            mapper: self.mapper,
            global_action_hooks: self.global_action_hooks,
            global_pipeline_hooks: self.global_pipeline_hooks,
            action_hook_resolver: Arc::new(action_hook_resolver),
            pipeline_hook_resolver: self.pipeline_hook_resolver,
            action_resolver: self.action_resolver,
            cancel_listener: self.cancel_listener,
        }
    }

    pub fn with_pipeline_hook_resolver<NewPHR>(
        self,
        pipeline_hook_resolver: NewPHR,
    ) -> Engine<M, HR, NewPHR, AR>
    where
        NewPHR: PipelineHookResolver + Send + Sync + 'static,
    {
        Engine {
            mapper: self.mapper,
            global_action_hooks: self.global_action_hooks,
            global_pipeline_hooks: self.global_pipeline_hooks,
            action_hook_resolver: self.action_hook_resolver,
            pipeline_hook_resolver: Arc::new(pipeline_hook_resolver),
            action_resolver: self.action_resolver,
            cancel_listener: self.cancel_listener,
        }
    }

    pub fn with_action_resolver<NewAR>(self, action_resolver: NewAR) -> Engine<M, HR, PHR, NewAR>
    where
        NewAR: ActionResolver + Send + Sync + 'static,
    {
        #[cfg(debug_assertions)]
        {
            if self.action_resolver.len() > 0 {
                eprintln!("Replacing ActionResolver after registering actions — previous actions are discarded");
            }
        }
        Engine {
            mapper: self.mapper,
            global_action_hooks: self.global_action_hooks,
            global_pipeline_hooks: self.global_pipeline_hooks,
            action_hook_resolver: self.action_hook_resolver,
            pipeline_hook_resolver: self.pipeline_hook_resolver,
            action_resolver: Box::new(action_resolver),
            cancel_listener: self.cancel_listener,
        }
    }

    pub fn with_cancel_listener(mut self, listener: CancellationListener) -> Self {
        self.cancel_listener = Some(listener);
        self
    }

    pub fn cancel_token(&self) -> Option<CancellationToken> {
        self.cancel_listener.as_ref().map(|c| c.token())
    }

    /// Executes a pipeline with mapper, cancellation, hooks, and resolver support.
    pub async fn execute(&self, pipeline: Pipeline, input: Value) -> Result<ExecutionResult> {
        debug!("Exectuting pipeline {}", pipeline.key);
        // --- Setup cancellation token ---
        let cancel_token = self
            .cancel_listener
            .as_ref()
            .map(|c| c.token())
            .unwrap_or_else(CancellationToken::new);

        debug!("Cancel token {:?}", cancel_token);

        // --- Resolve hooks ---
        let pipeline_hooks = self.pipeline_hook_resolver.resolve(&pipeline.key);
        debug!("Pipeline hoogs: # {:?}", pipeline_hooks.len());

        let all_pipeline_hooks = [self.global_pipeline_hooks.clone(), pipeline_hooks].concat();
        debug!("Total hooks: # {:?}", all_pipeline_hooks.len());

        debug!("Create PipelineExecutor");
        let executor = PipelineExecutor::new(
            pipeline.clone(),
            self.mapper.clone(),
            self.global_action_hooks.clone(),
            all_pipeline_hooks,
            &*self.action_hook_resolver,
            &*self.action_resolver,
            cancel_token,
        );
        debug!("Executing pipeline");
        let mut ex_context = executor.execute(input).await?;
        debug!("Marking context as finsihed");
        ex_context.finish(); // Mark as finished if not already done

        debug!("Building metrics");
        let finished_at = ex_context.finished_at.unwrap_or_else(Utc::now);
        let duration_ms = (finished_at - ex_context.started_at)
            .num_milliseconds()
            .max(0) as u64;

        let steps_total = ex_context.steps.len();
        let steps_succeeded = ex_context
            .steps
            .iter()
            .filter(|s| s.status == ExecutionStatus::Success)
            .count();

        let steps_failed = ex_context
            .steps
            .iter()
            .filter(|s| s.status == ExecutionStatus::Failed)
            .count();

        debug!("Composing metrics");

        let metrics = ExecutionMetrics {
            started_at: ex_context.started_at,
            finished_at,
            duration_ms,
            steps_total,
            steps_succeeded,
            steps_failed,
        };

        debug!("Assemble final result");
        let result = ExecutionResult {
            run_id: ex_context.run_id.clone(),
            pipeline_key: Some(pipeline.key.clone()),
            environment: Some("local".to_string()),
            status: if ex_context.error.is_none() {
                ExecutionStatus::Success
            } else {
                ExecutionStatus::Failed
            },
            result: match ex_context.steps.last() {
                Some(val) => val.output.clone(),
                None => None,
            },
            error: ex_context.error,
            steps: ex_context.steps,
            metrics,
        };

        debug!("Pipeline execution finsihed");

        Ok(result)
    }

    /// ------------------------------------------------------
    /// Developer-friendly `run()` method:
    /// Executes all registered Actions sequentially without requiring a Pipeline.
    /// ------------------------------------------------------
    pub async fn run(&self, input: Value) -> Result<()> {
        let cancel_token = self
            .cancel_listener
            .as_ref()
            .map(|c| c.token())
            .unwrap_or_else(CancellationToken::new);

        let mut ctx = ActionContext::new("", input);

        // Optional enumeration support
        let actions = self.action_resolver.all();
        if actions.is_empty() {
            println!("ActionResolver does not support enumeration — nothing to run.");
            return Ok(());
        }

        for action in actions {
            let name = action.key();

            if cancel_token.is_cancelled() {
                println!("Execution cancelled before '{}'", name);
                break;
            }

            println!("Running action: {}", name);

            // Merge global + resolved hooks
            let mut hooks = vec![];
            hooks.extend(self.global_action_hooks.clone());
            hooks.extend(self.action_hook_resolver.resolve(name));

            if let Err(err) = action.execute(&mut ctx).await {
                println!("Action '{}' failed: {}", name, err);
                return Err(EngineError::Action(err.to_string()));
            }

            println!("Action '{}' completed", name);
        }

        Ok(())
    }
}

/// ------------------------------------------------------
/// Builder extensions for Default Resolvers only
/// ------------------------------------------------------

impl<M> Engine<M, DefaultActionHookResolver, DefaultPipelineHookResolver, DefaultActionResolver>
where
    M: Mapper + Send + Sync + 'static,
{
    pub fn with_action<A>(mut self, action: A) -> Self
    where
        A: Action + Clone + Send + Sync + 'static,
    {
        let resolver = self.action_resolver.as_mut();
        resolver.register(action);

        self
    }

    pub fn with_action_hook_for<H: ActionHook>(self, action_id: &str, hook: H) -> Self
    where
        H: ActionHook + 'static,
    {
        let mut resolver = (*self.action_hook_resolver).clone();
        resolver.register(action_id, hook);

        Self {
            action_hook_resolver: Arc::new(resolver),
            ..self
        }
    }

    pub fn with_action_hook(mut self, hook: Arc<dyn ActionHook>) -> Self {
        self.global_action_hooks.push(hook);
        self
    }

    pub fn with_pipeline_hook(mut self, hook: Arc<dyn PipelineHook>) -> Self {
        self.global_pipeline_hooks.push(hook);
        self
    }
}

/// ------------------------------------------------------
/// Dedicated impl for the default engine configuration.
/// ------------------------------------------------------
impl
    Engine<
        DefaultMapper,
        DefaultActionHookResolver,
        DefaultPipelineHookResolver,
        DefaultActionResolver,
    >
{
    pub fn default() -> Self {
        Self {
            mapper: Arc::new(DefaultMapper),
            global_action_hooks: Vec::new(),
            global_pipeline_hooks: Vec::new(),
            action_hook_resolver: Arc::new(DefaultActionHookResolver::new()),
            pipeline_hook_resolver: Arc::new(DefaultPipelineHookResolver::new()),
            action_resolver: Box::new(DefaultActionResolver::new()),
            cancel_listener: None,
        }
    }
}

/// ------------------------------------------------------
/// Public Engine API for Flow integration
/// ------------------------------------------------------

#[async_trait]
pub trait EngineApi: Send + Sync {
    async fn execute_pipeline(&self, pipeline: Pipeline, input: Value) -> Result<ExecutionResult>;
}

#[async_trait]
impl<M, HR, PHR, AR> EngineApi for Engine<M, HR, PHR, AR>
where
    M: Mapper + Send + Sync + 'static,
    HR: ActionHookResolver + Send + Sync + 'static,
    PHR: PipelineHookResolver + Send + Sync + 'static,
    AR: ActionResolver + Send + Sync + 'static,
{
    async fn execute_pipeline(&self, pipeline: Pipeline, input: Value) -> Result<ExecutionResult> {
        let _ = tracing_subscriber::fmt::try_init();

        // The engine itself already tracks start, finish, and metrics internally.
        // Just call it and propagate the result.

        let pipeline_key = &pipeline.key.clone();
        match self.execute(pipeline, input).await {
            Ok(result) => Ok(result),
            Err(err) => {
                // Create a lightweight failed ExecutionResult
                let now = chrono::Utc::now();
                Ok(ExecutionResult {
                    run_id: ryvus_core::utils::id::generate_id("run"),
                    pipeline_key: Some(pipeline_key.to_owned()),
                    environment: Some("local".into()),
                    status: ExecutionStatus::Failed,
                    error: Some(err.to_string()),
                    steps: vec![],
                    result: None,
                    metrics: ryvus_core::action::result::ExecutionMetrics {
                        started_at: now,
                        finished_at: now,
                        duration_ms: 0,
                        steps_total: 0,
                        steps_succeeded: 0,
                        steps_failed: 0,
                    },
                })
            }
        }
    }
}
