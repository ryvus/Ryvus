use ryvus_core::error::Error;
use serde::de::DeserializeOwned;
use serde_json::Value;

pub trait JsonValueExt {
    fn req(&self, key: &str) -> Result<&Value, Error>;

    fn parse<T: DeserializeOwned>(&self) -> Result<T, Error>;
}

impl JsonValueExt for Value {
    fn req(&self, key: &str) -> Result<&Value, Error> {
        self.get(key)
            .ok_or_else(|| Error::NotFound(format!("{key} input not found")))
    }

    fn parse<T: DeserializeOwned>(&self) -> Result<T, Error> {
        serde_json::from_value(self.clone()).map_err(|e| Error::Action(e.to_string()))
    }
}
