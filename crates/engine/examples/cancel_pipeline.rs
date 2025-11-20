use async_trait::async_trait;
use ryvus_core::prelude::{
    pipeline::{Pipeline, PipelineStep},
    Action, ActionContext, ActionResult, Error,
};
use ryvus_engine::{action_resolver::ActionResolver, cancellation::CancellationListener, Engine};
use serde_json::json;
use std::time::Duration;
use tokio::time::sleep;

/// ------------------------------------------------
/// A slow cancellable action
/// ------------------------------------------------
#[derive(Clone)]
struct SlowAction {
    name: &'static str,
}

#[async_trait]
impl Action for SlowAction {
    async fn execute(&self, ctx: &mut ActionContext) -> Result<ActionResult, Error> {
        println!("Starting action '{}'", self.name);

        // Read delay from context parameters (injected by engine)
        let delay_ms = ctx
            .input
            .clone()
            .unwrap()
            .get("delay_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(300);

        for i in 1..=10 {
            println!("{} progress step {}", self.name, i);
            sleep(Duration::from_millis(delay_ms)).await;
        }

        let result = json!({
            "action": self.name,
            "status": "completed"
        });

        ctx.set_result(result.clone());
        println!("Finished action '{}'", self.name);

        Ok(ActionResult::success(result))
    }

    fn key(&self) -> &str {
        self.name
    }
}

/// ------------------------------------------------
/// Simple resolver for SlowAction (no config)
/// ------------------------------------------------
#[derive(Clone)]
struct DummyResolver;

#[async_trait]
impl ActionResolver for DummyResolver {
    async fn resolve(&self, key: &str) -> Option<Box<dyn Action + Send + Sync>> {
        match key {
            "first" => Some(Box::new(SlowAction { name: "first" })),
            "second" => Some(Box::new(SlowAction { name: "second" })),
            _ => None,
        }
    }

    fn len(&self) -> usize {
        2
    }
}

/// ------------------------------------------------
/// Main: Pipeline + cancellation
/// ------------------------------------------------
#[tokio::main]
async fn main() {
    println!("Starting pipeline with cancellable actions...");

    // Create a cancellation listener
    let listener = CancellationListener::new();

    // Clone its token so we can cancel externally
    let cancel_token = listener.token();

    // Trigger cancellation after 1.5 seconds
    tokio::spawn(async move {
        sleep(Duration::from_millis(1500)).await;
        println!("Triggering cancellation...");
        cancel_token.cancel();
    });

    // Build engine
    let engine = Engine::default()
        .with_cancel_listener(listener)
        .with_action_resolver(DummyResolver);

    // Create pipeline definition (string-based)
    let pipeline = Pipeline {
        key: "cancel_demo".into(),
        steps: vec![
            PipelineStep::builder("sfdo", "first")
                .params(json!({"delay_ms": 300}))
                .next("second")
                .build(),
            PipelineStep::builder("second", "second")
                .params(json!({"delay_ms": 300}))
                .build(),
        ],
    };

    let input = json!({ "test": "foobar" });

    // Execute pipeline
    match engine.execute(pipeline, input).await {
        Ok(_) => println!("Pipeline completed successfully"),
        Err(e) => println!("Pipeline stopped: {:?}", e),
    }

    println!("Done.");
}
