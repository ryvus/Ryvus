use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::{
    cli::Language,
    error::{CliError, Result},
};

pub fn run(project_name: String, language: Language) -> Result<()> {
    let project_dir = PathBuf::from(&project_name);

    if project_dir.exists() {
        return Err(CliError::ProjectAlreadyExists(
            project_dir.display().to_string(),
        ));
    }

    fs::create_dir_all(&project_dir)?;

    let context = TemplateContext {
        project_name: project_name.clone(),
        language,
    };

    render_project_templates(&project_dir, &context)?;
    render_language_templates(&project_dir, &context)?;

    println!("Created Ryvus project `{}`", project_name);
    println!();
    println!("Next steps:");
    println!("  cd {}", project_name);
    println!("  ryvus start");

    Ok(())
}

struct TemplateContext {
    project_name: String,
    language: Language,
}

struct TemplateFile {
    target_path: &'static str,
    content: &'static str,
}

fn render_project_templates(project_dir: &Path, context: &TemplateContext) -> Result<()> {
    let files = [
        TemplateFile {
            target_path: "project.json",
            content: include_str!("../../templates/new-project/project.json"),
        },
        TemplateFile {
            target_path: ".gitignore",
            content: include_str!("../../templates/new-project/.gitignore"),
        },
        TemplateFile {
            target_path: ".env",
            content: include_str!("../../templates/new-project/env.template"),
        },
        TemplateFile {
            target_path: "README.md",
            content: include_str!("../../templates/new-project/README.md"),
        },
    ];

    render_files(project_dir, context, &files)
}

fn render_language_templates(project_dir: &Path, context: &TemplateContext) -> Result<()> {
    match context.language {
        Language::Python => render_files(project_dir, context, PYTHON_TEMPLATE_FILES),
        Language::Node => render_files(project_dir, context, NODE_TEMPLATE_FILES),
        Language::Rust => render_files(project_dir, context, RUST_TEMPLATE_FILES),
    }
}

fn render_files(
    project_dir: &Path,
    context: &TemplateContext,
    files: &[TemplateFile],
) -> Result<()> {
    for file in files {
        let target_path = project_dir.join(file.target_path);

        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let rendered = render_template(file.content, context);

        fs::write(target_path, rendered)?;
    }

    Ok(())
}

fn render_template(template: &str, context: &TemplateContext) -> String {
    template
        .replace("{{ project_name }}", &context.project_name)
        .replace("{{ language }}", &context.language.to_string())
}

const PYTHON_TEMPLATE_FILES: &[TemplateFile] = &[
    TemplateFile {
        target_path: "requirements.txt",
        // content: include_str!("../../../templates/new-project/languages/python/requirements.txt"),
        content: include_str!("../../templates/new-project/languages/python/requirements.txt"),
    },
    TemplateFile {
        target_path: "src/modules/example/api/hello.py",
        content: include_str!(
            "../../templates/new-project/languages/python/src/modules/example/api/hello.py"
        ),
    },
];

const NODE_TEMPLATE_FILES: &[TemplateFile] = &[
    TemplateFile {
        target_path: "package.json",
        content: include_str!("../../templates/new-project/languages/node/package.json"),
    },
    TemplateFile {
        target_path: "tsconfig.json",
        content: include_str!("../../templates/new-project/languages/node/tsconfig.json"),
    },
    TemplateFile {
        target_path: "src/modules/example/api/hello.ts",
        content: include_str!(
            "../../templates/new-project/languages/node/src/modules/example/api/hello.ts"
        ),
    },
];

const RUST_TEMPLATE_FILES: &[TemplateFile] = &[
    TemplateFile {
        target_path: "Cargo.toml",
        content: include_str!("../../templates/new-project/languages/rust/Cargo.toml"),
    },
    TemplateFile {
        target_path: "src/modules/example/api/hello.rs",
        content: include_str!(
            "../../templates/new-project/languages/rust/src/modules/example/api/hello.rs"
        ),
    },
];

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn project_template_creates_ignored_memory_dotenv() {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("ryvus-new-dotenv-{id}"));
        fs::create_dir_all(&root).unwrap();
        let context = TemplateContext {
            project_name: "test-project".into(),
            language: Language::Python,
        };

        render_project_templates(&root, &context).unwrap();

        assert_eq!(
            fs::read_to_string(root.join(".env")).unwrap(),
            "RYVUS_EXECUTION_STORE=memory\n"
        );
        assert!(fs::read_to_string(root.join(".gitignore"))
            .unwrap()
            .lines()
            .any(|line| line == ".env"));
        fs::remove_dir_all(root).unwrap();
    }
}
