use serde_json::Value;

/// Masks known secret values in text or structured JSON.
pub struct SensitiveMasker {
    secrets: Vec<String>,
}

impl SensitiveMasker {
    pub fn new(secrets: Vec<String>) -> Self {
        Self { secrets }
    }

    pub fn mask_text(&self, text: &str) -> String {
        let mut masked = text.to_string();
        for secret in &self.secrets {
            if !secret.is_empty() {
                masked = masked.replace(secret, "****");
            }
        }
        masked
    }

    pub fn mask_value(&self, value: &Value) -> Value {
        let text = serde_json::to_string(value).unwrap_or_default();
        let masked = self.mask_text(&text);
        serde_json::from_str(&masked).unwrap_or(Value::Null)
    }
}
