use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionManifest {
    pub actions: Vec<ActionDefinition>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionDefinition {
    pub runtime: RuntimeKind,
    pub kind: ActionKind,
    pub source: PathBuf,
    pub entrypoint: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    #[serde(default, skip_serializing_if = "ActionExecutionPolicy::is_default")]
    pub policy: ActionExecutionPolicy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionExecutionPolicy {
    #[serde(
        default = "default_timeout",
        skip_serializing_if = "is_default_timeout"
    )]
    pub timeout: String,
    #[serde(default, skip_serializing_if = "ActionRetryPolicy::is_default")]
    pub retry: ActionRetryPolicy,
}

impl Default for ActionExecutionPolicy {
    fn default() -> Self {
        Self {
            timeout: "3s".to_string(),
            retry: ActionRetryPolicy::default(),
        }
    }
}

impl ActionExecutionPolicy {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionRetryPolicy {
    #[serde(
        default = "default_max_attempts",
        skip_serializing_if = "is_default_max_attempts"
    )]
    pub max_attempts: u32,
    #[serde(
        default = "default_initial_delay",
        skip_serializing_if = "is_default_initial_delay"
    )]
    pub initial_delay: String,
    #[serde(
        default = "default_backoff",
        skip_serializing_if = "is_default_backoff"
    )]
    pub backoff: f64,
}

impl Default for ActionRetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 1,
            initial_delay: "1s".to_string(),
            backoff: 2.0,
        }
    }
}

impl ActionRetryPolicy {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

fn default_timeout() -> String {
    "3s".to_string()
}

fn is_default_timeout(value: &str) -> bool {
    value == "3s"
}

fn default_max_attempts() -> u32 {
    1
}

fn is_default_max_attempts(value: &u32) -> bool {
    *value == 1
}

fn default_initial_delay() -> String {
    "1s".to_string()
}

fn is_default_initial_delay(value: &str) -> bool {
    value == "1s"
}

fn default_backoff() -> f64 {
    2.0
}

fn is_default_backoff(value: &f64) -> bool {
    *value == 2.0
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimeKind {
    Python,
    Node,
    Rust,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionKind {
    Api(ApiAction),
    Authorizer(AuthorizerAction),
    Schedule(ScheduleAction),
    Flow(FlowAction),
    Queue(QueueAction),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiAction {
    pub method: String,
    pub path: String,

    #[serde(
        default = "default_media_types",
        skip_serializing_if = "is_default_media_types"
    )]
    pub consumes: Vec<String>,

    #[serde(
        default = "default_media_types",
        skip_serializing_if = "is_default_media_types"
    )]
    pub produces: Vec<String>,

    #[serde(default)]
    pub request_schema: Option<serde_json::Value>,

    #[serde(default)]
    pub response_schema: Option<serde_json::Value>,

    #[serde(default)]
    pub query_params: Vec<ApiQueryParam>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorizer: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiQueryParam {
    pub name: String,
    pub required: bool,
    pub schema: serde_json::Value,
}

fn default_media_types() -> Vec<String> {
    vec!["application/json".to_string()]
}

fn is_default_media_types(value: &[String]) -> bool {
    value == ["application/json"]
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleAction {
    pub expression: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorizerAction {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub security: Vec<AuthorizerSecurity>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<AuthorizerParameter>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorizerSecurity {
    #[serde(rename = "type")]
    pub security_type: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheme: Option<String>,

    #[serde(rename = "in", default, skip_serializing_if = "Option::is_none")]
    pub location: Option<AuthorizerParameterLocation>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorizerParameter {
    pub name: String,

    #[serde(rename = "in")]
    pub location: AuthorizerParameterLocation,

    #[serde(default)]
    pub required: bool,

    #[serde(rename = "type", default = "default_authorizer_parameter_type")]
    pub parameter_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthorizerParameterLocation {
    Header,
    Query,
    Cookie,
}

fn default_authorizer_parameter_type() -> String {
    "string".to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowAction {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueAction {
    pub queue: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_policy_defaults_to_three_second_timeout_and_one_attempt() {
        let policy = ActionExecutionPolicy::default();

        assert_eq!(policy.timeout, "3s");
        assert_eq!(policy.retry.max_attempts, 1);
        assert_eq!(policy.retry.initial_delay, "1s");
        assert_eq!(policy.retry.backoff, 2.0);
    }

    #[test]
    fn api_action_defaults_to_json_media_types() {
        let action: ApiAction = serde_json::from_value(serde_json::json!({
            "method": "GET",
            "path": "/hello"
        }))
        .expect("api action should deserialize");

        assert_eq!(action.consumes, vec!["application/json"]);
        assert_eq!(action.produces, vec!["application/json"]);
    }

    #[test]
    fn api_action_supports_optional_authorizer() {
        let action: ApiAction = serde_json::from_value(serde_json::json!({
            "method": "GET",
            "path": "/hello",
            "authorizer": "petstore"
        }))
        .expect("api action should deserialize");

        assert_eq!(action.authorizer.as_deref(), Some("petstore"));
    }

    #[test]
    fn authorizer_action_deserializes() {
        let kind: ActionKind = serde_json::from_value(serde_json::json!({
            "Authorizer": {}
        }))
        .expect("authorizer kind should deserialize");

        assert!(matches!(
            kind,
            ActionKind::Authorizer(AuthorizerAction {
                security,
                parameters,
            }) if security.is_empty() && parameters.is_empty()
        ));
    }

    #[test]
    fn authorizer_action_supports_security_and_parameters() {
        let action: AuthorizerAction = serde_json::from_value(serde_json::json!({
            "security": [
                { "type": "http", "scheme": "bearer" },
                { "type": "apiKey", "in": "header", "name": "X-API-Key" }
            ],
            "parameters": [
                { "name": "X-Tenant-ID", "in": "header", "required": true },
                { "name": "session", "in": "cookie", "required": false, "type": "string" }
            ]
        }))
        .expect("authorizer action should deserialize");

        assert_eq!(action.security.len(), 2);
        assert_eq!(action.security[0].security_type, "http");
        assert_eq!(action.security[0].scheme.as_deref(), Some("bearer"));
        assert_eq!(action.parameters[0].parameter_type, "string");
        assert_eq!(
            action.parameters[0].location,
            AuthorizerParameterLocation::Header
        );
    }
}
