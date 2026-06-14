use axum::{routing::post, Json, Router};

use crate::{
    dto::executions::{CreateExecutionRequest, ExecutionResponse, ExecutionStatusResponse},
    error::{ErrorResponse, GatewayError},
    state::AppState,
};

pub fn execution_routes() -> Router<AppState> {
    Router::new().route("/system/executions", post(create_execution))
}

// rest stays the same
#[utoipa::path(
    post,
    path = "/system/executions",
    request_body = CreateExecutionRequest,
    responses(
        (status = 200, description = "Execution completed", body = ExecutionResponse),
        (status = 500, description = "Execution failed", body = ErrorResponse),
    ),
    tag = "system"
)]
pub async fn create_execution(
    Json(request): Json<CreateExecutionRequest>,
) -> Result<Json<ExecutionResponse>, GatewayError> {
    let response = ExecutionResponse {
        execution_id: "temporary-execution-id".to_string(),
        status: ExecutionStatusResponse::Succeeded,
        output: serde_json::json!({
            "action": request.action,
            "input": request.input,
        }),
        metadata: request.metadata,
    };

    Ok(Json(response))
}
