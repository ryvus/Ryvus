use axum::{routing::get, Router};

use crate::state::AppState;

use self::{executions::execution_routes, health::health};

pub mod executions;
pub mod health;

pub fn system_routes() -> Router<AppState> {
    Router::new()
        .route("/system/health", get(health))
        .merge(execution_routes())
}
