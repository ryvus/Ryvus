use ryvus_executor::local_process::LocalProcessExecutor;
use ryvus_executor::{executor::Executor, ActionDefinition};
use ryvus_executor::{LocalRuntimeResolver, RuntimeKind, RuntimeResolver};
use ryvus_protocol::contract::{InvocationRequest, InvocationStatus};
use serde_json::json;

#[test]
fn invokes_rust_action() {
    let action = ActionDefinition::new(
        RuntimeKind::Rust,
        "../../examples/actions/rust-echo",
        "Cargo.toml",
    );

    let resolver = LocalRuntimeResolver::new();
    let target = resolver.resolve(&action).expect("action should resolve");
    let executor = LocalProcessExecutor::new();

    let request = InvocationRequest::new(json!({
        "message": "hello"
    }));

    let result = executor
        .invoke(&target, &request)
        .expect("rust action should succeed");

    assert_eq!(result.invocation_result.status, InvocationStatus::Success);
    assert_eq!(
        result.invocation_result.output,
        Some(json!({
            "received": { "message": "hello" },
            "handled_by": "rust"
        }))
    );
    assert_eq!(result.exit_code, Some(0));
}
