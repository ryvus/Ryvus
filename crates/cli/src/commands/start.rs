use crate::{
    commands::{discover, project},
    error::{CliError, Result},
};
use ryvus_action_catalog::FileActionCatalog;

pub fn run() -> Result<()> {
    project::configure_python_path();

    discover::run()?;

    let config = project::gateway_config()?;
    let validation = ryvus_gateway::server::validate_config(&config)
        .map_err(|err| CliError::Validation(err.to_string()))?;
    let action_catalog = FileActionCatalog::load(config.manifest_path())
        .map_err(|err| CliError::Validation(err.to_string()))?;
    let scheduler = ryvus_scheduler::Scheduler::from_actions(action_catalog.all())
        .map_err(|err| CliError::Validation(err.to_string()))?;
    let execution_service =
        ryvus_gateway::server::build_execution_service(config.project_root.clone());

    project::print_validation(&validation);
    println!("Schedules: {}", scheduler.action_count());
    println!("Server: http://{}", config.addr);
    println!("Docs:   http://{}/docs", config.addr);

    let runtime = tokio::runtime::Runtime::new().map_err(CliError::Io)?;

    runtime.block_on(async move {
        tokio::select! {
            result = ryvus_gateway::server::serve_with_execution_service(
                config,
                execution_service.clone(),
            ) => result.map_err(|err| CliError::Gateway(err.to_string())),
            result = scheduler.run(execution_service) => {
                result.map_err(|err| CliError::Validation(err.to_string()))
            }
        }
    })?;

    Ok(())
}
