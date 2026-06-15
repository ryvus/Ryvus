use std::{collections::HashMap, fs, path::Path};

use ryvus_executor::{ActionDefinition, RuntimeKind};
use serde::Deserialize;

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

        let config: ActionCatalogConfig =
            serde_json::from_str(&content).map_err(|source| ActionCatalogError::ParseFailed {
                path: path.to_path_buf(),
                source,
            })?;

        let actions = config
            .actions
            .into_iter()
            .map(|action| {
                let definition = ActionDefinition {
                    runtime: action.runtime,
                    source: action.source.into(),
                    handler: action.handler,
                };

                (action.name, definition)
            })
            .collect();

        Ok(Self { actions })
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

#[derive(Debug, Deserialize)]
struct ActionCatalogConfig {
    actions: Vec<FileActionDefinition>,
}

#[derive(Debug, Deserialize)]
struct FileActionDefinition {
    name: String,
    runtime: RuntimeKind,
    source: String,
    handler: String,
}
