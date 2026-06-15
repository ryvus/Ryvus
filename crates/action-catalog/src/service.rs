use ryvus_executor::ActionDefinition;

use crate::{catalog::ActionCatalog, error::ActionCatalogResult};

pub struct ActionService<C> {
    catalog: C,
}

impl<C> ActionService<C>
where
    C: ActionCatalog,
{
    pub fn new(catalog: C) -> Self {
        Self { catalog }
    }

    pub fn resolve_action(&self, action: &str) -> ActionCatalogResult<&ActionDefinition> {
        self.catalog.resolve(action)
    }

    pub fn catalog(&self) -> &C {
        &self.catalog
    }
}
