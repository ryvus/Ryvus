use std::{collections::HashMap, fs, path::Path};

use ryvus_protocol::{ActionDefinition, ActionManifest};

use crate::{
    catalog::ActionCatalog,
    error::{ActionCatalogError, ActionCatalogResult},
};

#[derive(Debug, Clone)]
pub struct FileActionCatalog {
    actions: HashMap<String, ActionDefinition>,
}

impl FileActionCatalog {
    pub fn load(path: impl AsRef<Path>) -> ActionCatalogResult<Self> {
        let path = path.as_ref();

        let content =
            fs::read_to_string(path).map_err(|source| ActionCatalogError::LoadFailed {
                path: path.to_path_buf(),
                source,
            })?;

        let manifest: ActionManifest =
            serde_json::from_str(&content).map_err(|source| ActionCatalogError::ParseFailed {
                path: path.to_path_buf(),
                source,
            })?;

        let actions = manifest
            .actions
            .into_iter()
            .map(|action| {
                let key = action_key(&action);
                (key, action)
            })
            .collect();

        Ok(Self { actions })
    }

    pub fn all(&self) -> impl Iterator<Item = &ActionDefinition> {
        self.actions.values()
    }
}

impl ActionCatalog for FileActionCatalog {
    fn resolve(&self, action: &str) -> ActionCatalogResult<&ActionDefinition> {
        self.actions
            .get(action)
            .ok_or_else(|| ActionCatalogError::ActionNotFound {
                action: action.to_string(),
            })
    }
}

fn action_key(action: &ActionDefinition) -> String {
    format!("{}::{}", action.source.display(), action.entrypoint)
}
