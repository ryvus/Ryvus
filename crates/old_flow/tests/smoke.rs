use ryvus_flow::prelude::*;

#[tokio::test]
async fn can_register_and_start() -> Result<(), FlowError> {
    let store = InMemoryStateStore::default();
    let mut manager = FlowPipelineManager::new(store);

    let pipeline = PipelineDefinition {
        key: "".to_string(),
        description: None,
        version: None,
        steps: Vec::new(),
        pipeline_hooks: Vec::new(),
    };
    manager.register(pipeline);
    manager.start("smoke").await
}
