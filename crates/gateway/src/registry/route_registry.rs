use std::collections::HashMap;

use axum::http::Method;

use crate::config::routes::{GatewayConfig, HttpMethod, RouteDefinition};

pub struct RouteRegistry {
    routes: HashMap<(HttpMethod, String), RouteDefinition>,
}

impl RouteRegistry {
    pub fn from_config(config: GatewayConfig) -> Self {
        let routes = config
            .routes
            .into_iter()
            .map(|route| {
                let key = (route.method, route.path.clone());
                (key, route)
            })
            .collect();

        Self { routes }
    }
    pub fn resolve(&self, method: &Method, path: &str) -> Option<&RouteDefinition> {
        self.routes.get(&(method.into(), path.to_string()))
    }
}
