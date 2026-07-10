use std::{collections::HashMap, fs, path::Path};

use ryvus_protocol::{ActionDefinition, ActionKind, ActionManifest};

use crate::{
    catalog::ActionCatalog,
    error::{ActionCatalogError, ActionCatalogResult},
};

#[derive(Debug, Clone)]
pub struct FileActionCatalog {
    actions: HashMap<String, ActionDefinition>,
    authorizers_by_name: HashMap<String, String>,
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

        Ok(Self::from_actions(manifest.actions))
    }

    pub fn from_actions(actions: Vec<ActionDefinition>) -> Self {
        let mut keyed = HashMap::new();
        let mut authorizers_by_name = HashMap::new();

        for action in actions {
            let key = action_key(&action);
            if matches!(action.kind, ActionKind::Authorizer(_)) {
                if let Some(name) = &action.name {
                    authorizers_by_name.insert(name.clone(), key.clone());
                }
            }
            keyed.insert(key, action);
        }

        Self {
            actions: keyed,
            authorizers_by_name,
        }
    }

    pub fn all(&self) -> impl Iterator<Item = &ActionDefinition> {
        self.actions.values()
    }

    pub fn resolve_authorizer(&self, name: &str) -> ActionCatalogResult<&ActionDefinition> {
        let key = self.authorizers_by_name.get(name).ok_or_else(|| {
            ActionCatalogError::ActionNotFound {
                action: name.to_string(),
            }
        })?;

        self.resolve(key)
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ryvus_protocol::{
        ActionDefinition, ActionExecutionPolicy, ActionKind, AuthorizerAction, RuntimeKind,
    };

    use super::FileActionCatalog;

    #[test]
    fn resolves_authorizer_by_name() {
        let action = ActionDefinition {
            runtime: RuntimeKind::Python,
            kind: ActionKind::Authorizer(AuthorizerAction {
                security: Vec::new(),
                parameters: Vec::new(),
                cache: None,
            }),
            source: PathBuf::from("src/auth.py"),
            entrypoint: "auth".to_string(),
            name: Some("petstore".to_string()),
            policy: ActionExecutionPolicy::default(),
        };
        let catalog = FileActionCatalog::from_actions(vec![action]);

        let resolved = catalog
            .resolve_authorizer("petstore")
            .expect("authorizer should resolve");

        assert_eq!(resolved.entrypoint, "auth");
    }
}
