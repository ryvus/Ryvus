use ryvus_sdk::api_action;
use serde_json::json;

#[api_action(method = "GET", path = "/hello")]
fn hello(event: serde_json::Value) -> serde_json::Value {
    json!({
        "message": "Hello from Ryvus Rust!"
    })
}
