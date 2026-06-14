use axum::{
    body::{to_bytes, Body},
    extract::State,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
    Json,
};

use crate::state::AppState;

pub async fn handle_dynamic_route(
    State(state): State<AppState>,
    request: Request<Body>,
) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();

    let body = match to_bytes(request.into_body(), usize::MAX).await {
        Ok(body) => body,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "failed_to_read_request_body",
                    "message": error.to_string(),
                })),
            )
                .into_response();
        }
    };

    match state.route_registry.resolve(&method, &path) {
        Some(route) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "status": "matched",
                "route": route.name,
                "action": route.action,
                "body_size": body.len(),
            })),
        )
            .into_response(),

        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "route_not_configured",
                "method": method.to_string(),
                "path": path,
            })),
        )
            .into_response(),
    }
}
