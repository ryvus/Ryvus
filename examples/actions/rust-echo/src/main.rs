use ryvus_protocol::{InvocationRequest, InvocationResult};
use std::io::{self, Read};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;

    let request: InvocationRequest = serde_json::from_str(&input)?;

    let result = InvocationResult::success(
        request.invocation_id,
        serde_json::json!({
            "received": request.event,
            "handled_by": "rust"
        }),
    );

    println!("{}", serde_json::to_string(&result)?);

    Ok(())
}
