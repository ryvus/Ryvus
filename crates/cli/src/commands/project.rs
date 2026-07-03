use std::path::{Path, PathBuf};

use crate::error::{CliError, Result};

pub fn configure_python_path() {
    let Ok(ryvus_root) = ryvus_root() else {
        return;
    };

    prepend_python_path(ryvus_root.join("sdk/python"));
}

pub fn python_path() -> Result<String> {
    let sdk_path = ryvus_root()?.join("sdk/python");
    let existing = std::env::var("PYTHONPATH").unwrap_or_default();

    Ok(if existing.is_empty() {
        sdk_path.display().to_string()
    } else {
        format!("{}:{}", sdk_path.display(), existing)
    })
}

pub fn ryvus_root() -> Result<PathBuf> {
    if let Some(root) = std::env::var_os("RYVUS_ROOT")
        .map(PathBuf::from)
        .and_then(valid_root)
    {
        return Ok(root);
    }

    if let Some(root) = option_env!("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .and_then(|path| path.parent().and_then(Path::parent).map(Path::to_path_buf))
        .and_then(valid_root)
    {
        return Ok(root);
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(root) = find_root_near(&exe) {
            return Ok(root);
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        if let Some(root) = find_root_near(&cwd) {
            return Ok(root);
        }
    }

    Err(CliError::Validation(
        "could not find Ryvus root; set RYVUS_ROOT".to_string(),
    ))
}

fn prepend_python_path(sdk_path: PathBuf) {
    let existing = std::env::var("PYTHONPATH").unwrap_or_default();
    let pythonpath = if existing.is_empty() {
        sdk_path.display().to_string()
    } else {
        format!("{}:{}", sdk_path.display(), existing)
    };

    std::env::set_var("PYTHONPATH", pythonpath);
}

fn find_root_near(path: &Path) -> Option<PathBuf> {
    for ancestor in path.ancestors() {
        if let Some(root) = valid_root(ancestor.to_path_buf()) {
            return Some(root);
        }

        if let Some(root) = valid_root(ancestor.join("ryvus")) {
            return Some(root);
        }
    }

    None
}

fn valid_root(path: PathBuf) -> Option<PathBuf> {
    let root = path.canonicalize().ok()?;

    root.join("sdk/python").is_dir().then_some(root)
}

pub fn gateway_config() -> Result<ryvus_gateway::server::GatewayServerConfig> {
    Ok(ryvus_gateway::server::GatewayServerConfig {
        project_root: std::env::current_dir().map_err(CliError::Io)?,
        manifest_path: ".ryvus/action-manifest.json".into(),
        addr: ([127, 0, 0, 1], 8080).into(),
    })
}

pub fn control_addr() -> std::net::SocketAddr {
    ([127, 0, 0, 1], 8079).into()
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
