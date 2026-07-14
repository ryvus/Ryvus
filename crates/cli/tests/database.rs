use std::process::Command;

#[test]
fn database_migrate_requires_a_url() {
    let output = Command::new(env!("CARGO_BIN_EXE_ryvus"))
        .args(["database", "migrate"])
        .env_remove("DATABASE_URL")
        .output()
        .expect("database migrate command should run");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success());
    assert!(stderr.contains("database URL is required"));
}
