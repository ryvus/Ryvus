use axum::http::Method;
use serde::Deserialize;

use std::{fs, path::Path};

#[derive(Debug, Clone, Deserialize)]
pub struct GatewayConfig {
    pub routes: Vec<RouteDefinition>,
}

impl GatewayConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, GatewayConfigError> {
        let path = path.as_ref();

        let content = fs::read_to_string(path).map_err(|source| GatewayConfigError::Read {
            path: path.display().to_string(),
            source,
        })?;

        serde_json::from_str(&content).map_err(|source| GatewayConfigError::Parse {
            path: path.display().to_string(),
            source,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GatewayConfigError {
    #[error("failed to read gateway config at {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },

    #[error("failed to parse gateway config at {path}: {source}")]
    Parse {
        path: String,
        source: serde_json::Error,
    },
}

#[derive(Debug, Clone, Deserialize)]
pub struct RouteDefinition {
    pub name: String,
    pub method: HttpMethod,
    pub path: String,
    pub action: String,
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

impl From<&Method> for HttpMethod {
    fn from(method: &Method) -> Self {
        match *method {
            Method::GET => HttpMethod::Get,
            Method::POST => HttpMethod::Post,
            Method::PUT => HttpMethod::Put,
            Method::PATCH => HttpMethod::Patch,
            Method::DELETE => HttpMethod::Delete,
            _ => HttpMethod::Get,
        }
    }
}
