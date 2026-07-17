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

use crate::{
    ExecutionHistoryPage, ExecutionHistoryQuery, ExecutionScopeId, ExecutionStateStore,
    StateStoreError,
};

#[derive(Clone)]
struct HistoryState {
    store: Arc<dyn ExecutionStateStore>,
    scope: ExecutionScopeId,
}

#[derive(Debug, Deserialize)]
struct HistoryQuery {
    action_id: Option<String>,
    action_revision: Option<String>,
    cursor: Option<String>,
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
) -> Result<Json<ExecutionHistoryPage>, HistoryError> {
    let cursor = query
        .cursor
        .map(|cursor| {
            (!cursor.trim().is_empty())
                .then(|| ExecutionId::from(cursor))
                .ok_or(HistoryError::InvalidCursor)
        })
        .transpose()?;
    Ok(Json(state.store.list_history(ExecutionHistoryQuery {
        execution_scope_id: state.scope,
        action_id: query.action_id,
        action_revision: query.action_revision,
        cursor,
        limit: query.limit.unwrap_or(100),
    })?))
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
    InvalidCursor,
    Store,
}

impl From<StateStoreError> for HistoryError {
    fn from(error: StateStoreError) -> Self {
        match error {
            StateStoreError::InvalidHistoryCursor { .. } => Self::InvalidCursor,
            _ => Self::Store,
        }
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
            Self::InvalidCursor => (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "execution_invalid_cursor",
                    "message": "invalid execution cursor",
                })),
            )
                .into_response(),
            Self::Store => (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "error": "execution_store_unavailable",
                    "message": "execution store unavailable",
                })),
            )
                .into_response(),
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::{
        body::{to_bytes, Body},
        http::{Method, Request},
    };
    use serde_json::Value;
    use tower::ServiceExt;

    use crate::{ExecutionHistoryPage, MockExecutionStateStore};

    use super::*;

    #[tokio::test]
    async fn collection_route_returns_pages_and_forwards_filters_and_cursor() {
        let mut store = MockExecutionStateStore::new();
        store.expect_list_history().times(2).returning(|query| {
            assert_eq!(query.execution_scope_id.as_ref(), "trusted-scope");
            assert_eq!(query.limit, 1);
            assert_eq!(query.action_id.as_deref(), Some("inventory"));
            assert_eq!(query.action_revision.as_deref(), Some("revision-1"));
            if let Some(cursor) = &query.cursor {
                assert_eq!(cursor.as_ref(), "execution-1");
            }
            Ok(ExecutionHistoryPage {
                items: Vec::new(),
                next_cursor: query
                    .cursor
                    .is_none()
                    .then(|| ExecutionId::from("execution-1")),
            })
        });
        let app = app(store);

        let (status, first) = request(
            app.clone(),
            Method::GET,
            "/internal/executions?limit=1&action_id=inventory&action_revision=revision-1",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(first, json!({ "items": [], "next_cursor": "execution-1" }));

        let (status, second) = request(
            app,
            Method::GET,
            "/internal/executions?limit=1&action_id=inventory&action_revision=revision-1&cursor=execution-1",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(second, json!({ "items": [], "next_cursor": null }));
    }

    #[tokio::test]
    async fn history_errors_are_stable_and_safe() {
        let mut store = MockExecutionStateStore::new();
        store.expect_list_history().times(2).returning(|query| {
            if let Some(cursor) = query.cursor {
                Err(StateStoreError::InvalidHistoryCursor { cursor })
            } else {
                Err(StateStoreError::Backend(
                    "postgres://admin:secret@database/executions".into(),
                ))
            }
        });
        store.expect_load().return_once(|_| Ok(None));
        let app = app(store);

        let (status, invalid) = request(
            app.clone(),
            Method::GET,
            "/internal/executions?cursor=unknown",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(invalid["error"], "execution_invalid_cursor");

        let (status, missing) =
            request(app.clone(), Method::GET, "/internal/executions/unknown").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(missing["error"], "execution_not_found");

        let (status, unavailable) = request(app, Method::GET, "/internal/executions").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(unavailable["error"], "execution_store_unavailable");
        assert!(!unavailable.to_string().contains("admin:secret"));
    }

    fn app(store: MockExecutionStateStore) -> Router {
        execution_history_routes(
            Arc::new(store),
            ExecutionScopeId::new("trusted-scope").expect("scope should be valid"),
        )
    }

    async fn request(app: Router, method: Method, uri: &str) -> (StatusCode, Value) {
        let response = app
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("request should be handled");
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response should read");
        let body = serde_json::from_slice(&bytes).expect("response should be json");
        (status, body)
    }
}
