use ryvus_execution::{action_revision, StateStoreResult};
use ryvus_protocol::{ActionDefinition, ActionRetryPolicy};
use serde::Serialize;

#[derive(Serialize)]
pub struct CatalogDocument<'a> {
    actions: Vec<CatalogAction<'a>>,
}

#[derive(Serialize)]
struct CatalogAction<'a> {
    #[serde(flatten)]
    definition: &'a ActionDefinition,
    action_revision: String,
    effective_policy: EffectivePolicy<'a>,
}

#[derive(Serialize)]
struct EffectivePolicy<'a> {
    timeout: &'a str,
    retry: EffectiveRetryPolicy<'a>,
}

#[derive(Serialize)]
struct EffectiveRetryPolicy<'a> {
    max_attempts: u32,
    initial_delay: &'a str,
    backoff: f64,
}

pub fn catalog_document<'a>(
    actions: impl IntoIterator<Item = &'a ActionDefinition>,
) -> StateStoreResult<CatalogDocument<'a>> {
    let actions = actions
        .into_iter()
        .map(|definition| {
            let ActionRetryPolicy {
                max_attempts,
                initial_delay,
                backoff,
            } = &definition.policy.retry;
            Ok(CatalogAction {
                definition,
                action_revision: action_revision(definition)?,
                effective_policy: EffectivePolicy {
                    timeout: &definition.policy.timeout,
                    retry: EffectiveRetryPolicy {
                        max_attempts: *max_attempts,
                        initial_delay,
                        backoff: *backoff,
                    },
                },
            })
        })
        .collect::<StateStoreResult<Vec<_>>>()?;
    Ok(CatalogDocument { actions })
}
