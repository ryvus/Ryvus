use crate::{
    commands::{discover, project},
    error::{CliError, Result},
};
use ryvus_action_catalog::FileActionCatalog;
use ryvus_control::{ControlService, LocalControlConfig};
use std::sync::Arc;

pub fn run(run_schedules: bool) -> Result<()> {
    project::configure_python_path();

    discover::run()?;

    let config = project::gateway_config()?;
    let control_service = Arc::new(
        ControlService::load_local(LocalControlConfig {
            project_root: config.project_root.clone(),
            manifest_path: config.manifest_path(),
        })
        .map_err(|err| CliError::Validation(err.to_string()))?,
    );
    let control_addr = project::control_addr();
    let validation = ryvus_gateway::server::validate_config(&config)
        .map_err(|err| CliError::Validation(err.to_string()))?;
    let action_catalog = FileActionCatalog::load(config.manifest_path())
        .map_err(|err| CliError::Validation(err.to_string()))?;
    let scheduler = ryvus_scheduler::Scheduler::from_actions(action_catalog.all())
        .map_err(|err| CliError::Validation(err.to_string()))?;
    let execution_service =
        ryvus_gateway::server::build_execution_service(config.project_root.clone());
    let scheduler_service = Arc::new(ryvus_scheduler::http::SchedulerService::new(
        action_catalog.all().cloned().collect(),
        Arc::clone(&execution_service),
    ));
    let scheduler_routes = ryvus_scheduler::http::scheduler_routes(scheduler_service);
    let flow_store = Arc::new(ryvus_flow::InMemoryFlowStateStore::default());
    let flow_service = Arc::new(
        ryvus_flow::FlowService::new(
            control_service
                .typed_flow_spec()
                .map_err(|err| CliError::Validation(err.to_string()))?,
            action_catalog.all().cloned().collect(),
            flow_store,
            Arc::clone(&execution_service),
        )
        .map_err(|err| CliError::Validation(err.to_string()))?,
    );
    let control_routes = scheduler_routes.merge(ryvus_flow::http::flow_routes(flow_service));

    println!("Validated {} action(s)", validation.action_count);
    println!("Gateway: http://{}", config.addr);
    println!("Control: http://{}", control_addr);
    println!("Portal:  http://{}", control_addr);

    let runtime = tokio::runtime::Runtime::new().map_err(CliError::Io)?;

    if run_schedules {
        runtime.block_on(async move {
            let control_service = Arc::clone(&control_service);
            tokio::select! {
                result = ryvus_gateway::server::serve_with_execution_service(
                    config,
                    execution_service.clone(),
                ) => result.map_err(|err| CliError::Gateway(err.to_string())),
                result = ryvus_control::http::serve_with_routes(control_addr, control_service, control_routes) => {
                    result.map_err(|err| CliError::Gateway(err.to_string()))
                },
                result = scheduler.run(execution_service) => {
                    result.map_err(|err| CliError::Validation(err.to_string()))
                }
            }
        })?;
    } else {
        runtime.block_on(async move {
            tokio::select! {
                result = ryvus_gateway::server::serve_with_execution_service(config, execution_service) => {
                    result.map_err(|err| CliError::Gateway(err.to_string()))
                },
                result = ryvus_control::http::serve_with_routes(control_addr, control_service, control_routes) => {
                    result.map_err(|err| CliError::Gateway(err.to_string()))
                }
            }
        })?;
    }

    Ok(())
}
