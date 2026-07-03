use ryvus_flow::prelude::*;

#[tokio::main]
async fn main() -> Result<(), FlowError> {
    let store = InMemoryStateStore::default();
    let mut manager = FlowPipelineManager::new(store);

    let pipeline = PipelineDefinition {
        key: "pipe1".to_string(),
        description: Some("Example pipeline".to_string()),
        version: Some("0.1.0".to_string()),
        steps: Vec::new(),
        pipeline_hooks: Vec::new(),
    };

    manager.register(pipeline);
    manager.start("pipe1").await?;
    Ok(())
}
