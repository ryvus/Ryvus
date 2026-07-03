use crate::{error::FlowError, pipeline::PipelineDefinition, store::StateStore};
use std::sync::Arc;

/// FlowContext wraps execution and persistence concerns for a pipeline.
pub struct FlowContext<S: StateStore> {
    pub pipeline: PipelineDefinition,
    pub store: Arc<S>,
}

impl<S: StateStore> FlowContext<S> {
    pub fn new(pipeline: PipelineDefinition, store: Arc<S>) -> Self {
        Self { pipeline, store }
    }

    pub async fn run(&self) -> Result<(), FlowError> {
        // self.store.save_state(&self.pipeline.id, "started").await?;
        // Placeholder for real engine orchestration
        println!("Starting pipeline: {}", self.pipeline.key);
        // self.store.save_state(&self.pipeline.id, "finished").await?;
        Ok(())
    }
}
