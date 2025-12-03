use async_trait::async_trait;
use ryvus_core::error::Error;
use ryvus_core::prelude::{Action, ActionContext, ActionResult};
use tracing::debug;

use crate::error::EngineError;

/// Retries an Action up to `max_retries` times when it fails.
#[derive(Clone)]
pub struct RetryableAction<A: Action> {
    inner: A,
    max_retries: usize,
}

impl<A: Action> RetryableAction<A> {
    pub fn new(inner: A, max_retries: usize) -> Self {
        Self { inner, max_retries }
    }
}

#[async_trait]
impl<A> Action for RetryableAction<A>
where
    A: Action + Send + Sync,
{
    async fn execute(&self, ctx: &mut ActionContext) -> Result<ActionResult, Error> {
        let mut attempts = 0;
        debug!("RetryableAction exec {attempts}");
        loop {
            match self.inner.execute(ctx).await {
                Ok(res) => return Ok(res),
                Err(e) if attempts < self.max_retries => {
                    attempts += 1;
                    println!(
                        "Action {} failed (attempt {}/{}): {}",
                        self.inner.key(),
                        attempts,
                        self.max_retries,
                        e
                    );
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn key(&self) -> &str {
        self.inner.key()
    }
}
