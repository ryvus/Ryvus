use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::error::{CliError, Result};
use ryvus_protocol::{ActionDefinition, ActionKind, ActionManifest, ApiAction, RuntimeKind};

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
    dir: &Path,
    actions: &mut Vec<ActionDefinition>,
) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            discover_python_actions(project_root, &path, actions)?;
            continue;
        }

        if path.extension().and_then(|ext| ext.to_str()) != Some("py") {
            continue;
        }

        let content = fs::read_to_string(&path)?;
        let discovered = discover_python_file(project_root, &path, &content)?;

        actions.extend(discovered);
    }

    Ok(())
}

fn discover_python_file(
    project_root: &Path,
    path: &Path,
    content: &str,
) -> Result<Vec<ActionDefinition>> {
    let mut actions = Vec::new();
    let lines: Vec<&str> = content.lines().collect();

    for index in 0..lines.len() {
        let line = lines[index].trim();

        if !line.starts_with("@api_action") {
            continue;
        }

        let decorator = collect_decorator(&lines, index);
        let method = extract_string_arg(&decorator, "method").unwrap_or_else(|| "GET".to_string());
        let route_path = extract_string_arg(&decorator, "path").ok_or_else(|| {
            CliError::Validation(format!("Missing path in @api_action in {:?}", path))
        })?;

        let entrypoint = find_next_function_name(&lines, index).ok_or_else(|| {
            CliError::Validation(format!("Missing function after @api_action in {:?}", path))
        })?;

        let source = path
            .strip_prefix(project_root)
            .unwrap_or(path)
            .to_path_buf();

        actions.push(ActionDefinition {
            runtime: RuntimeKind::Python,
            source,
            entrypoint,
            kind: ActionKind::Api(ApiAction {
                method,
                path: route_path,
            }),
        });
    }

    Ok(actions)
}

fn collect_decorator(lines: &[&str], start: usize) -> String {
    let mut value = String::new();

    for line in &lines[start..] {
        value.push_str(line.trim());

        if line.contains(')')
            || !line.trim_start().starts_with("@api_action(") && value.contains(')')
        {
            break;
        }
    }

    value
}

fn extract_string_arg(input: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=");
    let start = input.find(&needle)? + needle.len();

    let rest = input[start..].trim_start();
    let quote = rest.chars().next()?;

    if quote != '"' && quote != '\'' {
        return None;
    }

    let rest = &rest[1..];
    let end = rest.find(quote)?;

    Some(rest[..end].to_string())
}

fn find_next_function_name(lines: &[&str], start: usize) -> Option<String> {
    for line in &lines[start + 1..] {
        let line = line.trim();

        if !line.starts_with("def ") {
            continue;
        }

        let name_start = "def ".len();
        let rest = &line[name_start..];
        let name_end = rest.find('(')?;

        return Some(rest[..name_end].to_string());
    }

    None
}
