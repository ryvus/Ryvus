use std::sync::Arc;

use ryvus_core::prelude::pipeline::Pipeline;

use crate::action_resolver::ActionResolver;

pub struct ResolvedPipeline {
    pub key: String,
    pub steps: Vec<ResolvedStep>,
}

pub struct ResolvedStep {
    pub action: Arc<dyn ryvus_core::prelude::Action + Send + Sync>,
    pub params: serde_json::Value,
}

impl ResolvedPipeline {
    pub async fn from_unresolved<P: ActionResolver + Sync>(
        p: &Pipeline,
        resolver: &P,
    ) -> crate::error::Result<Self> {
        let mut steps = Vec::with_capacity(p.steps.len());

        for s in &p.steps {
            // Resolve the action with its configuration/params
            let action = resolver.resolve(&s.action).await.unwrap();

            steps.push(ResolvedStep {
                action: Arc::from(action),
                params: s.params.clone(), // Keep params for Mapper or debugging
            });
        }

        Ok(Self {
            key: p.key.clone(),
            steps,
        })
    }
}
