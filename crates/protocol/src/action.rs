use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionManifest {
    pub actions: Vec<ActionDefinition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionDefinition {
    pub runtime: RuntimeKind,
    pub kind: ActionKind,
    pub source: PathBuf,
    pub entrypoint: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimeKind {
    Python,
    Node,
    Rust,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionKind {
    Api(ApiAction),
    Schedule(ScheduleAction),
    Queue(QueueAction),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiAction {
    pub method: String,
    pub path: String,

    #[serde(default)]
    pub request_schema: Option<serde_json::Value>,

    #[serde(default)]
    pub response_schema: Option<serde_json::Value>,

    #[serde(default)]
    pub query_params: Vec<ApiQueryParam>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiQueryParam {
    pub name: String,
    pub required: bool,
    pub schema: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleAction {
    pub expression: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueAction {
    pub queue: String,
}
