use async_trait::async_trait;
use ryvus_core::{
    pipeline::{hook::ActionHook, pipeline::PipelineStep},
    prelude::{pipeline::Pipeline, Action, ActionContext, ActionResult, Error},
};
use ryvus_engine::{ActionExt, Engine};
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
    //  Pipeline uses steps with string-based action names
    let pipeline = Pipeline {
        key: "cmd_hooks".into(),
        steps: vec![PipelineStep::builder("foobar", "foobar").build()],
    };

    let input = json!({ "test": "foobar" });

    let dum = DummyAction("foobar").with_hooks(vec![PrintHook("foobar")]);

    // Engine uses the correct resolvers
    let engine = Engine::default().with_action(dum);
    let result = engine.execute(pipeline, input).await;

    println!("Result: {:?}", result);
}
