use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use crate::routes::public::dynamic::AuthorizerDecision;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AuthorizerCacheKey(pub Vec<(String, String)>);

pub trait AuthorizerCache: Send + Sync {
    fn get(&self, key: &AuthorizerCacheKey) -> Option<AuthorizerDecision>;
    fn put(&self, key: AuthorizerCacheKey, decision: AuthorizerDecision, ttl: Duration);
}

#[derive(Default)]
pub struct InMemoryAuthorizerCache {
    entries: Mutex<HashMap<AuthorizerCacheKey, CacheEntry>>,
}

struct CacheEntry {
    decision: AuthorizerDecision,
    expires_at: Instant,
}

impl AuthorizerCache for InMemoryAuthorizerCache {
    fn get(&self, key: &AuthorizerCacheKey) -> Option<AuthorizerDecision> {
        let mut entries = self.entries.lock().expect("authorizer cache should lock");
        let entry = entries.get(key)?;

        if entry.expires_at <= Instant::now() {
            entries.remove(key);
            return None;
        }

        Some(entry.decision.clone())
    }

    fn put(&self, key: AuthorizerCacheKey, decision: AuthorizerDecision, ttl: Duration) {
        if ttl.is_zero() {
            return;
        }

        self.entries
            .lock()
            .expect("authorizer cache should lock")
            .insert(
                key,
                CacheEntry {
                    decision,
                    expires_at: Instant::now() + ttl,
                },
            );
    }
}
