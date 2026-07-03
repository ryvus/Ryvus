use async_trait::async_trait;
use ryvus_core::prelude::Action;
use std::collections::HashMap;
use std::sync::Arc;

/// The default resolver stores immutable, shared Action templates.
/// Each call to `.resolve()` produces a fresh clone (owned instance) for safe configuration and execution.
#[derive(Clone)]
pub struct DefaultActionResolver {
    registry: HashMap<(usize, String), Arc<dyn ActionFactory>>,
}

/// Internal trait to create new `Action` instances from stored templates.
/// This is similar to `Clone`, but object-safe for dynamic use.
pub trait ActionFactory: Send + Sync {
    fn create(&self) -> Box<dyn Action + Send + Sync>;
}

/// Blanket implementation for any clonable Action.
impl<A> ActionFactory for A
where
    A: Action + Clone + Send + Sync + 'static,
{
    fn create(&self) -> Box<dyn Action + Send + Sync> {
        Box::new(self.clone())
    }
}

impl DefaultActionResolver {
    pub fn new() -> Self {
        Self {
            registry: HashMap::new(),
        }
    }

    /// Registers a clonable Action template.
    pub fn register<A>(&mut self, action: A)
    where
        A: Action + Clone + Send + Sync + 'static,
    {
        let order = self.registry.len();
        let name = action.key().to_string();
        self.registry.insert((order, name), Arc::new(action));
    }

    /// Returns the number of registered actions.
    pub fn len(&self) -> usize {
        self.registry.len()
    }

    /// Returns all registered Actions as *new owned clones*.
    pub fn all(&self) -> Vec<Box<dyn Action + Send + Sync>> {
        let mut entries: Vec<_> = self.registry.iter().collect();
        entries.sort_by_key(|((order, _), _)| *order);
        entries.into_iter().map(|(_, a)| a.create()).collect()
    }
}

/// Trait for resolving actions dynamically.
#[async_trait]
pub trait ActionResolver: Send + Sync {
    async fn resolve(&self, key: &str) -> Option<Box<dyn Action + Send + Sync>>;

    fn all(&self) -> Vec<Box<dyn Action + Send + Sync>> {
        vec![]
    }

    fn len(&self) -> usize {
        0
    }
}

#[async_trait]
impl ActionResolver for DefaultActionResolver {
    async fn resolve(&self, key: &str) -> Option<Box<dyn Action + Send + Sync>> {
        self.registry
            .iter()
            .find(|((_, n), _)| n == key)
            .map(|(_, a)| a.create())
    }

    fn all(&self) -> Vec<Box<dyn Action + Send + Sync>> {
        DefaultActionResolver::all(self)
    }

    fn len(&self) -> usize {
        self.registry.len()
    }
}
