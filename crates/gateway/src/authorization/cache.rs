use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use ryvus_protocol::{ActionDefinition, ActionKind, AuthorizerParameterLocation};
use serde_json::Value;

use super::{decision::AuthorizationDecision, evaluator};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AuthorizationCacheKey {
    parts: Vec<(String, String)>,
}

impl AuthorizationCacheKey {
    pub fn from_identity_sources(
        authorizer: &ActionDefinition,
        authorizer_name: &str,
        headers: &serde_json::Map<String, Value>,
        query_params: &HashMap<String, String>,
    ) -> Option<Self> {
        let ActionKind::Authorizer(authorizer_config) = &authorizer.kind else {
            return None;
        };

        if authorizer_config.security.is_empty() && authorizer_config.parameters.is_empty() {
            return None;
        }

        let cookies = evaluator::parse_cookies(headers);
        let mut parts = vec![
            ("authorizer:name".to_string(), authorizer_name.to_string()),
            (
                "authorizer:source".to_string(),
                authorizer.source.display().to_string(),
            ),
            (
                "authorizer:entrypoint".to_string(),
                authorizer.entrypoint.clone(),
            ),
        ];

        for security in &authorizer_config.security {
            if security.security_type == "http"
                && security
                    .scheme
                    .as_deref()
                    .is_some_and(|scheme| scheme.eq_ignore_ascii_case("bearer"))
            {
                parts.push((
                    "security:header:authorization".to_string(),
                    headers
                        .get("authorization")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                ));
            } else if security.security_type == "apiKey" {
                let Some(name) = security.name.as_ref() else {
                    continue;
                };
                parts.push((
                    format!(
                        "security:{}:{}",
                        security
                            .location
                            .as_ref()
                            .map(evaluator::location_name)
                            .unwrap_or("unknown"),
                        name.to_ascii_lowercase()
                    ),
                    value_from_location(
                        security.location.as_ref(),
                        name,
                        headers,
                        query_params,
                        &cookies,
                    ),
                ));
            }
        }

        for parameter in &authorizer_config.parameters {
            parts.push((
                format!(
                    "parameter:{}:{}",
                    evaluator::location_name(&parameter.location),
                    parameter.name.to_ascii_lowercase()
                ),
                value_from_location(
                    Some(&parameter.location),
                    &parameter.name,
                    headers,
                    query_params,
                    &cookies,
                ),
            ));
        }

        parts.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));

        Some(Self { parts })
    }
}

pub trait AuthorizerCache: Send + Sync {
    fn get(&self, key: &AuthorizationCacheKey) -> Option<AuthorizationDecision>;
    fn put(&self, key: AuthorizationCacheKey, decision: AuthorizationDecision, ttl: Duration);
}

#[derive(Default)]
pub struct InMemoryAuthorizerCache {
    entries: Mutex<HashMap<AuthorizationCacheKey, CacheEntry>>,
}

struct CacheEntry {
    decision: AuthorizationDecision,
    expires_at: Instant,
}

impl AuthorizerCache for InMemoryAuthorizerCache {
    fn get(&self, key: &AuthorizationCacheKey) -> Option<AuthorizationDecision> {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let entry = entries.get(key)?;

        if entry.expires_at <= Instant::now() {
            entries.remove(key);
            return None;
        }

        Some(entry.decision.clone())
    }

    fn put(&self, key: AuthorizationCacheKey, decision: AuthorizationDecision, ttl: Duration) {
        if ttl.is_zero() {
            return;
        }

        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Instant::now();
        entries.retain(|_, entry| entry.expires_at > now);
        entries.insert(
            key,
            CacheEntry {
                decision,
                expires_at: now + ttl,
            },
        );
    }
}

fn value_from_location(
    location: Option<&AuthorizerParameterLocation>,
    name: &str,
    headers: &serde_json::Map<String, Value>,
    query_params: &HashMap<String, String>,
    cookies: &HashMap<String, String>,
) -> String {
    match location {
        Some(AuthorizerParameterLocation::Header) => headers
            .get(&name.to_ascii_lowercase())
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        Some(AuthorizerParameterLocation::Query) => {
            query_params.get(name).cloned().unwrap_or_default()
        }
        Some(AuthorizerParameterLocation::Cookie) => cookies.get(name).cloned().unwrap_or_default(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::{panic, path::PathBuf};

    use ryvus_protocol::{
        ActionDefinition, ActionExecutionPolicy, AuthorizerAction, AuthorizerCacheConfig,
        AuthorizerParameter, AuthorizerParameterLocation, AuthorizerSecurity, RuntimeKind,
    };

    use super::*;

    #[test]
    fn key_constructor_normalizes_identity_sources() {
        let action = authorizer_action(vec![AuthorizerParameter {
            name: "X-Tenant".to_string(),
            location: AuthorizerParameterLocation::Header,
            required: true,
            parameter_type: "string".to_string(),
        }]);
        let mut headers = serde_json::Map::new();
        headers.insert("x-tenant".to_string(), Value::String("acme".to_string()));

        let key = AuthorizationCacheKey::from_identity_sources(
            &action,
            "petstore",
            &headers,
            &HashMap::new(),
        );

        assert!(key.is_some());
    }

    #[test]
    fn poisoned_lock_does_not_panic_request_cache_access() {
        let cache = InMemoryAuthorizerCache::default();
        let key = AuthorizationCacheKey {
            parts: vec![("authorizer:name".to_string(), "petstore".to_string())],
        };

        let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            let _guard = cache.entries.lock().expect("cache should lock");
            panic!("poison cache");
        }));

        cache.put(
            key.clone(),
            AuthorizationDecision::Allow {
                principal_id: Some("user".to_string()),
                context: serde_json::Map::new(),
            },
            Duration::from_secs(60),
        );

        assert!(matches!(
            cache.get(&key),
            Some(AuthorizationDecision::Allow { .. })
        ));
    }

    fn authorizer_action(parameters: Vec<AuthorizerParameter>) -> ActionDefinition {
        ActionDefinition {
            runtime: RuntimeKind::Python,
            kind: ActionKind::Authorizer(AuthorizerAction {
                security: vec![AuthorizerSecurity {
                    security_type: "http".to_string(),
                    scheme: Some("bearer".to_string()),
                    location: None,
                    name: None,
                }],
                parameters,
                cache: Some(AuthorizerCacheConfig { ttl_seconds: 60 }),
            }),
            source: PathBuf::from("src/auth.py"),
            entrypoint: "auth".to_string(),
            name: Some("petstore".to_string()),
            policy: ActionExecutionPolicy::default(),
        }
    }
}
