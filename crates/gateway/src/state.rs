use std::sync::Arc;

use ryvus_control::ControlService;
use ryvus_execution_service::ExecutionService;
use ryvus_executor::{Executor, RuntimeResolver};
use ryvus_persistence::ExecutionPersistence;

pub type GatewayExecutionService =
    ExecutionService<Arc<dyn RuntimeResolver>, Arc<dyn Executor>, Arc<dyn ExecutionPersistence>>;
#[derive(Clone)]
pub struct AppState {
    pub control_service: Arc<ControlService>,
    pub execution_service: Arc<GatewayExecutionService>,
}
