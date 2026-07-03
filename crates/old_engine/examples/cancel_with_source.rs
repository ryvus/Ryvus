use async_trait::async_trait;
use ryvus_core::prelude::{
    pipeline::{Pipeline, PipelineStep},
    Action, ActionContext, ActionResult, Error,
};
use ryvus_engine::action_resolver::ActionResolver;
use ryvus_engine::{
    cancellation::{CancellationListener, CancellationSource},
    Engine,
};
use serde_json::json;
use std::{sync::Arc, time::Duration};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

/// ------------------------------------------------
/// Action definition
/// ------------------------------------------------
struct LongRunningAction(&'static str);

#[async_trait]
impl Action for LongRunningAction {
    async fn execute(&self, _ctx: &mut ActionContext) -> Result<ActionResult, Error> {
        println!("Starting '{}'", self.0);

        for i in 1..=10 {
            println!("{} step {}", self.0, i);
            sleep(Duration::from_millis(300)).await;
        }

        let result = json!({ "action": self.0, "status": "completed" });
        Ok(ActionResult::success(result))
    }

    fn key(&self) -> &str {
        self.0
    }
}

/// ------------------------------------------------
/// Timeout-based CancellationSource
/// ------------------------------------------------
pub struct TimeoutCancellationSource {
    delay_ms: u64,
}

impl TimeoutCancellationSource {
    pub fn new(delay_ms: u64) -> Self {
        Self { delay_ms }
    }
}

#[async_trait]
impl CancellationSource for TimeoutCancellationSource {
    async fn monitor(&self, token: CancellationToken) {
        println!("CancellationSource started ({}ms)...", self.delay_ms);
        sleep(Duration::from_millis(self.delay_ms)).await;
        println!("Timeout reached — canceling pipeline!");
        token.cancel();
    }
}

/// ------------------------------------------------
/// ActionResolver (creates LongRunningAction per step)
/// ------------------------------------------------
struct DummyResolver;

#[async_trait]
impl ActionResolver for DummyResolver {
    async fn resolve(&self, key: &str) -> Option<Box<dyn Action + Send + Sync>> {
        match key {
            "prepare_data" => Some(Box::new(LongRunningAction("prepare_data"))),
            "process_data" => Some(Box::new(LongRunningAction("process_data"))),
            _ => None,
        }
    }

    fn len(&self) -> usize {
        2
    }
}

/// ------------------------------------------------
/// Main demo
/// ------------------------------------------------
#[tokio::main]
async fn main() {
    println!("Starting pipeline with CancellationSource demo...");

    // Create listener + timeout source
    let mut listener = CancellationListener::new();
    let source = Arc::new(TimeoutCancellationSource::new(1500));
    listener.start(source);

    // Build engine with listener
    let engine = Engine::default()
        .with_cancel_listener(listener)
        .with_action_resolver(DummyResolver);

    // Define pipeline
    let pipeline = Pipeline {
        key: "cancel_source_demo".into(),
        steps: vec![
            PipelineStep::builder("prepare_data", "prepare_data")
                .next("process_data")
                .build(),
            PipelineStep::builder("process_data", "process_data").build(),
        ],
    };

    // Input
    let input = json!({ "test": "foobar" });

    // Execute
    match engine.execute(pipeline, input).await {
        Ok(_) => println!("Pipeline completed successfully"),
        Err(e) => println!("Pipeline stopped: {:?}", e),
    }

    println!("Done.");
}
