use std::collections::HashMap;

use axum::http::Method;
use ryvus_protocol::{ActionDefinition, ActionKind};

use crate::config::routes::{HttpMethod, RouteDefinition, RouteMatch};

#[derive(Debug, Default)]
pub struct RouteRegistry {
    routes: Vec<RouteDefinition>,
}

impl RouteRegistry {
    pub fn from_actions<'a>(actions: impl IntoIterator<Item = &'a ActionDefinition>) -> Self {
        let mut routes = Vec::new();

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

                routes.push(RouteDefinition {
                    name,
                    method,
                    path: api.path.clone(),
                    action: action_key,
                });
            }
        }

        Self { routes }
    }

    pub fn resolve(&self, method: &Method, path: &str) -> Option<RouteMatch> {
        let method = HttpMethod::from_axum(method)?;

        self.routes.iter().find_map(|route| {
            if route.method != method {
                return None;
            }

            match_path(&route.path, path).map(|path_params| RouteMatch {
                definition: route.clone(),
                path_params,
            })
        })
    }
}

fn match_path(template: &str, path: &str) -> Option<HashMap<String, String>> {
    let template_parts: Vec<_> = template.trim_matches('/').split('/').collect();
    let path_parts: Vec<_> = path.trim_matches('/').split('/').collect();

    if template_parts.len() != path_parts.len() {
        return None;
    }

    let mut params = HashMap::new();

    for (template_part, path_part) in template_parts.iter().zip(path_parts.iter()) {
        if template_part.starts_with('{') && template_part.ends_with('}') {
            let param_name = template_part.trim_start_matches('{').trim_end_matches('}');

            params.insert(param_name.to_string(), path_part.to_string());
            continue;
        }

        if template_part != path_part {
            return None;
        }
    }

    Some(params)
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
