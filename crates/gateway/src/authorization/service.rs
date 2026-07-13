use std::{collections::HashMap, sync::Arc, time::Duration};

use axum::http::StatusCode;
use ryvus_control::ControlService;
use ryvus_protocol::{ActionDefinition, ActionKind, InvocationRequest, InvocationStatus};
use serde_json::Value;

use crate::{
    authorization::{
        cache::{AuthorizationCacheKey, AuthorizerCache},
        decision::AuthorizationDecision,
        evaluator,
    },
    routes::public::errors::{action_error_status, execution_error_status},
    state::GatewayExecutionService,
};

#[derive(Clone)]
pub struct AuthorizationService {
    control_service: Arc<ControlService>,
    execution_service: Arc<GatewayExecutionService>,
    cache: Arc<dyn AuthorizerCache>,
}

pub struct AuthorizationRequest {
    pub authorizer_name: String,
    pub body: Value,
    pub path_params: HashMap<String, String>,
    pub query_params: HashMap<String, String>,
    pub headers: serde_json::Map<String, Value>,
    pub method: String,
    pub path: String,
}

pub struct AuthorizationFailure {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl AuthorizationService {
    pub fn new(
        control_service: Arc<ControlService>,
        execution_service: Arc<GatewayExecutionService>,
        cache: Arc<dyn AuthorizerCache>,
    ) -> Self {
        Self {
            control_service,
            execution_service,
            cache,
        }
    }

    pub fn authorize(
        &self,
        request: AuthorizationRequest,
    ) -> Result<AuthorizationDecision, AuthorizationFailure> {
        let authorizer = self
            .control_service
            .resolve_authorizer(&request.authorizer_name)
            .map_err(|error| AuthorizationFailure {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "authorizer_not_found",
                message: format!("{}: {}", request.authorizer_name, error),
            })?;

        evaluator::validate_authorizer_security(
            authorizer,
            &request.headers,
            &request.query_params,
        )
        .map_err(|message| AuthorizationFailure {
            status: StatusCode::UNAUTHORIZED,
            code: "unauthorized",
            message,
        })?;

        evaluator::validate_authorizer_parameters(
            authorizer,
            &request.headers,
            &request.query_params,
        )
        .map_err(|message| AuthorizationFailure {
            status: StatusCode::BAD_REQUEST,
            code: "request_validation_failed",
            message,
        })?;

        let cache_key = authorizer_cache_ttl(authorizer).and_then(|_| {
            AuthorizationCacheKey::from_identity_sources(
                authorizer,
                &request.authorizer_name,
                &request.headers,
                &request.query_params,
            )
        });

        if let Some(key) = cache_key.as_ref() {
            if let Some(decision) = self.cache.get(key) {
                return Ok(decision);
            }
        }

        let authorizer_event = serde_json::json!({
            "body": request.body,
            "path_params": request.path_params,
            "query_params": request.query_params,
            "headers": request.headers,
            "method": request.method,
            "path": request.path,
        });

        let decision =
            self.execute_authorizer(authorizer, &request.authorizer_name, authorizer_event)?;

        if let AuthorizationDecision::Allow { .. } = &decision {
            if let Some((key, ttl)) =
                cache_key.and_then(|key| authorizer_cache_ttl(authorizer).map(|ttl| (key, ttl)))
            {
                self.cache.put(key, decision.clone(), ttl);
            }
        }

        Ok(decision)
    }

    fn execute_authorizer(
        &self,
        authorizer: &ActionDefinition,
        authorizer_name: &str,
        event: Value,
    ) -> Result<AuthorizationDecision, AuthorizationFailure> {
        let request = InvocationRequest::new(event);

        let policy = ryvus_execution::ExecutionPolicy::from_action_policy(&authorizer.policy)
            .map_err(|error| AuthorizationFailure {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "authorizer_failed",
                message: error.to_string(),
            })?;

        let record = self
            .execution_service
            .execute(authorizer, &request, &policy)
            .map_err(|error| AuthorizationFailure {
                status: execution_error_status(&error),
                code: "authorizer_failed",
                message: error.to_string(),
            })?;

        let result = record.result.invocation_result;

        if result.status != InvocationStatus::Success {
            return Err(match result.error {
                Some(error) => AuthorizationFailure {
                    status: if error.code == "Timeout" {
                        action_error_status(&error.code)
                    } else {
                        StatusCode::INTERNAL_SERVER_ERROR
                    },
                    code: "authorizer_failed",
                    message: error.message,
                },
                None => AuthorizationFailure {
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                    code: "authorizer_failed",
                    message: format!("authorizer `{authorizer_name}` failed"),
                },
            });
        }

        evaluator::parse_authorizer_decision(result.output.unwrap_or(Value::Null)).map_err(
            |message| AuthorizationFailure {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "authorizer_failed",
                message,
            },
        )
    }
}

fn authorizer_cache_ttl(authorizer: &ActionDefinition) -> Option<Duration> {
    let ActionKind::Authorizer(authorizer) = &authorizer.kind else {
        return None;
    };

    authorizer
        .cache
        .as_ref()
        .filter(|cache| cache.ttl_seconds > 0)
        .map(|cache| Duration::from_secs(cache.ttl_seconds))
}
