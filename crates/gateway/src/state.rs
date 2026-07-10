use std::sync::Arc;

use ryvus_control::ControlService;
use ryvus_execution::{ExecutionPersistence, ExecutionService, Executor, RuntimeResolver};

use crate::authorization::AuthorizationService;

pub type GatewayExecutionService =
    ExecutionService<Arc<dyn RuntimeResolver>, Arc<dyn Executor>, Arc<dyn ExecutionPersistence>>;
#[derive(Clone)]
pub struct AppState {
    pub control_service: Arc<ControlService>,
    pub execution_service: Arc<GatewayExecutionService>,
    pub authorization_service: Arc<AuthorizationService>,
}
