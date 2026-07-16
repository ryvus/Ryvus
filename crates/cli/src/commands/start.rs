use crate::{
    commands::{discover, project},
    error::{CliError, Result},
};
use ryvus_action_catalog::FileActionCatalog;
use ryvus_control::{ControlService, LocalControlConfig};
use std::sync::Arc;
use std::time::Duration;

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
    let composition = project::build_local_composition(&config)?;
    let execution_service = Arc::clone(&composition.execution_service);
    let scheduler_service = Arc::new(ryvus_scheduler::DurableSchedulerService::new(
        Arc::clone(&composition.schedule_store),
        Arc::clone(&execution_service),
        composition.execution_scope.clone(),
        composition.actor,
        "local-scheduler",
        Duration::from_secs(30),
    ));
    scheduler_service
        .reconcile(
            &action_catalog.all().cloned().collect::<Vec<_>>(),
            std::time::SystemTime::now(),
        )
        .map_err(|err| CliError::Validation(err.to_string()))?;
    let scheduler_routes = ryvus_scheduler::http::scheduler_routes(Arc::clone(&scheduler_service));
    let execution_history_routes = ryvus_execution::execution_history_routes(
        composition.execution_store,
        composition.execution_scope,
    );
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
    let control_routes = scheduler_routes
        .merge(execution_history_routes)
        .merge(ryvus_flow::http::flow_routes(flow_service));

    println!("Validated {} action(s)", validation.action_count);
    println!("Gateway: http://{}", config.addr);
    println!("Control: http://{}", control_addr);
    println!("Portal:  http://{}", control_addr);

    let runtime = tokio::runtime::Runtime::new().map_err(CliError::Io)?;
    let shutdown_grace = Duration::from_secs(3);

    if run_schedules {
        runtime.block_on(async move {
            let control_service = Arc::clone(&control_service);
            let shutdown_service = Arc::clone(&execution_service);
            tokio::select! {
                result = ryvus_gateway::server::serve_with_execution_service(
                    config,
                    execution_service.clone(),
                ) => result.map_err(|err| CliError::Gateway(err.to_string())),
                result = ryvus_control::http::serve_with_routes(control_addr, control_service, control_routes) => {
                    result.map_err(|err| CliError::Gateway(err.to_string()))
                },
                result = scheduler_service.run() => {
                    result.map_err(|err| CliError::Validation(err.to_string()))
                },
                result = tokio::signal::ctrl_c() => {
                    result.map_err(CliError::Io)?;
                    shutdown_service
                        .shutdown(shutdown_grace)
                        .map_err(|err| CliError::Gateway(err.to_string()))
                }
            }
        })?;
    } else {
        runtime.block_on(async move {
            let shutdown_service = Arc::clone(&execution_service);
            tokio::select! {
                result = ryvus_gateway::server::serve_with_execution_service(config, execution_service.clone()) => {
                    result.map_err(|err| CliError::Gateway(err.to_string()))
                },
                result = ryvus_control::http::serve_with_routes(control_addr, control_service, control_routes) => {
                    result.map_err(|err| CliError::Gateway(err.to_string()))
                },
                result = tokio::signal::ctrl_c() => {
                    result.map_err(CliError::Io)?;
                    shutdown_service
                        .shutdown(shutdown_grace)
                        .map_err(|err| CliError::Gateway(err.to_string()))
                }
            }
        })?;
    }

    Ok(())
}
