use std::{
    collections::{HashMap, HashSet},
    fs,
};

use super::variable::VariableResolver;

pub struct FileResolver {
    values: HashMap<String, String>,
    secrets: HashSet<String>,
}

// TODO rewrite
impl FileResolver {
    pub fn from_file(path: &str) -> Self {
        let content = fs::read_to_string(path).unwrap_or_default();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap_or_default();

        let mut values = HashMap::new();
        let mut secrets = HashSet::new();

        if let Some(obj) = json.as_object() {
            for (k, v) in obj {
                if k == "_secrets" {
                    if let Some(arr) = v.as_array() {
                        for s in arr {
                            if let Some(name) = s.as_str() {
                                secrets.insert(name.to_string());
                            }
                        }
                    }
                } else if let Some(val) = v.as_str() {
                    values.insert(k.clone(), val.to_string());
                }
            }
        }

        Self { values, secrets }
    }
}

impl VariableResolver for FileResolver {
    fn resolve(&self, key: &str) -> Option<String> {
        self.values.get(key).cloned()
    }

    fn is_secret(&self, key: &str) -> bool {
        self.secrets.contains(key)
    }

    fn secret_keys(&self) -> Vec<String> {
        self.secrets.iter().cloned().collect()
    }
}
