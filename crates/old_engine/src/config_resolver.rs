use ryvus_core::prelude::ExecutionContext;
use serde_json::Value;

use crate::utils::jsonpath_resolver::{build_jsonpath_context, resolve_jsonpaths};

pub trait ConfigResolver: Send + Sync {
    fn resolve(&self, config: &mut Value, ctx: &ExecutionContext);
}

pub struct JsonPathConfigResolver;

impl ConfigResolver for JsonPathConfigResolver {
    fn resolve(&self, config: &mut Value, ctx: &ExecutionContext) {
        let ctx_json = build_jsonpath_context(ctx);
        resolve_jsonpaths(config, &ctx_json);
    }
}
