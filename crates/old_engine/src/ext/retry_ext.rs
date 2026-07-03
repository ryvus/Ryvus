use ryvus_core::prelude::Action;

use crate::internal::retryable_action::RetryableAction;

/// Extension trait that adds `.retryable()` to any Action.
pub trait RetryExt: Action + Sized {
    fn retryable(self, max_retries: usize) -> RetryableAction<Self> {
        RetryableAction::new(self, max_retries)
    }
}

impl<A: Action> RetryExt for A {}
