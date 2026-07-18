use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ExecutionScopeIdError {
    #[error("execution scope must not be empty")]
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ExecutionScopeId(String);

impl ExecutionScopeId {
    pub fn new(value: impl Into<String>) -> Result<Self, ExecutionScopeIdError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(ExecutionScopeIdError::Empty);
        }
        Ok(Self(value))
    }

    pub fn local_default() -> Self {
        Self("local".to_string())
    }
}

impl std::fmt::Display for ExecutionScopeId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl AsRef<str> for ExecutionScopeId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ExecutionScopeId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_shape_remains_a_string() {
        let scope = ExecutionScopeId::new("tenant-a").expect("scope should be valid");

        assert_eq!(
            serde_json::to_value(scope).expect("scope should serialize"),
            serde_json::json!("tenant-a")
        );
    }

    #[test]
    fn deserialization_rejects_empty_scope() {
        assert!(serde_json::from_value::<ExecutionScopeId>(serde_json::json!("  ")).is_err());
    }
}
