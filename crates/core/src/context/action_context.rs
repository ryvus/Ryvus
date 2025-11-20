use serde_json::Value;

#[derive(Debug, Clone, Default)]
pub struct ActionContext {
    pub id: String,
    pub input: Option<Value>,
    pub result: Option<Value>,
    pub config: Option<Value>,
}

impl ActionContext {
    pub fn new(id: &str, input: Value) -> Self {
        Self {
            id: id.into(),
            input: Some(input),
            result: None,
            config: None,
        }
    }

    pub fn get_result(&self) -> Option<&Value> {
        self.result.as_ref()
    }

    pub fn get_input(&self) -> Option<&Value> {
        self.input.as_ref()
    }

    pub fn set_input(&mut self, value: Value) {
        self.input = Some(value.clone());
    }

    pub fn set_result(&mut self, value: Value) {
        self.result = Some(value.clone());
    }
}
