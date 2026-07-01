use axum::{routing::get, Router};

use crate::state::AppState;

use self::health::health;

pub mod health;

pub fn system_routes() -> Router<AppState> {
    Router::new().route("/system/health", get(health))
}
