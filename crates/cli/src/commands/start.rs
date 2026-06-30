use crate::{
    commands::discover,
    error::{CliError, Result},
};

pub fn run() -> Result<()> {
    let ryvus_root = std::env::var("RYVUS_ROOT").ok();

    if let Some(ryvus_root) = ryvus_root {
        let sdk_path = std::path::PathBuf::from(ryvus_root).join("sdk/python");

        let existing = std::env::var("PYTHONPATH").unwrap_or_default();

        let pythonpath = if existing.is_empty() {
            sdk_path.to_string_lossy().to_string()
        } else {
            format!("{}:{}", sdk_path.display(), existing)
        };

        std::env::set_var("PYTHONPATH", pythonpath);
    }

    discover::run()?;

    let project_root = std::env::current_dir().map_err(CliError::Io)?;

    let runtime = tokio::runtime::Runtime::new().map_err(CliError::Io)?;

    runtime
        .block_on(async {
            ryvus_gateway::server::serve(ryvus_gateway::server::GatewayServerConfig {
                project_root,
                manifest_path: ".ryvus/action-manifest.json".into(),
                addr: ([127, 0, 0, 1], 8080).into(),
            })
            .await
        })
        .map_err(|err| CliError::Gateway(err.to_string()))?;

    Ok(())
}
