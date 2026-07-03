use async_trait::async_trait;
use ryvus_core::{
    pipeline::hook::ActionHook,
    prelude::{Action, ActionContext, ActionResult, Error},
};
use ryvus_engine::{prelude::RetryExt, ActionExt, Engine};
use serde_json::json;

/// ------------------------------------------------
/// Example Action
/// ------------------------------------------------
#[derive(Clone)]
struct DummyAction(&'static str);

#[async_trait]
impl Action for DummyAction {
    async fn execute(&self, _ctx: &mut ActionContext) -> Result<ActionResult, Error> {
        println!("Executing action '{}'", self.0);
        Ok(ActionResult::success(json!({ "status": "ok" })))
    }

    fn key(&self) -> &str {
        self.0
    }
}

/// ------------------------------------------------
/// Hook that prints before/after/error
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

    async fn error(&self, _ctx: &mut ActionContext, _err: &Error) {
        println!("ActionHook error ({})", self.0);
    }
}

/// ------------------------------------------------
/// Main
/// ------------------------------------------------
#[tokio::main]
async fn main() {
    let input = json!({ "test": "foobar" });

    let dum = DummyAction("foobar")
        .with_hooks(vec![PrintHook("foobar")])
        .retryable(100);
    let dum2 = DummyAction("foobar2").with_hooks(vec![PrintHook("foobar2")]);

    // ✅ Engine uses the correct resolvers
    let engine = Engine::default().with_action(dum).with_action(dum2);
    let result = engine.run(input).await;

    println!("Result: {:?}", result);
}
