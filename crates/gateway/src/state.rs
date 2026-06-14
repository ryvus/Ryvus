use std::sync::Arc;

use crate::registry::route_registry::RouteRegistry;

#[derive(Clone)]
pub struct AppState {
    pub route_registry: Arc<RouteRegistry>,
}
