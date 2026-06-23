use std::collections::HashMap;

use axum::http::Method;
use ryvus_protocol::{ActionDefinition, ActionKind};

use crate::config::routes::{HttpMethod, RouteDefinition};

#[derive(Debug, Default)]
pub struct RouteRegistry {
    routes: HashMap<(HttpMethod, String), RouteDefinition>,
}

impl RouteRegistry {
    pub fn from_actions<'a>(actions: impl IntoIterator<Item = &'a ActionDefinition>) -> Self {
        let mut routes = HashMap::new();

        for action in actions {
            if let ActionKind::Api(api) = &action.kind {
                let method = match api.method.as_str() {
                    "GET" => HttpMethod::Get,
                    "POST" => HttpMethod::Post,
                    "PUT" => HttpMethod::Put,
                    "DELETE" => HttpMethod::Delete,
                    "PATCH" => HttpMethod::Patch,
                    value => panic!("unsupported HTTP method: {value}"),
                };

                let name = format!("{} {}", api.method, api.path);
                let action_key = format!("{}::{}", action.source.display(), action.entrypoint);

                let route = RouteDefinition {
                    name,
                    method: method.clone(),
                    path: api.path.clone(),
                    action: action_key,
                };
                routes.insert((method, api.path.clone()), route);
            }
        }

        Self { routes }
    }

    pub fn resolve(&self, method: &Method, path: &str) -> Option<&RouteDefinition> {
        let method = HttpMethod::from_axum(method)?;

        self.routes.get(&(method, path.to_string()))
    }
}

impl HttpMethod {
    pub fn from_axum(method: &Method) -> Option<Self> {
        match *method {
            Method::GET => Some(Self::Get),
            Method::POST => Some(Self::Post),
            Method::PUT => Some(Self::Put),
            Method::DELETE => Some(Self::Delete),
            Method::PATCH => Some(Self::Patch),
            _ => None,
        }
    }
}
