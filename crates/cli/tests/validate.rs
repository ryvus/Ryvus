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
fn validate_rejects_log_config_without_leaking_values() {
    let project = TestProject::new("validate-log-config");
    project.add_action(
        "hello.py",
        r#"
@api_action(method="GET", path="/hello")
def hello():
    return {"ok": True}
"#,
    );
    fs::write(
        project.root.join(".env"),
        "RYVUS_LOG_STORE=secret-provider-name\n",
    )
    .expect("environment file");

    let output = Command::new(env!("CARGO_BIN_EXE_ryvus"))
        .arg("validate")
        .current_dir(&project.root)
        .output()
        .expect("validate command should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid log configuration: RYVUS_LOG_STORE"));
    assert!(!stderr.contains("secret-provider-name"));
}

#[test]
fn validate_rejects_unwritable_log_root_before_serving() {
    let project = TestProject::new("validate-log-root");
    project.add_action(
        "hello.py",
        r#"
@api_action(method="GET", path="/hello")
def hello():
    return {"ok": True}
"#,
    );
    let occupied = project.root.join("occupied");
    fs::write(&occupied, "not a directory").expect("occupied path");
    fs::write(
        project.root.join(".env"),
        "RYVUS_LOG_STORE=filesystem\nRYVUS_LOG_FILESYSTEM_ROOT=occupied\n",
    )
    .expect("environment file");

    let output = Command::new(env!("CARGO_BIN_EXE_ryvus"))
        .arg("validate")
        .current_dir(&project.root)
        .output()
        .expect("validate command should run");

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("invalid log configuration: RYVUS_LOG_FILESYSTEM_ROOT"));
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
fn validate_discovers_node_authorizer() {
    let project = TestProject::new("validate-node-authorizer");
    project.add_node_action(
        "auth.js",
        r#"
export default authorizer({
  name: "store",
  security: [
    { type: "http", scheme: "bearer" },
    { type: "apiKey", in: "header", name: "X-API-Key" },
  ],
  parameters: [
    { name: "X-Tenant-ID", in: "header", required: true },
  ],
  handler({ headers }) {
    return headers.authorization === "Bearer dev"
      ? { effect: "allow" }
      : { effect: "unauthorized" };
  },
});
"#,
    );
    project.add_node_action(
        "products.js",
        r#"
export default apiAction({
  method: "GET",
  path: "/store/products",
  authorizer: "store",
  handler() {
    return [];
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

    let manifest: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(project.root.join(".ryvus/action-manifest.json"))
            .expect("manifest should be written"),
    )
    .expect("manifest should parse");

    assert!(manifest["actions"]
        .as_array()
        .expect("actions should be an array")
        .iter()
        .any(|action| action["kind"] == serde_json::json!({
            "Authorizer": {
                "security": [
                    { "type": "http", "scheme": "bearer" },
                    { "type": "apiKey", "in": "header", "name": "X-API-Key" }
                ],
                "parameters": [
                    { "name": "X-Tenant-ID", "in": "header", "required": true, "type": "string" }
                ]
            }
        }) && action["name"] == "store"));
    assert!(manifest["actions"]
        .as_array()
        .expect("actions should be an array")
        .iter()
        .any(|action| action["kind"]["Api"]["path"] == "/store/products"
            && action["kind"]["Api"]["authorizer"] == "store"));
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
fn validate_writes_portal_artifacts() {
    let project = TestProject::new("validate-artifacts");
    project.add_action(
        "hello.py",
        r#"
@api_action(method="GET", path="/hello")
def hello():
    return {"ok": True}
"#,
    );
    project.add_schedule_action(
        "sync.py",
        r#"
@scheduled_action(every="10s")
def sync_inventory(context):
    return {"ok": True}
"#,
    );
    project.add_doc("docs/guide.md", "# Guide\n\nProject guide.");
    project.add_flow(
        "flows/restock.json",
        r#"{
  "key": "restock_flow",
  "steps": [
    {
      "key": "restock",
      "action": "sync_inventory"
    }
  ]
}
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

    assert!(project.root.join(".ryvus/action-manifest.json").is_file());
    assert!(project.root.join(".ryvus/catalog.json").is_file());
    assert!(project.root.join(".ryvus/openapi.json").is_file());
    assert!(project.root.join(".ryvus/schedules.json").is_file());
    assert!(project.root.join(".ryvus/flows.json").is_file());
    assert!(project.root.join(".ryvus/docs/registry.json").is_file());

    let manifest: ryvus_protocol::ActionManifest = serde_json::from_str(
        &fs::read_to_string(project.root.join(".ryvus/action-manifest.json"))
            .expect("manifest should be written"),
    )
    .expect("manifest should parse");
    let catalog: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(project.root.join(".ryvus/catalog.json"))
            .expect("catalog should be written"),
    )
    .expect("catalog should parse");
    assert_eq!(
        catalog["actions"][0]["action_revision"],
        ryvus_execution::action_revision(&manifest.actions[0])
            .expect("action revision should compute")
    );
    assert_eq!(catalog["actions"][0]["effective_policy"]["timeout"], "3s");
    assert_eq!(
        catalog["actions"][0]["effective_policy"]["retry"]["max_attempts"],
        1
    );
    assert_eq!(
        catalog["actions"][0]["effective_policy"]["retry"]["initial_delay"],
        "1s"
    );
    assert_eq!(
        catalog["actions"][0]["effective_policy"]["retry"]["backoff"],
        2.0
    );

    let openapi: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(project.root.join(".ryvus/openapi.json"))
            .expect("openapi should be written"),
    )
    .expect("openapi should parse");
    assert!(openapi["paths"]["/hello"]["get"].is_object());
    assert!(openapi["paths"]["/system/schedules/sync_inventory/run"].is_null());

    let schedules: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(project.root.join(".ryvus/schedules.json"))
            .expect("schedules should be written"),
    )
    .expect("schedules should parse");
    assert_eq!(schedules["schedules"][0]["name"], "sync_inventory");
    assert_eq!(schedules["schedules"][0]["expression"], "every 10s");
    assert_eq!(schedules["schedules"][0]["runtime"], "python");
    assert_eq!(schedules["schedules"][0]["enabled"], true);

    let flows: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(project.root.join(".ryvus/flows.json"))
            .expect("flows should be written"),
    )
    .expect("flows should parse");
    assert_eq!(flows["flows"][0]["key"], "restock_flow");
    assert_eq!(flows["flows"][0]["steps"][0]["action"], "sync_inventory");

    let registry: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(project.root.join(".ryvus/docs/registry.json"))
            .expect("docs registry should be written"),
    )
    .expect("docs registry should parse");
    assert!(registry["pages"]
        .as_array()
        .expect("pages should be an array")
        .iter()
        .any(|page| page["path"] == "/docs/guide.md"));
    let content_path = registry["pages"][0]["content_path"]
        .as_str()
        .expect("content path should be a string")
        .trim_start_matches("/.ryvus/");
    assert_eq!(
        fs::read_to_string(project.root.join(".ryvus").join(content_path))
            .expect("doc page should be copied"),
        "# Guide\n\nProject guide."
    );
}

#[test]
fn validate_writes_nested_flows_artifact() {
    let project = TestProject::new("validate-nested-flows");
    project.add_schedule_action(
        "sync.py",
        r#"
@scheduled_action(every="10s")
def sync_inventory(context):
    return {"ok": True}
"#,
    );
    project.add_flow(
        "src/modules/billing/flows/invoice_payment/invoice_payment.flows.json",
        r#"{
  "key": "billing/invoice_payment",
  "steps": [
    {
      "key": "sync",
      "action": "sync_inventory"
    }
  ]
}
"#,
    );
    project.add_flow(
        "src/modules/billing/flows/invoice_payment/draft.json",
        r#"{
  "key": "billing/draft",
  "steps": [
    {
      "key": "sync",
      "action": "sync_inventory"
    }
  ]
}
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

    let flows: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(project.root.join(".ryvus/flows.json"))
            .expect("flows should be written"),
    )
    .expect("flows should parse");
    let loaded = flows["flows"].as_array().expect("flows should be an array");

    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0]["key"], "billing/invoice_payment");
}

#[test]
fn validate_ignores_generated_flow_artifacts() {
    let project = TestProject::new("validate-ignored-flows");
    project.add_schedule_action(
        "sync.py",
        r#"
@scheduled_action(every="10s")
def sync_inventory(context):
    return {"ok": True}
"#,
    );
    project.add_flow(
        "src/modules/billing/flows/invoice_payment/invoice_payment.flows.json",
        r#"{
  "key": "billing/invoice_payment",
  "steps": [
    {
      "key": "sync",
      "action": "sync_inventory"
    }
  ]
}
"#,
    );
    project.add_flow(
        ".ryvus/generated.flows.json",
        r#"{
  "key": "generated/should_not_load",
  "steps": [
    {
      "key": "sync",
      "action": "sync_inventory"
    }
  ]
}
"#,
    );
    project.add_flow(
        "node_modules/package/vendor.flows.json",
        r#"{
  "key": "vendor/should_not_load",
  "steps": [
    {
      "key": "sync",
      "action": "sync_inventory"
    }
  ]
}
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

    let flows: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(project.root.join(".ryvus/flows.json"))
            .expect("flows should be written"),
    )
    .expect("flows should parse");
    let loaded = flows["flows"].as_array().expect("flows should be an array");

    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0]["key"], "billing/invoice_payment");
}

#[test]
fn validate_reports_nested_flow_json_path() {
    let project = TestProject::new("validate-bad-nested-flow");
    project.add_schedule_action(
        "sync.py",
        r#"
@scheduled_action(every="10s")
def sync_inventory(context):
    return {"ok": True}
"#,
    );
    project.add_flow(
        "src/modules/billing/flows/invoice_payment/bad.flows.json",
        r#"{
  "key": "billing/invoice_payment",
  "steps": [
"#,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_ryvus"))
        .arg("validate")
        .current_dir(&project.root)
        .output()
        .expect("validate command should run");

    assert!(!output.status.success());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid flow spec"));
    assert!(stderr.contains("src/modules/billing/flows/invoice_payment/bad.flows.json"));
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
fn schedule_run_does_not_fall_back_when_postgres_is_unavailable() {
    let project = TestProject::new("schedule-run-postgres-unavailable");
    project.add_schedule_action(
        "sync.py",
        r#"
@scheduled_action(every="10s")
def sync_inventory(context):
    return {"ok": True}
"#,
    );
    let database_url = "postgres://user:secret@127.0.0.1:1/ryvus";
    fs::write(
        project.root.join(".env"),
        format!("RYVUS_EXECUTION_STORE=postgres\nDATABASE_URL={database_url}\n"),
    )
    .expect("environment should be written");

    let output = Command::new(env!("CARGO_BIN_EXE_ryvus"))
        .args(["schedule", "run", "sync_inventory"])
        .current_dir(&project.root)
        .output()
        .expect("schedule run should run");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("PostgreSQL execution store initialization failed"));
    assert!(!stderr.contains(database_url));
    assert!(!stderr.contains("status: success"));
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

    fn add_doc(&self, path: &str, content: &str) {
        let full_path = self.root.join(path);
        fs::create_dir_all(full_path.parent().expect("doc should have parent"))
            .expect("doc parent should be created");
        fs::write(full_path, content).expect("doc should be written");
    }

    fn add_flow(&self, path: &str, content: &str) {
        let full_path = self.root.join(path);
        fs::create_dir_all(full_path.parent().expect("flow should have parent"))
            .expect("flow parent should be created");
        fs::write(full_path, content).expect("flow should be written");
    }

    fn add_node_action(&self, file: &str, body: &str) {
        let sdk_path = workspace_root().join("sdk/node/dist/index.js");
        let content = format!(
            r#"import {{ apiAction, authorizer }} from {sdk_path:?};
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
