use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use ryvus_protocol::ExecutionId;
use serde::Deserialize;
use serde_json::json;

use crate::{ExecutionHistoryQuery, ExecutionScopeId, ExecutionStateStore, StateStoreError};

#[derive(Clone)]
struct HistoryState {
    store: Arc<dyn ExecutionStateStore>,
    scope: ExecutionScopeId,
}

#[derive(Debug, Deserialize)]
struct HistoryQuery {
    action_id: Option<String>,
    action_revision: Option<String>,
    limit: Option<usize>,
}

pub fn execution_history_routes(
    store: Arc<dyn ExecutionStateStore>,
    scope: ExecutionScopeId,
) -> Router {
    Router::new()
        .route("/internal/executions", get(list_executions))
        .route("/internal/executions/{id}", get(get_execution))
        .with_state(HistoryState { store, scope })
}

async fn list_executions(
    State(state): State<HistoryState>,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<serde_json::Value>, HistoryError> {
    Ok(Json(json!(state.store.list_history(
        ExecutionHistoryQuery {
            execution_scope_id: state.scope,
            action_id: query.action_id,
            action_revision: query.action_revision,
            limit: query.limit.unwrap_or(100),
        }
    )?)))
}

async fn get_execution(
    State(state): State<HistoryState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, HistoryError> {
    let execution = state
        .store
        .load(&ExecutionId::from(id))?
        .filter(|execution| execution.execution_scope_id == state.scope)
        .ok_or(HistoryError::NotFound)?;
    Ok(Json(json!(execution)))
}

enum HistoryError {
    NotFound,
    Store(StateStoreError),
}

impl From<StateStoreError> for HistoryError {
    fn from(error: StateStoreError) -> Self {
        Self::Store(error)
    }
}

impl IntoResponse for HistoryError {
    fn into_response(self) -> axum::response::Response {
        match self {
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "execution_not_found" })),
            )
                .into_response(),
            Self::Store(error) => (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "error": "execution_store_unavailable",
                    "message": error.to_string(),
                })),
            )
                .into_response(),
        }
    }
}
