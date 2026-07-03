use async_trait::async_trait;
use ryvus_core::{error::Error, prelude::ExecutionContext};
use serde_json::{json, Value};

use crate::utils::jsonpath_resolver::{build_jsonpath_context, resolve_jsonpaths};

/// Defines how inputs for Actions are derived from the current ExecutionContext.
#[async_trait]
pub trait Mapper: Send + Sync {
    async fn map_input(
        &self,
        mut value: Value,
        exec_ctx: &ExecutionContext,
    ) -> Result<Value, Error>;
}

/// DefaultMapper provides a simple sequential behavior:
/// - Uses the previous Action's result as input for the next Action.
/// - If no previous result exists, returns an empty object `{}`.
pub struct DefaultMapper;

#[async_trait]
impl Mapper for DefaultMapper {
    async fn map_input(
        &self,
        mut _value: Value,
        exec_ctx: &ExecutionContext,
    ) -> Result<Value, Error> {
        if let Some((_, last)) = exec_ctx.results.iter().last() {
            Ok(last.clone())
        } else {
            let initial_input = match exec_ctx.data.get("payload") {
                Some(val) => val,
                None => &json!({}),
            };

            Ok(initial_input.to_owned())
        }
    }
}

pub struct JsonMapper;

#[async_trait]
impl Mapper for JsonMapper {
    async fn map_input(
        &self,
        mut value: Value,
        exec_ctx: &ExecutionContext,
    ) -> Result<Value, Error> {
        let ctx_json = build_jsonpath_context(exec_ctx);
        resolve_jsonpaths(&mut value, &ctx_json);
        Ok(value)
    }
}
