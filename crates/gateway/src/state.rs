use std::sync::Arc;

use ryvus_action_catalog::{file_catalog::FileActionCatalog, service::ActionService};
use ryvus_execution_service::ExecutionService;
use ryvus_executor::{Executor, RuntimeResolver};
use ryvus_persistence::ExecutionPersistence;

use crate::registry::route_registry::RouteRegistry;

pub type GatewayExecutionService =
    ExecutionService<Arc<dyn RuntimeResolver>, Arc<dyn Executor>, Arc<dyn ExecutionPersistence>>;
#[derive(Clone)]
pub struct AppState {
    pub route_registry: Arc<RouteRegistry>,
    pub action_service: Arc<ActionService<FileActionCatalog>>,
    pub execution_service: Arc<GatewayExecutionService>,
}
