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

    ryvus_scheduler::validate_schedule_actions(action_catalog.all())
        .map_err(|err| CliError::Validation(err.to_string()))?;

    project::print_validation(&validation);

    Ok(())
}
