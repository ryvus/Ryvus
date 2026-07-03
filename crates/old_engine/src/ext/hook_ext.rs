use crate::internal::hooked_action::HookedAction;
use ryvus_core::pipeline::hook::ActionHook;
use ryvus_core::prelude::Action;
use std::sync::Arc;

pub trait ActionExt: Action + Sized {
    fn with_hooks<H, I>(self, hooks: I) -> HookedAction<Self>
    where
        H: ActionHook + 'static,
        I: IntoIterator<Item = H>,
        Self: Clone,
    {
        let arc_hooks: Vec<Arc<dyn ActionHook>> = hooks
            .into_iter()
            .map(|h| Arc::new(h) as Arc<dyn ActionHook>)
            .collect();

        HookedAction::new(self).with_hooks(arc_hooks)
    }
}

impl<A: Action> ActionExt for A {}
