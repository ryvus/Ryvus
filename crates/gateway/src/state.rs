use std::sync::Arc;

use ryvus_action_catalog::{file_catalog::FileActionCatalog, service::ActionService};
use ryvus_execution_service::ExecutionService;
use ryvus_executor::{LocalProcessExecutor, RecordingExecutor};
use ryvus_persistence::FilesystemExecutionPersistence;

use crate::registry::route_registry::RouteRegistry;

pub type GatewayExecutionService =
    ExecutionService<LocalProcessExecutor, FilesystemExecutionPersistence>;

#[derive(Clone)]
pub struct AppState {
    pub route_registry: Arc<RouteRegistry>,
    pub action_service: Arc<ActionService<FileActionCatalog>>,
    pub execution_service: Arc<GatewayExecutionService>,
}
