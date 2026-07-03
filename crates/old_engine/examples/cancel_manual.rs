use async_trait::async_trait;
use ryvus_core::prelude::{
    pipeline::{Pipeline, PipelineStep},
    Action, ActionContext, ActionResult, Error,
};
use ryvus_engine::action_resolver::DefaultActionResolver;
use ryvus_engine::{
    cancellation::{CancellationListener, CancellationSource},
    Engine,
};
use serde_json::json;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

/// ----------------------------------------------
/// Long-running action
/// ----------------------------------------------
#[derive(Clone)]
struct RepetitiveAction(&'static str);

#[async_trait]
impl Action for RepetitiveAction {
    async fn execute(&self, ctx: &mut ActionContext) -> Result<ActionResult, Error> {
        println!("Starting '{}'", self.0);
        println!("Context: {:?}", ctx);

        for i in 1..=10 {
            println!("{} step {}", self.0, i);
            sleep(Duration::from_millis(300)).await;
        }

        let result = json!({ "action": self.0, "status": "done" });
        Ok(ActionResult::success(result))
    }

    fn key(&self) -> &str {
        self.0
    }
}

/// ----------------------------------------------
/// Manual cancellation source
/// ----------------------------------------------
pub struct ManualCancellationSource {
    triggered: Arc<AtomicBool>,
}

impl ManualCancellationSource {
    pub fn new() -> Self {
        Self {
            triggered: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn trigger_cancel(&self) {
        self.triggered.store(true, Ordering::SeqCst);
        println!("Manual cancellation triggered!");
    }
}

#[async_trait]
impl CancellationSource for ManualCancellationSource {
    async fn monitor(&self, token: CancellationToken) {
        loop {
            if token.is_cancelled() {
                break;
            }
            if self.triggered.load(Ordering::SeqCst) {
                token.cancel();
                break;
            }
            sleep(Duration::from_millis(100)).await;
        }
    }
}

/// ----------------------------------------------
/// Main demo
/// ----------------------------------------------
#[tokio::main]
async fn main() {
    println!("Starting manual cancellation demo...");

    // Create listener + source
    let mut listener = CancellationListener::new();
    let manual_source = Arc::new(ManualCancellationSource::new());
    listener.start(manual_source.clone());

    let mut default_action_resolver = DefaultActionResolver::new();
    default_action_resolver.register(RepetitiveAction("task_one"));
    default_action_resolver.register(RepetitiveAction("task_two"));

    // Build engine with listener and resolver
    let engine = Engine::default()
        .with_cancel_listener(listener)
        .with_action_resolver(default_action_resolver);

    let mut ps1 = PipelineStep::new(
        "task_one".to_string(),
        "task_one".to_string(),
        json!({}),
        json!({"test": "foobar"}),
    );

    ps1.next = Some("task_two".to_string());

    let ps2 = PipelineStep::new(
        "task_two".to_string(),
        "task_two".to_string(),
        json!({}),
        json!({"test": "foobar"}),
    );

    // Create pipeline with 2 long actions
    let pipeline = Pipeline {
        key: "manual_cancel_demo".into(),
        steps: vec![ps1, ps2],
    };

    // Trigger cancellation after 1 second
    let cancel_trigger = manual_source.clone();
    tokio::spawn(async move {
        sleep(Duration::from_millis(1000)).await;
        cancel_trigger.trigger_cancel();
    });

    // Execute
    match engine.execute(pipeline, json!({ "test": "foobar" })).await {
        Ok(_) => println!("Pipeline finished successfully"),
        Err(e) => println!("Pipeline stopped: {:?}", e),
    }

    println!("Done.");
}
