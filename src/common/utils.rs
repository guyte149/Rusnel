use anyhow::{Error, Result};
use serde::de::DeserializeOwned;
use serde::Serialize;

pub trait SerdeHelper: Serialize + DeserializeOwned {
    fn from_json(msg: &str) -> Result<Self>
    where
        Self: Sized,
    {
        serde_json::from_str(msg).map_err(Error::new)
    }

    fn from_bytes(buffer: Vec<u8>) -> Result<Self>
    where
        Self: Sized,
    {
        let msg = String::from_utf8(buffer).map_err(Error::new)?;
        Self::from_json(&msg)
    }

    fn to_json(&self) -> Result<String> {
        serde_json::to_string(self).map_err(Error::new)
    }
}
