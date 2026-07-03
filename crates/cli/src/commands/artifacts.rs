use std::{
    fs,
    path::{Path, PathBuf},
};

use ryvus_docs::openapi::build_public_openapi_json_from_actions;
use ryvus_protocol::{ActionKind, ActionManifest, RuntimeKind};
use serde_json::json;

use crate::error::{CliError, Result};

pub fn write_portal_artifacts(project_root: &Path, manifest: &ActionManifest) -> Result<()> {
    let output_dir = project_root.join(".ryvus");
    let docs_dir = output_dir.join("docs");
    let pages_dir = docs_dir.join("pages");

    fs::create_dir_all(&pages_dir).map_err(CliError::Io)?;

    write_json(output_dir.join("catalog.json"), manifest)?;
    write_json(
        output_dir.join("openapi.json"),
        &build_public_openapi_json_from_actions(&manifest.actions),
    )?;
    write_json(output_dir.join("schedules.json"), &schedules_json(manifest))?;
    write_docs(project_root, docs_dir.join("registry.json"), &pages_dir)?;

    Ok(())
}

fn schedules_json(manifest: &ActionManifest) -> serde_json::Value {
    let schedules = manifest
        .actions
        .iter()
        .filter_map(|action| {
            let ActionKind::Schedule(schedule) = &action.kind else {
                return None;
            };

            let name = action
                .name
                .clone()
                .unwrap_or_else(|| action.entrypoint.clone());
            let handler = format!("{}::{}", action.source.display(), action.entrypoint);

            Some(json!({
                "name": name,
                "expression": schedule.expression,
                "runtime": runtime_label(&action.runtime),
                "handler": handler,
                "action": handler,
                "enabled": true,
            }))
        })
        .collect::<Vec<_>>();

    json!({ "schedules": schedules })
}

fn runtime_label(runtime: &RuntimeKind) -> &'static str {
    match runtime {
        RuntimeKind::Python => "python",
        RuntimeKind::Node => "node",
        RuntimeKind::Rust => "rust",
    }
}

fn write_docs(project_root: &Path, registry_path: PathBuf, pages_dir: &Path) -> Result<()> {
    let mut pages = Vec::new();

    for doc_path in project_docs(project_root)? {
        let relative = doc_path
            .strip_prefix(project_root)
            .expect("project doc should be under project root");
        let page_path = format!("/{}", relative.display().to_string().replace('\\', "/"));
        let file_name = page_file_name(page_path.trim_start_matches('/'));
        let target = pages_dir.join(file_name);
        let content = fs::read_to_string(&doc_path).map_err(CliError::Io)?;
        fs::write(&target, content).map_err(CliError::Io)?;

        pages.push(json!({
            "id": page_path.trim_start_matches('/'),
            "title": title_from_path(relative),
            "path": page_path,
            "source": "Project",
            "content_type": "Markdown",
            "content_path": format!(
                "/.ryvus/docs/pages/{}",
                target
                    .file_name()
                    .expect("page file should have name")
                    .to_string_lossy()
            ),
        }));
    }

    write_json(
        registry_path,
        &json!({
            "nav": pages
                .iter()
                .map(|page| {
                    json!({
                        "id": page["id"],
                        "title": page["title"],
                        "path": page["path"],
                        "children": [],
                    })
                })
                .collect::<Vec<_>>(),
            "pages": pages,
        }),
    )
}

fn project_docs(project_root: &Path) -> Result<Vec<PathBuf>> {
    let mut docs = Vec::new();
    let readme = project_root.join("README.md");
    if readme.is_file() {
        docs.push(readme);
    }

    collect_markdown(&project_root.join("docs"), &mut docs)?;
    docs.sort();
    Ok(docs)
}

fn collect_markdown(dir: &Path, docs: &mut Vec<PathBuf>) -> Result<()> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(());
    };

    for entry in entries {
        let path = entry.map_err(CliError::Io)?.path();
        if path
            .components()
            .any(|component| component.as_os_str() == ".ryvus")
        {
            continue;
        }
        if path.is_dir() {
            collect_markdown(&path, docs)?;
        } else if path.extension().and_then(|value| value.to_str()) == Some("md") {
            docs.push(path);
        }
    }

    Ok(())
}

fn title_from_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("Doc")
        .replace(['-', '_'], " ")
}

fn page_file_name(path: &str) -> String {
    let mut output = String::new();

    for character in path.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
            output.push(character);
        } else {
            let mut buffer = [0; 4];
            for byte in character.encode_utf8(&mut buffer).bytes() {
                output.push_str(&format!("%{byte:02X}"));
            }
        }
    }

    output
}

fn write_json(path: PathBuf, value: &impl serde::Serialize) -> Result<()> {
    let json = serde_json::to_string_pretty(value).map_err(|_| CliError::SerializationFailed)?;
    fs::write(path, json).map_err(CliError::Io)
}
