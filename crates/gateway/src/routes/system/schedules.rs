use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use ryvus_protocol::InvocationStatus;
use serde::Serialize;
use serde_json::{json, Value};

use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct ScheduleRunResponse {
    pub invocation_id: String,
    pub status: String,
    pub output: Option<Value>,
}

pub async fn list_schedules(State(state): State<AppState>) -> Json<Value> {
    let schedules = state
        .control_service
        .schedule_infos()
        .expect("loaded schedule expressions should already be validated");

    Json(json!(schedules
        .into_iter()
        .map(|schedule| {
            json!({
                "id": schedule.id,
                "name": schedule.name,
                "source": schedule.source,
                "entrypoint": schedule.entrypoint,
                "expression": schedule.expression,
                "action_key": schedule.action_key,
            })
        })
        .collect::<Vec<_>>()))
}

pub async fn run_schedule(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ScheduleRunResponse>, (StatusCode, Json<Value>)> {
    let result = ryvus_scheduler::run_schedule_once(
        state.control_service.action_catalog().all(),
        &id,
        state.execution_service.clone(),
    )
    .map_err(|error| {
        let status = match error {
            ryvus_scheduler::SchedulerError::ScheduleNotFound { .. } => StatusCode::NOT_FOUND,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };

        (
            status,
            Json(json!({
                "error": "schedule_execution_failed",
                "message": error.to_string(),
            })),
        )
    })?;

    Ok(Json(ScheduleRunResponse {
        invocation_id: result.invocation_id,
        status: status_label(&result.status).to_string(),
        output: result.output,
    }))
}

fn status_label(status: &InvocationStatus) -> &'static str {
    match status {
        InvocationStatus::Success => "success",
        InvocationStatus::Failed => "failed",
    }
}
