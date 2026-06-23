use ryvus_protocol::ActionDefinition;

use crate::ActionCatalogResult;

pub trait ActionCatalog: Send + Sync {
    fn resolve(&self, action: &str) -> ActionCatalogResult<&ActionDefinition>;
}
