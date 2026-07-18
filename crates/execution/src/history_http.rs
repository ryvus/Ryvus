use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    extract::{rejection::QueryRejection, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use ryvus_protocol::ExecutionId;
use serde::Deserialize;
use serde_json::json;

use crate::{
    ExecutionHistoryPage, ExecutionHistoryQuery, ExecutionScopeId, ExecutionState,
    ExecutionStateStore, ExecutionTriggerKind, StateStoreError,
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
    state: Option<ExecutionState>,
    trigger: Option<ExecutionTriggerKind>,
    created_after_unix_ms: Option<i64>,
    created_before_unix_ms: Option<i64>,
    search: Option<String>,
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
    query: Result<Query<HistoryQuery>, QueryRejection>,
) -> Result<Json<ExecutionHistoryPage>, HistoryError> {
    let Query(query) = query.map_err(|_| HistoryError::InvalidQuery)?;
    let cursor = query
        .cursor
        .map(|cursor| {
            (!cursor.trim().is_empty())
                .then(|| ExecutionId::from(cursor))
                .ok_or(HistoryError::InvalidCursor)
        })
        .transpose()?;
    let created_after = query
        .created_after_unix_ms
        .map(system_time_from_unix_ms)
        .transpose()?;
    let created_before = query
        .created_before_unix_ms
        .map(system_time_from_unix_ms)
        .transpose()?;
    if created_after
        .zip(created_before)
        .is_some_and(|(after, before)| after >= before)
    {
        return Err(HistoryError::InvalidQuery);
    }
    let execution_id_prefix = query
        .search
        .map(|search| {
            (!search.trim().is_empty() && search.len() <= 128)
                .then_some(search)
                .ok_or(HistoryError::InvalidQuery)
        })
        .transpose()?;
    Ok(Json(state.store.list_history(ExecutionHistoryQuery {
        execution_scope_id: state.scope,
        action_id: query.action_id,
        action_revision: query.action_revision,
        state: query.state,
        trigger: query.trigger,
        created_after,
        created_before,
        execution_id_prefix,
        cursor,
        limit: query.limit.unwrap_or(100),
    })?))
}

fn system_time_from_unix_ms(value: i64) -> Result<SystemTime, HistoryError> {
    let duration = Duration::from_millis(value.unsigned_abs());
    if value >= 0 {
        UNIX_EPOCH.checked_add(duration)
    } else {
        UNIX_EPOCH.checked_sub(duration)
    }
    .ok_or(HistoryError::InvalidQuery)
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
    InvalidQuery,
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
            Self::InvalidQuery => (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "execution_invalid_query",
                    "message": "invalid execution history query",
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
    use std::time::{Duration, UNIX_EPOCH};

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
            assert_eq!(query.state, Some(crate::ExecutionState::Failed));
            assert_eq!(query.trigger, Some(crate::ExecutionTriggerKind::Api));
            assert_eq!(
                query.created_after,
                Some(UNIX_EPOCH + Duration::from_secs(10))
            );
            assert_eq!(
                query.created_before,
                Some(UNIX_EPOCH + Duration::from_secs(20))
            );
            assert_eq!(query.execution_id_prefix.as_deref(), Some("exec-fail"));
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
            "/internal/executions?limit=1&action_id=inventory&action_revision=revision-1&state=failed&trigger=api&created_after_unix_ms=10000&created_before_unix_ms=20000&search=exec-fail",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(first, json!({ "items": [], "next_cursor": "execution-1" }));

        let (status, second) = request(
            app,
            Method::GET,
            "/internal/executions?limit=1&action_id=inventory&action_revision=revision-1&state=failed&trigger=api&created_after_unix_ms=10000&created_before_unix_ms=20000&search=exec-fail&cursor=execution-1",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(second, json!({ "items": [], "next_cursor": null }));
    }

    #[tokio::test]
    async fn invalid_history_filters_return_stable_bad_requests() {
        let store = MockExecutionStateStore::new();
        let app = app(store);
        let long_search = "x".repeat(129);
        let uris = [
            "/internal/executions?created_after_unix_ms=not-a-date".to_string(),
            "/internal/executions?created_after_unix_ms=20&created_before_unix_ms=20".to_string(),
            "/internal/executions?created_after_unix_ms=21&created_before_unix_ms=20".to_string(),
            "/internal/executions?search=%20".to_string(),
            format!("/internal/executions?search={long_search}"),
        ];

        for uri in uris {
            let (status, body) = request(app.clone(), Method::GET, &uri).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(
                body,
                json!({
                    "error": "execution_invalid_query",
                    "message": "invalid execution history query",
                })
            );
        }
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
