use crate::{
    commands::{discover, project},
    error::{CliError, Result},
};
use ryvus_control::{ControlService, LocalControlConfig};

pub fn run() -> Result<()> {
    project::configure_python_path();
    discover::run()?;

    let config = project::gateway_config()?;
    let control_service = ControlService::load_local(LocalControlConfig {
        project_root: config.project_root.clone(),
        manifest_path: config.manifest_path(),
    })
    .map_err(|err| CliError::Validation(err.to_string()))?;
    let validation = ryvus_gateway::server::validate_config(&config)
        .map_err(|err| CliError::Validation(err.to_string()))?;

    control_service
        .schedule_infos()
        .map_err(|err| CliError::Validation(err.to_string()))?;

    project::print_validation(&validation);

    Ok(())
}
