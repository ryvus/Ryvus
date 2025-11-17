use ryvus_core::error::Error;

pub trait OptionExt<T> {
    fn req(self, msg: &str) -> Result<T, Error>;
}

impl<T> OptionExt<T> for Option<T> {
    fn req(self, msg: &str) -> Result<T, Error> {
        self.ok_or_else(|| Error::NotFound(msg.to_string()))
    }
}
