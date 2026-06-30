use crate::error::{CliError, Result};

pub fn configure_python_path() {
    let Some(ryvus_root) = std::env::var("RYVUS_ROOT").ok() else {
        return;
    };

    let sdk_path = std::path::PathBuf::from(ryvus_root).join("sdk/python");
    let existing = std::env::var("PYTHONPATH").unwrap_or_default();

    let pythonpath = if existing.is_empty() {
        sdk_path.to_string_lossy().to_string()
    } else {
        format!("{}:{}", sdk_path.display(), existing)
    };

    std::env::set_var("PYTHONPATH", pythonpath);
}

pub fn gateway_config() -> Result<ryvus_gateway::server::GatewayServerConfig> {
    Ok(ryvus_gateway::server::GatewayServerConfig {
        project_root: std::env::current_dir().map_err(CliError::Io)?,
        manifest_path: ".ryvus/action-manifest.json".into(),
        addr: ([127, 0, 0, 1], 8080).into(),
    })
}

pub fn print_validation(validation: &ryvus_gateway::server::GatewayValidation) {
    println!("Validated {} action(s)", validation.action_count);

    if validation.routes.is_empty() {
        return;
    }

    println!("Routes:");

    for route in &validation.routes {
        println!("  {:<6} {:<32} {}", route.method, route.path, route.action);
    }
}
