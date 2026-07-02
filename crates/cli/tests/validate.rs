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

#[test]
fn validate_discovers_node_only_projects() {
    let project = TestProject::new("validate-node-only");
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

    assert!(stdout.contains("Discovered 1 action(s)"));
    assert!(stdout.contains("/node/hello"));

    let manifest: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(project.root.join(".ryvus/action-manifest.json"))
            .expect("manifest should be written"),
    )
    .expect("manifest should parse");

    assert_eq!(manifest["actions"][0]["entrypoint"], "default");
    assert_eq!(manifest["actions"][0]["name"], "hello");
}

#[test]
fn validate_reports_missing_typescript_config() {
    let project = TestProject::new("validate-ts-no-config");
    project.add_ts_action(
        "hello.ts",
        r#"
export default apiAction({
  method: "GET",
  path: "/ts/hello",
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

    assert!(!output.status.success());

    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(stderr.contains("TypeScript actions require tsconfig.json"));
}

#[test]
fn validate_discovers_scheduled_actions() {
    let project = TestProject::new("validate-schedule");
    project.add_schedule_action(
        "sync.py",
        r#"
@scheduled_action(every="10s")
def sync_inventory(context):
    return {"ok": True}
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

    assert!(stdout.contains("Discovered 1 action(s)"));
    assert!(stdout.contains("Validated 1 action(s)"));
}

#[test]
fn validate_rejects_invalid_scheduled_actions() {
    let project = TestProject::new("validate-bad-schedule");
    project.add_schedule_action(
        "sync.py",
        r#"
@scheduled_action(every="daily")
def sync_inventory(context):
    return {"ok": True}
"#,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_ryvus"))
        .arg("validate")
        .current_dir(&project.root)
        .output()
        .expect("validate command should run");

    assert!(!output.status.success());

    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(stderr.contains("invalid schedule expression"));
}

#[test]
fn schedule_list_prints_discovered_schedules() {
    let project = TestProject::new("schedule-list");
    project.add_schedule_action(
        "sync.py",
        r#"
@scheduled_action(every="10s")
def sync_inventory(context):
    return {"ok": True}
"#,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_ryvus"))
        .args(["schedule", "list"])
        .current_dir(&project.root)
        .output()
        .expect("schedule list should run");

    assert!(
        output.status.success(),
        "schedule list failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("sync_inventory"));
    assert!(stdout.contains("every 10s"));
    assert!(stdout.contains("src/sync.py::sync_inventory"));
}

#[test]
fn schedule_run_executes_one_schedule() {
    let project = TestProject::new("schedule-run");
    project.add_schedule_action(
        "sync.py",
        r#"
@scheduled_action(every="10s")
def sync_inventory(event):
    print("sync log")
    return {"expression": event.expression}
"#,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_ryvus"))
        .args(["schedule", "run", "sync_inventory"])
        .current_dir(&project.root)
        .output()
        .expect("schedule run should run");

    assert!(
        output.status.success(),
        "schedule run failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("status: success"));
    assert!(
        stdout.contains("\"expression\":\"every 10s\"")
            || stdout.contains("\"expression\": \"every 10s\"")
    );
}

#[test]
fn schedule_run_reports_unknown_selector() {
    let project = TestProject::new("schedule-run-missing");
    project.add_schedule_action(
        "sync.py",
        r#"
@scheduled_action(every="10s")
def sync_inventory(context):
    return {"ok": True}
"#,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_ryvus"))
        .args(["schedule", "run", "missing"])
        .current_dir(&project.root)
        .output()
        .expect("schedule run should run");

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("schedule not found"));
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

    fn add_schedule_action(&self, file: &str, body: &str) {
        let content = format!(
            r#"from ryvus import scheduled_action
{body}
"#,
        );

        fs::write(self.root.join("src").join(file), content)
            .expect("schedule action should be written");
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

    fn add_ts_action(&self, file: &str, body: &str) {
        let sdk_path = workspace_root().join("sdk/node/src/index.ts");
        let content = format!(
            r#"import {{ apiAction }} from {sdk_path:?};
{body}
"#,
            sdk_path = format!("file://{}", sdk_path.display()),
            body = body,
        );

        fs::write(self.root.join("src").join(file), content)
            .expect("TypeScript action should be written");
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root should resolve")
}
