use axum::{
    extract::State,
    http::StatusCode,
    Json,
};
use serde_json::Value;

use crate::state::AppState;

pub async fn openapi(State(state): State<AppState>) -> Result<Json<Value>, StatusCode> {
    state
        .control_service
        .docs_registry()
        .json_page("/openapi.json")
        .cloned()
        .map(Json)
        .map_err(|_| StatusCode::NOT_FOUND)
}
