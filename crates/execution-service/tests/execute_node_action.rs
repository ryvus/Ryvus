use ryvus_execution_service::ExecutionService;
use ryvus_executor::{
    ActionDefinition, LocalProcessExecutor, LocalRuntimeResolver, RecordingExecutor, RuntimeKind,
};
use ryvus_persistence::FilesystemExecutionPersistence;
use ryvus_protocol::{InvocationRequest, InvocationStatus};
use serde_json::json;

#[test]
fn executes_records_and_persists_node_action() {
    let action = ActionDefinition::new(
        RuntimeKind::Node,
        "../../examples/actions/node-echo",
        "handler.js",
    );

    let service = ExecutionService::new(
        LocalRuntimeResolver::new(),
        RecordingExecutor::new(LocalProcessExecutor::new()),
        FilesystemExecutionPersistence::new(".ryvus-service-test"),
    );

    let request = InvocationRequest::new(json!({
        "message": "hello"
    }));

    let record = service
        .execute(&action, &request)
        .expect("execution service should succeed");

    assert_eq!(
        record.result.invocation_result.status,
        InvocationStatus::Success
    );

    assert_eq!(
        record.result.invocation_result.output,
        Some(json!({
            "received": { "message": "hello" },
            "handled_by": "node"
        }))
    );

    let run_dir = std::path::Path::new(".ryvus-service-test")
        .join("runs")
        .join(&record.invocation_id);

    assert!(run_dir.join("record.json").exists());
    assert!(run_dir.join("request.json").exists());
    assert!(run_dir.join("result.json").exists());
    assert!(run_dir.join("stdout.log").exists());
    assert!(run_dir.join("stderr.log").exists());
}
