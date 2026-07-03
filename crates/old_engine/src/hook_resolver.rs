use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use ryvus_core::{pipeline::hook::ActionHook, prelude::PipelineHook};

#[async_trait]
pub trait ActionHookResolver: Send + Sync {
    fn resolve(&self, _action_name: &str) -> Vec<Arc<dyn ActionHook>> {
        Vec::new()
    }
}

#[derive(Default, Clone)]
pub struct DefaultActionHookResolver {
    registry: HashMap<String, Arc<dyn ActionHook + Send + Sync>>,
}
impl ActionHookResolver for DefaultActionHookResolver {}

impl DefaultActionHookResolver {
    pub fn new() -> Self {
        Self {
            registry: HashMap::new(),
        }
    }

    pub fn register<H: ActionHook + Send + Sync + 'static>(&mut self, action_id: &str, hook: H) {
        self.registry.insert(action_id.to_string(), Arc::new(hook));
    }
}

#[async_trait]
pub trait PipelineHookResolver: Send + Sync {
    fn resolve(&self, _pipeline_name: &str) -> Vec<Arc<dyn PipelineHook>> {
        Vec::new()
    }
}

#[derive(Default, Clone)]
pub struct DefaultPipelineHookResolver {
    registry: Vec<Arc<dyn PipelineHook + Send + Sync>>,
}

impl PipelineHookResolver for DefaultPipelineHookResolver {}
impl DefaultPipelineHookResolver {
    pub fn new() -> Self {
        Self {
            registry: Vec::new(),
        }
    }

    pub fn register<H: PipelineHook + Send + Sync + 'static>(&mut self, hook: H) {
        let mut registry = self.registry.clone();
        registry.push(Arc::new(hook));
    }
}
