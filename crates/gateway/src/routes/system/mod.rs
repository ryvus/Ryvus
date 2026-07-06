use axum::{routing::get, Router};

use crate::state::AppState;

use self::health::health;
use self::openapi::openapi;

pub mod health;
pub mod openapi;

pub fn system_routes() -> Router<AppState> {
    Router::new()
        .route("/system/health", get(health))
        .route("/openapi.json", get(openapi))
}
