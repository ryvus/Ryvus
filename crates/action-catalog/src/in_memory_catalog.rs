use std::collections::HashMap;

use ryvus_executor::ActionDefinition;

use crate::{
    error::{ActionCatalogError, ActionCatalogResult},
    ActionCatalog,
};

pub struct InMemoryActionCatalog {
    actions: HashMap<String, ActionDefinition>,
}

impl InMemoryActionCatalog {
    pub fn new(actions: HashMap<String, ActionDefinition>) -> Self {
        Self { actions }
    }

    pub fn empty() -> Self {
        Self {
            actions: HashMap::new(),
        }
    }

    pub fn register(&mut self, name: impl Into<String>, action: ActionDefinition) {
        self.actions.insert(name.into(), action);
    }
}

impl ActionCatalog for InMemoryActionCatalog {
    fn resolve(&self, action: &str) -> ActionCatalogResult<&ActionDefinition> {
        self.actions
            .get(action)
            .ok_or_else(|| ActionCatalogError::ActionNotFound {
                action: action.to_string(),
            })
    }
}
