use async_trait::async_trait;
use ryvus_core::{
    pipeline::hook::ActionHook,
    prelude::{
        pipeline::{Pipeline, PipelineStep},
        Action, ActionContext, ActionResult, Error, ExecutionContext, PipelineHook,
    },
};
use ryvus_engine::{action_resolver::ActionResolver, Engine};
use serde_json::json;
use std::sync::Arc;

/// ------------------------------------------------
/// Action
/// ------------------------------------------------
#[derive(Clone)]
struct DummyAction;

#[async_trait]
impl Action for DummyAction {
    async fn execute(&self, _ctx: &mut ActionContext) -> Result<ActionResult, Error> {
        println!("Executing DummyAction");
        Ok(ActionResult::success(json!({ "status": "ok" })))
    }

    fn key(&self) -> &str {
        "DummyAction"
    }
}

/// ------------------------------------------------
/// Hook (both ActionHook + PipelineHook)
/// ------------------------------------------------
struct PrintHook(&'static str);

#[async_trait]
impl ActionHook for PrintHook {
    async fn before(&self, _ctx: &mut ActionContext) {
        println!("ActionHook before ({})", self.0);
    }

    async fn after(&self, _ctx: &mut ActionContext) {
        println!("ActionHook after ({})", self.0);
    }

    async fn error(&self, _ctx: &mut ActionContext, _e: &Error) {
        println!("ActionHook error ({})", self.0);
    }
}

#[async_trait]
impl PipelineHook for PrintHook {
    async fn start(&self, _ctx: &mut ExecutionContext) {
        println!("Pipeline start ({})", self.0);
    }

    async fn completed(&self, _ctx: &mut ExecutionContext) {
        println!("Pipeline completed ({})", self.0);
    }

    async fn failed(&self, _ctx: &mut ExecutionContext) {
        println!("Pipeline failed ({})", self.0);
    }

    async fn canceled(&self, _ctx: &mut ExecutionContext) {
        println!("Pipeline canceled ({})", self.0);
    }
}

/// ------------------------------------------------
/// Resolver providing DummyAction
/// ------------------------------------------------
struct DummyResolver;

#[async_trait]
impl ActionResolver for DummyResolver {
    async fn resolve(
        &self,
        name: &str,
    ) -> Option<Box<dyn ryvus_core::prelude::Action + Send + Sync>> {
        if name == "dummy" {
            Some(Box::new(DummyAction))
        } else {
            None
        }
    }
}

/// ------------------------------------------------
/// Main
/// ------------------------------------------------
#[tokio::main]
async fn main() {
    let pipeline = Pipeline {
        key: "with_hooks".into(),

        steps: vec![PipelineStep::builder("dummy", "dummy").build()],
    };

    let input = json!({ "test": "foobar" });

    // Register a hook both as global ActionHook and PipelineHook
    let global_hook = Arc::new(PrintHook("global"));

    let engine = Engine::default()
        .with_global_hooks(vec![global_hook.clone()], vec![global_hook.clone()])
        .with_action_resolver(DummyResolver);

    let result = engine.execute(pipeline, input).await;
    println!("Result: {:?}", result);
}
