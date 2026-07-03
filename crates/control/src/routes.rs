use std::collections::{HashMap, HashSet};

use ryvus_protocol::{ActionDefinition, ActionKind};
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Default)]
pub struct RouteRegistry {
    routes: Vec<RouteDefinition>,
}

impl RouteRegistry {
    pub fn from_actions<'a>(
        actions: impl IntoIterator<Item = &'a ActionDefinition>,
    ) -> Result<Self, RouteRegistryError> {
        let mut routes = Vec::new();
        let mut seen = HashSet::new();

        for action in actions {
            if let ActionKind::Api(api) = &action.kind {
                validate_path_template(&api.path)?;

                let method = HttpMethod::parse(&api.method).ok_or_else(|| {
                    RouteRegistryError::UnsupportedMethod {
                        method: api.method.clone(),
                        path: api.path.clone(),
                    }
                })?;

                let route_key = (method, normalize_path_template(&api.path));

                if !seen.insert(route_key) {
                    return Err(RouteRegistryError::DuplicateRoute {
                        method: api.method.clone(),
                        path: api.path.clone(),
                    });
                }

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

        Ok(Self { routes })
    }

    pub fn resolve(&self, method: &str, path: &str) -> Option<RouteMatch> {
        let method = HttpMethod::parse(method)?;

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

    pub fn path_exists(&self, path: &str) -> bool {
        self.routes
            .iter()
            .any(|route| match_path(&route.path, path).is_some())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RouteDefinition {
    pub name: String,
    pub method: HttpMethod,
    pub path: String,
    pub action: String,
}

#[derive(Debug, Clone)]
pub struct RouteMatch {
    pub definition: RouteDefinition,
    pub path_params: HashMap<String, String>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl HttpMethod {
    fn parse(method: &str) -> Option<Self> {
        match method.to_ascii_uppercase().as_str() {
            "GET" => Some(Self::Get),
            "POST" => Some(Self::Post),
            "PUT" => Some(Self::Put),
            "PATCH" => Some(Self::Patch),
            "DELETE" => Some(Self::Delete),
            _ => None,
        }
    }
}

#[derive(Debug, Error)]
pub enum RouteRegistryError {
    #[error("unsupported HTTP method `{method}` for route `{path}`")]
    UnsupportedMethod { method: String, path: String },

    #[error("duplicate route `{method} {path}`")]
    DuplicateRoute { method: String, path: String },

    #[error("invalid route path `{path}`: {reason}")]
    InvalidPath { path: String, reason: String },
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

fn validate_path_template(path: &str) -> Result<(), RouteRegistryError> {
    if !path.starts_with('/') {
        return Err(RouteRegistryError::InvalidPath {
            path: path.to_string(),
            reason: "path must start with `/`".to_string(),
        });
    }

    for part in path.trim_matches('/').split('/') {
        let starts = part.starts_with('{');
        let ends = part.ends_with('}');

        if starts != ends {
            return Err(RouteRegistryError::InvalidPath {
                path: path.to_string(),
                reason: "path parameters must use `{name}`".to_string(),
            });
        }

        if starts
            && part
                .trim_start_matches('{')
                .trim_end_matches('}')
                .is_empty()
        {
            return Err(RouteRegistryError::InvalidPath {
                path: path.to_string(),
                reason: "path parameter name cannot be empty".to_string(),
            });
        }
    }

    Ok(())
}

fn normalize_path_template(path: &str) -> String {
    path.trim_matches('/')
        .split('/')
        .map(|part| {
            if part.starts_with('{') && part.ends_with('}') {
                "{}"
            } else {
                part
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}
