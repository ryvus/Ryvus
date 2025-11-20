use async_trait::async_trait;
use ryvus_core::prelude::{Action, ActionContext, ActionResult, Error};
use ryvus_engine::{mapper::mapper::JsonMapper, Engine};
use ryvus_flow::flow::{EngineAdapter, FlowExecutor};
use serde_json::{json, Value};

#[derive(Clone, Default)]
struct LogAction {
    pub test: Option<String>,
}

#[async_trait]
impl Action for LogAction {
    async fn execute(&self, ctx: &mut ActionContext) -> Result<ActionResult, Error> {
        let params = ctx.input.clone().unwrap();

        let message = params
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("No message");
        let result = ActionResult::success(json!({ "message": message }));

        Ok(result)
    }

    async fn configure(&mut self, config: Value) -> Result<(), String> {
        let s = config.get("password");
        match s {
            Some(val) => {
                if let Some(s) = val.as_str() {
                    println!("{}", s);
                }
                self.test = Some(val.to_string())
            }
            None => self.test = Some("Test was not".to_string()),
        }

        Ok(())
    }

    fn key(&self) -> &str {
        "ryvus/log"
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Build an engine and register the single LogAction
    let engine = Engine::default()
        .with_action(LogAction::default())
        .with_mapper(JsonMapper);

    // Wrap engine in Flow adapter
    let adapter = EngineAdapter::new(engine);

    // Run a pipeline definition from file
    let result = adapter
        .start_pipeline(
            "/home/mbollemeijer/dev/ryvus/flow/examples/from_file/steps.json".into(),
            json!({ "context": "from_main" }),
        )
        .await?;

    let json_result = serde_json::to_string_pretty(&json!(result))?;
    println!("{}", json_result);
    Ok(())
}
