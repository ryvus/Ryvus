use async_trait::async_trait;
use ryvus_core::prelude::{Action, ActionContext, ActionResult, Error};
use ryvus_engine::prelude::RetryExt;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

/// A flaky action that fails twice before succeeding.
struct FlakyAction {
    name: &'static str,
    fail_until: usize,
    attempts: AtomicUsize,
}

#[async_trait]
impl Action for FlakyAction {
    async fn execute(&self, ctx: &mut ActionContext) -> Result<ActionResult, Error> {
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
        if attempt <= self.fail_until {
            return Err(Error::Action(format!(
                "Simulated failure on attempt {}",
                attempt
            )));
        }

        // Return success
        let result = json!({
            "action": self.name,
            "attempts": attempt,
            "status": "Success"
        });

        ctx.set_result(result.clone());
        Ok(ActionResult::success(result))
    }

    fn key(&self) -> &str {
        self.name
    }
}

#[tokio::main]
async fn main() {
    // Configure a flaky action that fails 2 times before succeeding.
    let flaky = FlakyAction {
        name: "unstable_download",
        fail_until: 2,
        attempts: AtomicUsize::new(0),
    }
    .retryable(3);

    // Create an action context.
    let mut ctx = ActionContext::default();
    let start = Instant::now();

    // Execute the action (it will retry internally)
    match flaky.execute(&mut ctx).await {
        Ok(result) => {
            println!("Final result after retries: {:#?}", result);
        }
        Err(e) => {
            println!("Final failure: {:?}", e);
        }
    }

    println!("Total duration: {} ms", start.elapsed().as_millis());
}
