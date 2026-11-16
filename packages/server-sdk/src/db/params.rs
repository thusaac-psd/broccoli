use serde_json::Value as JsonValue;

pub struct Params {
    args: Vec<JsonValue>,
    idx: usize,
}

impl Params {
    pub fn new() -> Self {
        Self {
            args: Vec::new(),
            idx: 1,
        }
    }

    pub fn bind(&mut self, value: impl Into<JsonValue>) -> String {
        let placeholder = format!("${}", self.idx);
        self.args.push(value.into());
        self.idx += 1;
        placeholder
    }

    pub fn into_args(self) -> Vec<JsonValue> {
        self.args
    }
}

impl Default for Params {
    fn default() -> Self {
        Self::new()
    }
}
