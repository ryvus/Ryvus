use crate::error::FlowError;
use async_trait::async_trait;

#[async_trait]
pub trait StateStore: Send + Sync + 'static {
    async fn save_state(&self, pipeline_id: &str, state: &str) -> Result<(), FlowError>;
    async fn load_state(&self, pipeline_id: &str) -> Result<Option<String>, FlowError>;
}

/// Simple in-memory state store for testing and examples.
#[derive(Default)]
pub struct InMemoryStateStore {
    pub states: tokio::sync::RwLock<std::collections::HashMap<String, String>>,
}

#[async_trait]
impl StateStore for InMemoryStateStore {
    async fn save_state(&self, pipeline_id: &str, state: &str) -> Result<(), FlowError> {
        let mut guard = self.states.write().await;
        guard.insert(pipeline_id.to_string(), state.to_string());
        Ok(())
    }

    async fn load_state(&self, pipeline_id: &str) -> Result<Option<String>, FlowError> {
        let guard = self.states.read().await;
        Ok(guard.get(pipeline_id).cloned())
    }
}
