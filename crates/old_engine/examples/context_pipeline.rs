use async_trait::async_trait;
use ryvus_core::prelude::{
    pipeline::{Pipeline, PipelineStep},
    Action, ActionContext, ActionResult, Error,
};
use ryvus_engine::{action_resolver::ActionResolver, Engine};
use serde_json::json;

/// ------------------------------------------------
/// DummyAction – just prints and returns results
/// ------------------------------------------------
#[derive(Clone)]
struct DummyAction(&'static str);

#[async_trait]
impl Action for DummyAction {
    async fn execute(&self, ctx: &mut ActionContext) -> Result<ActionResult, Error> {
        println!("Executing action: {}", self.0);

        if let Some(input) = &ctx.input {
            println!("Input for {}: {}", self.0, input);
        }

        let result = json!({
            "action": self.0,
            "output": format!("{} completed", self.0)
        });

        ctx.set_result(result.clone());

        Ok(ActionResult::success(result))
    }

    fn key(&self) -> &str {
        self.0
    }
}

/// ------------------------------------------------
/// Simple resolver providing DummyAction by name
/// ------------------------------------------------
struct DummyResolver;

#[async_trait]
impl ActionResolver for DummyResolver {
    fn len(&self) -> usize {
        3
    }
    async fn resolve(
        &self,
        name: &str,
    ) -> Option<Box<dyn ryvus_core::prelude::Action + Send + Sync>> {
        match name {
            "first" => Some(Box::new(DummyAction("first"))),
            "second" => Some(Box::new(DummyAction("second"))),
            "third" => Some(Box::new(DummyAction("third"))),
            _ => None,
        }
    }
}

/// ------------------------------------------------
/// Main
/// ------------------------------------------------
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Define pipeline (now string-based)
    let pipeline = Pipeline {
        key: "context_demo".into(),
        steps: vec![
            PipelineStep::builder("first", "first")
                .next("second")
                .build(),
            PipelineStep::builder("second", "second")
                .next("third")
                .build(),
            PipelineStep::builder("third", "third").build(),
        ],
    };

    // Build engine using current architecture
    let engine = Engine::default().with_action_resolver(DummyResolver);

    // Execute the full pipeline — this will print all steps and results
    engine
        .execute(
            pipeline,
            json!({
                "test": "foobar"
            }),
        )
        .await?;

    Ok(())
}
