use executor::contract::{InvocationRequest, InvocationStatus};
use executor::executor::Executor;
use executor::local_process::LocalProcessExecutor;
use serde_json::json;
#[test]
fn invokes_python_action() {
    let request = InvocationRequest::new(json!({
        "message": "hello"
    }));

    let executor = LocalProcessExecutor::with_args(
        "python3",
        ["../..//examples/actions/python-echo/handler.py"],
    );

    let result = executor
        .invoke(request)
        .expect("python action should succeed");

    assert_eq!(result.status, InvocationStatus::Success);
    assert_eq!(
        result.output,
        Some(json!({
            "received": { "message": "hello" },
            "handled_by": "python"
        }))
    );
}
