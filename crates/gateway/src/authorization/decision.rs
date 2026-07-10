use axum::http::StatusCode;
use serde_json::Value;

#[derive(Clone)]
pub enum AuthorizationDecision {
    Allow {
        principal_id: Option<String>,
        context: serde_json::Map<String, Value>,
    },
    Deny {
        status: StatusCode,
        code: &'static str,
        reason: String,
    },
}
