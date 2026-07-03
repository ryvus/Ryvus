use serde_json::Value;
use tracing::debug;

/// Deep merge `b` into `a`:
/// - Objects are merged recursively
/// - Arrays are replaced
/// - Other values are overwritten by `b`
pub fn deep_merge(mut a: Value, b: Value) -> Value {
    debug!("{:?}, {:?}", a, b);

    match (a, b) {
        (Value::Object(mut a_obj), Value::Object(b_obj)) => {
            for (k, v_b) in b_obj {
                let v_a = a_obj.remove(&k);
                a_obj.insert(k, deep_merge(v_a.unwrap_or(Value::Null), v_b));
            }
            Value::Object(a_obj)
        }

        // Arrays are REPLACED, not merged
        (Value::Array(_), Value::Array(b_arr)) => Value::Array(b_arr),

        // Everything else → override
        (_, v_b) => v_b,
    }
}
