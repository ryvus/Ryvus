use ryvus_executor::local_process::LocalProcessExecutor;
use ryvus_executor::{executor::Executor, ActionDefinition};
use ryvus_executor::{LocalRuntimeResolver, RuntimeKind, RuntimeResolver};
use ryvus_protocol::contract::{InvocationRequest, InvocationStatus};
use serde_json::json;
#[test]
fn invokes_python_action() {
    let action = ActionDefinition::new(
        RuntimeKind::Python,
        "../../examples/actions/python-echo",
        "handler.py",
    );

    let resolver = LocalRuntimeResolver::new();
    let target = resolver.resolve(&action).expect("action should resolve");
    let request = InvocationRequest::new(json!({
        "message": "hello"
    }));

    let executor = LocalProcessExecutor::new();

    let result = executor
        .invoke(&target, &request)
        .expect("python action should succeed");

    assert_eq!(result.invocation_result.status, InvocationStatus::Success);
    assert_eq!(
        result.invocation_result.output,
        Some(json!({
            "received": { "message": "hello" },
            "handled_by": "python"
        }))
    );
    assert_eq!(result.exit_code, Some(0));
}
