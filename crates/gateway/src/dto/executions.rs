use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateExecutionRequest {
    pub action: String,

    #[serde(default)]
    pub input: serde_json::Value,

    #[serde(default)]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ExecutionResponse {
    pub execution_id: String,
    pub status: ExecutionStatusResponse,

    #[serde(default)]
    pub output: serde_json::Value,

    #[serde(default)]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatusResponse {
    Pending,
    Running,
    Succeeded,
    Failed,
}
