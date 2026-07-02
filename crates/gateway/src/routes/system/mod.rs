use axum::{
    routing::{get, post},
    Router,
};

use crate::state::AppState;

use self::{
    health::health,
    schedules::{list_schedules, run_schedule},
};

pub mod health;
pub mod schedules;

pub fn system_routes() -> Router<AppState> {
    Router::new()
        .route("/system/health", get(health))
        .route("/system/schedules", get(list_schedules))
        .route("/system/schedules/{id}/run", post(run_schedule))
}
