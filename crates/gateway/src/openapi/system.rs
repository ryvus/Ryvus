use utoipa::OpenApi;

use crate::{
    dto::executions::{CreateExecutionRequest, ExecutionResponse, ExecutionStatusResponse},
    error::ErrorResponse,
};

#[derive(OpenApi)]
#[openapi(
paths(
    crate::routes::system::executions::create_execution,
    crate::routes::system::health::health,
),
    components(
        schemas(
            CreateExecutionRequest,
            ExecutionResponse,
            ExecutionStatusResponse,
            ErrorResponse,
        )
    ),
    tags(
        (name = "executions", description = "Ryvus execution endpoints")
    )
)]
pub struct ApiDoc;
