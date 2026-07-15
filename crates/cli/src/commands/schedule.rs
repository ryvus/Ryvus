use crate::{
    commands::{discover, project},
    error::{CliError, Result},
};
use ryvus_action_catalog::FileActionCatalog;
use ryvus_protocol::InvocationStatus;

pub fn list() -> Result<()> {
    project::configure_python_path();
    discover::run()?;

    let config = project::gateway_config()?;
    let action_catalog = FileActionCatalog::load(config.manifest_path())
        .map_err(|err| CliError::Validation(err.to_string()))?;
    let schedules = ryvus_scheduler::schedule_infos(action_catalog.all())
        .map_err(|err| CliError::Validation(err.to_string()))?;

    println!("{:<24} {:<12} ACTION", "NAME", "EXPRESSION");
    for schedule in schedules {
        println!(
            "{:<24} {:<12} {}",
            schedule.name, schedule.expression, schedule.action_key
        );
    }

    Ok(())
}

pub fn run(selector: String) -> Result<()> {
    project::configure_python_path();
    discover::run()?;

    let config = project::gateway_config()?;
    ryvus_gateway::server::validate_config(&config)
        .map_err(|err| CliError::Validation(err.to_string()))?;
    let action_catalog = FileActionCatalog::load(config.manifest_path())
        .map_err(|err| CliError::Validation(err.to_string()))?;
    ryvus_scheduler::validate_schedule_actions(action_catalog.all())
        .map_err(|err| CliError::Validation(err.to_string()))?;

    let execution_service = project::build_execution_service(&config)?;
    let result =
        ryvus_scheduler::run_schedule_once(action_catalog.all(), &selector, execution_service)
            .map_err(|err| CliError::Validation(err.to_string()))?;

    println!("execution_id: {}", result.execution_id);
    println!("attempt_id: {}", result.attempt_id);
    println!("attempt_number: {}", result.attempt_number);
    println!("status: {}", status_label(&result.status));
    println!(
        "output: {}",
        result
            .output
            .map(|output| output.to_string())
            .unwrap_or_else(|| "null".to_string())
    );

    Ok(())
}

fn status_label(status: &InvocationStatus) -> &'static str {
    match status {
        InvocationStatus::Success => "success",
        InvocationStatus::Failed => "failed",
    }
}
