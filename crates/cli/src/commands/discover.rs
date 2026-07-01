use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use crate::{
    commands::project,
    error::{CliError, Result},
};
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
    discover_node_actions(project_root, &src_dir, &mut actions)?;

    Ok(ActionManifest { actions })
}

fn discover_python_actions(
    project_root: &Path,
    dir: &Path,
    actions: &mut Vec<ActionDefinition>,
) -> Result<()> {
    if !has_python_sources(dir) {
        return Ok(());
    }

    let output = Command::new("python")
        .args([
            "-m",
            "ryvus.discover",
            "--project-root",
            project_root.to_str().ok_or_else(|| {
                CliError::Validation("project root is not valid UTF-8".to_string())
            })?,
        ])
        .env("PYTHONPATH", project::python_path()?)
        .output()
        .map_err(|error| {
            CliError::Validation(format!(
                "Python discovery failed: could not start python: {error}"
            ))
        })?;

    if !output.status.success() {
        return Err(CliError::Validation(format!(
            "Python discovery failed:\n{}",
            command_output(&output)
        )));
    }

    let manifest: ActionManifest = serde_json::from_slice(&output.stdout).map_err(|error| {
        CliError::Validation(format!("failed to parse Python discovery output: {error}"))
    })?;

    actions.extend(manifest.actions);

    Ok(())
}

fn discover_node_actions(
    project_root: &Path,
    dir: &Path,
    actions: &mut Vec<ActionDefinition>,
) -> Result<()> {
    if has_ts_sources(dir) {
        compile_typescript(project_root)?;
        discover_node_actions_from(project_root, &project_root.join("dist"), actions)?;
    }

    discover_node_actions_from(project_root, dir, actions)
}

fn discover_node_actions_from(
    project_root: &Path,
    dir: &Path,
    actions: &mut Vec<ActionDefinition>,
) -> Result<()> {
    if !has_node_sources(dir) {
        return Ok(());
    }

    let discover_script = node_discover_script()?;
    if !discover_script.is_file() {
        return Err(CliError::Validation(format!(
            "Node discovery failed: discovery script not found at {}. Build the Node SDK first.",
            discover_script.display()
        )));
    }

    let output = Command::new("node")
        .args([
            discover_script.to_str().ok_or_else(|| {
                CliError::Validation("Node discovery script path is not valid UTF-8".to_string())
            })?,
            "--project-root",
            project_root.to_str().ok_or_else(|| {
                CliError::Validation("project root is not valid UTF-8".to_string())
            })?,
            "--source-root",
            dir.to_str().ok_or_else(|| {
                CliError::Validation("Node source root is not valid UTF-8".to_string())
            })?,
        ])
        .output()
        .map_err(|error| {
            CliError::Validation(format!(
                "Node discovery failed: could not start node: {error}"
            ))
        })?;

    if !output.status.success() {
        return Err(CliError::Validation(format!(
            "Node discovery failed:\n{}",
            command_output(&output)
        )));
    }

    let manifest: ActionManifest = serde_json::from_slice(&output.stdout).map_err(|error| {
        CliError::Validation(format!("failed to parse Node discovery output: {error}"))
    })?;

    actions.extend(manifest.actions);

    Ok(())
}

fn compile_typescript(project_root: &Path) -> Result<()> {
    let tsconfig = project_root.join("tsconfig.json");

    if !tsconfig.is_file() {
        return Err(CliError::Validation(
            "TypeScript actions require tsconfig.json".to_string(),
        ));
    }

    let tsc = project::ryvus_root()?
        .join("sdk")
        .join("node")
        .join("node_modules")
        .join(".bin")
        .join("tsc");

    if !tsc.is_file() {
        return Err(CliError::Validation(format!(
            "TypeScript build failed: tsc not found at {}. Run npm install in sdk/node.",
            tsc.display()
        )));
    }

    let output = Command::new(tsc)
        .args([
            "-p",
            tsconfig.to_str().ok_or_else(|| {
                CliError::Validation("tsconfig path is not valid UTF-8".to_string())
            })?,
        ])
        .output()
        .map_err(|error| {
            CliError::Validation(format!(
                "TypeScript build failed: could not start tsc: {error}"
            ))
        })?;

    if !output.status.success() {
        return Err(CliError::Validation(format!(
            "TypeScript build failed:\n{}",
            command_output(&output)
        )));
    }

    Ok(())
}

fn node_discover_script() -> Result<PathBuf> {
    Ok(project::ryvus_root()?
        .join("sdk")
        .join("node")
        .join("dist")
        .join("discover.js"))
}

fn has_node_sources(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };

    for entry in entries.flatten() {
        let path = entry.path();

        if path.is_dir() && has_node_sources(&path) {
            return true;
        }

        if matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("js" | "mjs")
        ) {
            return true;
        }
    }

    false
}

fn has_python_sources(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };

    for entry in entries.flatten() {
        let path = entry.path();

        if path.is_dir() && has_python_sources(&path) {
            return true;
        }

        if matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("py")
        ) {
            return true;
        }
    }

    false
}

fn has_ts_sources(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };

    for entry in entries.flatten() {
        let path = entry.path();

        if path.is_dir() && has_ts_sources(&path) {
            return true;
        }

        if matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("ts")
        ) {
            return true;
        }
    }

    false
}

fn command_output(output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    match (stdout.trim(), stderr.trim()) {
        ("", "") => format!("process exited with status {}", output.status),
        ("", stderr) => stderr.to_string(),
        (stdout, "") => stdout.to_string(),
        (stdout, stderr) => format!("{stdout}\n{stderr}"),
    }
}
