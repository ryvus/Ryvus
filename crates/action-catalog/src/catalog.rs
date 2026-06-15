use ryvus_executor::ActionDefinition;

use crate::ActionCatalogResult;

pub trait ActionCatalog: Send + Sync {
    fn resolve(&self, action: &str) -> ActionCatalogResult<&ActionDefinition>;
}
