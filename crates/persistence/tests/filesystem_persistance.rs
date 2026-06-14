use ryvus_executor::{
    ActionDefinition, LocalProcessExecutor, LocalRuntimeResolver, RecordingExecutor, RuntimeKind,
};
use ryvus_persistence::{ExecutionPersistence, FilesystemExecutionPersistence};
use ryvus_protocol::InvocationRequest;
use serde_json::json;

#[test]
fn records_and_persists_node_execution() {
    let action = ActionDefinition::new(
        RuntimeKind::Node,
        "../../examples/actions/node-echo",
        "handler.js",
    );

    let resolver = LocalRuntimeResolver::new();
    let target = resolver.resolve(&action).expect("action should resolve");

    let request = InvocationRequest::new(json!({
        "message": "hello"
    }));

    let recorder = RecordingExecutor::new(LocalProcessExecutor::new());
    let record = recorder
        .invoke_recorded(&target, &request)
        .expect("execution should succeed");

    let persistence = FilesystemExecutionPersistence::new(".ryvus-test");
    persistence
        .save_execution(&record)
        .expect("execution record should persist");
}
