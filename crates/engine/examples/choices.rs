use async_trait::async_trait;
use ryvus_core::{
    error::Error,
    prelude::{pipeline::Pipeline, Action, ActionContext, ActionResult, PipelineStep},
};
use ryvus_engine::{action_resolver::DefaultActionResolver, Engine};
use serde_json::json;

/// ------------------------------------------------
/// Dummy Actions
/// ------------------------------------------------

#[derive(Clone)]
struct LogAction {
    msg: String,
}

#[async_trait]
impl Action for LogAction {
    async fn execute(&self, ctx: &mut ActionContext) -> Result<ActionResult, Error> {
        let message = ctx
            .input
            .as_ref()
            .and_then(|p| p.get("message"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Action("Missing or invalid 'message'".into()))?
            .to_string();
        println!("{:?}", &message);
        Ok(ActionResult::success(json!({ "message": self.msg })))
    }

    fn key(&self) -> &str {
        "ryvus/log"
    }
}
#[derive(Clone)]
struct UploadActionUS {
    region: String,
}

#[async_trait]
impl Action for UploadActionUS {
    async fn execute(&self, _ctx: &mut ActionContext) -> Result<ActionResult, Error> {
        println!("Uploading to region '{}'", self.region);
        Ok(ActionResult::success(
            json!({ "region": self.region, "uploaded": true }),
        ))
    }

    fn key(&self) -> &str {
        "custom/upload_us"
    }
}

#[derive(Clone)]
struct UploadActionEU {
    region: String,
}

#[async_trait]
impl Action for UploadActionEU {
    async fn execute(&self, _ctx: &mut ActionContext) -> Result<ActionResult, Error> {
        println!("Uploading to region '{}'", self.region);
        Ok(ActionResult::success(
            json!({ "region": self.region, "uploaded": true }),
        ))
    }

    fn key(&self) -> &str {
        "custom/upload_eu"
    }
}

#[derive(Clone)]
struct FailAction;

#[async_trait]
impl Action for FailAction {
    async fn execute(&self, _ctx: &mut ActionContext) -> Result<ActionResult, Error> {
        println!("Simulated failure!");
        Err(Error::Unsupported("Intentional failure".into()))
    }

    fn key(&self) -> &str {
        "custom/fail"
    }
}

/// ------------------------------------------------
/// Example Entrypoint
/// ------------------------------------------------
#[tokio::main]
async fn main() -> Result<(), Error> {
    // Set up engine with default components
    let mut resolver = DefaultActionResolver::new();

    // Register example actions in resolver
    resolver.register(LogAction { msg: String::new() });
    resolver.register(UploadActionEU {
        region: "eu".into(),
    });
    resolver.register(UploadActionUS {
        region: "us".into(),
    });
    resolver.register(FailAction);

    // Define pipeline
    let pipeline = Pipeline::builder("region_upload_pipeline")
        .step(
            PipelineStep::builder("start", "ryvus/log")
                .params(json!({ "message": "Starting pipeline" }))
                .next("check_region")
                .build(),
        )
        .step(
            PipelineStep::builder("check_region", "ryvus/log")
                .params(json!({ "message": "Determining upload target..." }))
                .when("$.payload.region == 'eu'", "eu_upload")
                .when("$.payload.region == 'us'", "us_upload")
                .otherwise("upload_fallback")
                .on_error("error_handler")
                .build(),
        )
        .step(
            PipelineStep::builder("eu_upload", "custom/upload_eu")
                .params(json!({}))
                .next("done")
                .build(),
        )
        .step(
            PipelineStep::builder("us_upload", "custom/upload_us")
                .params(json!({}))
                .next("done")
                .build(),
        )
        .step(
            PipelineStep::builder("upload_fallback", "custom/fail")
                .params(json!({}))
                .on_error("error_handler")
                .build(),
        )
        .step(
            PipelineStep::builder("error_handler", "ryvus/log")
                .params(json!({ "message": "Handling failed upload" }))
                .next("done")
                .build(),
        )
        .step(
            PipelineStep::builder("done", "ryvus/log")
                .params(json!({ "message": "Pipeline complete" }))
                .build(),
        )
        .build();

    // Initialize engine
    let engine = Engine::default().with_action_resolver(resolver);

    let payloads = vec![
        json!({ "region": "eu" } ),
        json!({ "region": "us" }),
        json!({ "region": "apac" }),
    ];

    for payload in payloads {
        println!("\n=== Running pipeline with payload: {} ===", payload);
        let result = engine.execute(pipeline.clone(), payload).await;

        match result {
            Ok(res) => println!("Pipeline finished with status: {:?}", res.status),
            Err(e) => println!("Pipeline failed: {}", e),
        }
    }

    Ok(())
}
