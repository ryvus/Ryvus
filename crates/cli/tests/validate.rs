use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

#[test]
fn validate_discovers_and_validates_api_actions() {
    let project = TestProject::new("validate");
    project.add_action(
        "hello.py",
        r#"
@api_action(method="GET", path="/hello")
def hello():
    return {"ok": True}
"#,
    );
    project.add_node_action(
        "hello.js",
        r#"
export default apiAction({
  method: "GET",
  path: "/node/hello",
  handler() {
    return { ok: true };
  },
});
"#,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_ryvus"))
        .arg("validate")
        .current_dir(&project.root)
        .output()
        .expect("validate command should run");

    assert!(
        output.status.success(),
        "validate failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("Discovered 2 action(s)"));
    assert!(stdout.contains("Validated 2 action(s)"));
    assert!(stdout.contains("GET"));
    assert!(stdout.contains("/hello"));
    assert!(stdout.contains("/node/hello"));
}

struct TestProject {
    root: PathBuf,
}

impl TestProject {
    fn new(name: &str) -> Self {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("ryvus-cli-{name}-{id}"));

        fs::create_dir_all(root.join("src")).expect("test project should be created");

        Self { root }
    }

    fn add_action(&self, file: &str, body: &str) {
        let content = format!(
            r#"from ryvus import api_action
{body}
"#,
        );

        fs::write(self.root.join("src").join(file), content).expect("action should be written");
    }

    fn add_node_action(&self, file: &str, body: &str) {
        let sdk_path = workspace_root().join("sdk/node/dist/index.js");
        let content = format!(
            r#"import {{ apiAction }} from {sdk_path:?};
{body}
"#,
            sdk_path = format!("file://{}", sdk_path.display()),
            body = body,
        );

        fs::write(self.root.join("src").join(file), content)
            .expect("node action should be written");
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root should resolve")
}
