use anyhow::{Error, Result};
use serde::de::DeserializeOwned;
use serde::Serialize;

pub trait SerdeHelper: Serialize + DeserializeOwned {
    fn from_bytes(buffer: Vec<u8>) -> Result<Self>
    where
        Self: Sized,
    {
        rmp_serde::from_slice(&buffer).map_err(Error::new)
    }

    fn to_bytes(&self) -> Result<Vec<u8>> {
        rmp_serde::to_vec(self).map_err(Error::new)
    }
}
