use crate::{
    commands::{discover, project},
    error::{CliError, Result},
};

pub fn run() -> Result<()> {
    project::configure_python_path();
    discover::run()?;

    let config = project::gateway_config()?;
    let validation = ryvus_gateway::server::validate_config(&config)
        .map_err(|err| CliError::Validation(err.to_string()))?;

    project::print_validation(&validation);

    Ok(())
}
