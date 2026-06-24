use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use crate::error::{CliError, Result};
use ryvus_protocol::{ActionDefinition, ActionManifest};

pub fn run() -> Result<()> {
    let manifest = discover_project(".")?;

    let output_dir = PathBuf::from(".ryvus");
    fs::create_dir_all(&output_dir)?;

    let output_path = output_dir.join("action-manifest.json");

    let json =
        serde_json::to_string_pretty(&manifest).map_err(|_| CliError::SerializationFailed)?;

    fs::write(&output_path, json).map_err(CliError::Io)?;

    println!(
        "Discovered {} action(s) -> {}",
        manifest.actions.len(),
        output_path.display()
    );

    Ok(())
}
pub fn discover_project(project_root: impl AsRef<Path>) -> Result<ActionManifest> {
    let project_root = project_root.as_ref();
    let src_dir = project_root.join("src");

    let mut actions = Vec::new();

    discover_python_actions(project_root, &src_dir, &mut actions)?;

    Ok(ActionManifest { actions })
}

fn discover_python_actions(
    project_root: &Path,
    _dir: &Path,
    actions: &mut Vec<ActionDefinition>,
) -> Result<()> {
    let output = Command::new("python")
        .args([
            "-m",
            "ryvus.discover",
            "--project-root",
            project_root.to_str().ok_or_else(|| {
                CliError::Validation("project root is not valid UTF-8".to_string())
            })?,
        ])
        .output()
        .map_err(CliError::Io)?;

    if !output.status.success() {
        return Err(CliError::Validation(format!(
            "Python discovery failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    let manifest: ActionManifest = serde_json::from_slice(&output.stdout).map_err(|error| {
        CliError::Validation(format!("failed to parse Python discovery output: {error}"))
    })?;

    actions.extend(manifest.actions);

    Ok(())
}
