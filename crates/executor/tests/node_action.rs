// use ryvus_executor::{
//     ActionDefinition, Executor, LocalProcessExecutor, LocalRuntimeResolver, RuntimeKind,
//     RuntimeResolver,
// };
// use ryvus_protocol::{InvocationRequest, InvocationStatus};
// use serde_json::json;

// #[test]
// fn invokes_node_action() {
//     let action = ActionDefinition::new(
//         RuntimeKind::Node,
//         "../../examples/actions/node-echo",
//         "handler.js",
//     );

//     let resolver = LocalRuntimeResolver::new();
//     let target = resolver.resolve(&action).expect("action should resolve");

//     let request = InvocationRequest::new(json!({
//         "message": "hello"
//     }));

//     let executor = LocalProcessExecutor::new();

//     let result = executor
//         .invoke(&target, &request)
//         .expect("node action should succeed");

//     assert_eq!(result.invocation_result.status, InvocationStatus::Success);

//     assert_eq!(
//         result.invocation_result.output,
//         Some(json!({
//             "received": { "message": "hello" },
//             "handled_by": "node"
//         }))
//     );

//     assert_eq!(result.exit_code, Some(0));
// }
