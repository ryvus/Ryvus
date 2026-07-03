use async_trait::async_trait;
use ryvus_core::error::Error;
use ryvus_core::pipeline::hook::ActionHook;
use ryvus_core::prelude::{Action, ActionContext, ActionResult};
use std::sync::Arc;

/// Internal wrapper used by the engine and executors.
/// Not exposed directly to end users.
#[derive(Clone)]
pub struct HookedAction<A: Action> {
    pub(crate) inner: A,
    pub(crate) hooks: Vec<Arc<dyn ActionHook>>,
}

#[allow(unused_must_use)]
#[allow(unused_variables)]
#[allow(dead_code)]
impl<A: Action> HookedAction<A> {
    pub(crate) fn new(inner: A) -> Self {
        Self {
            inner,
            hooks: Vec::new(),
        }
    }

    pub(crate) fn with_hooks(mut self, hooks: Vec<Arc<dyn ActionHook>>) -> Self {
        self.hooks = hooks;
        self
    }

    pub(crate) fn hooks(&self) -> &[Arc<dyn ActionHook>] {
        &self.hooks
    }

    pub(crate) fn inner(&self) -> &A {
        &self.inner
    }
}

#[async_trait]
impl<A> Action for HookedAction<A>
where
    A: Action + Send + Sync,
{
    async fn execute(&self, ctx: &mut ActionContext) -> Result<ActionResult, Error> {
        for hook in &self.hooks {
            hook.before(ctx).await;
        }

        match self.inner.execute(ctx).await {
            Ok(result) => {
                for hook in &self.hooks {
                    hook.after(ctx).await
                }

                Ok(result)
            }
            Err(err) => {
                for hook in &self.hooks {
                    hook.error(ctx, &err).await;
                }

                Err(err)
            }
        }
    }

    fn key(&self) -> &str {
        self.inner.key()
    }
}
