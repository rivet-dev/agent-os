use anyhow::{bail, Result};
use vbare::OwnedVersionedData;

use crate::generated::v1;

macro_rules! versioned_v1 {
    ($name:ident, $type:ty) => {
        pub enum $name {
            V1($type),
        }

        impl OwnedVersionedData for $name {
            type Latest = $type;

            fn wrap_latest(latest: Self::Latest) -> Self {
                Self::V1(latest)
            }

            fn unwrap_latest(self) -> Result<Self::Latest> {
                match self {
                    Self::V1(value) => Ok(value),
                }
            }

            fn deserialize_version(payload: &[u8], version: u16) -> Result<Self> {
                match version {
                    1 => Ok(Self::V1(serde_bare::from_slice(payload)?)),
                    _ => bail!("unsupported actor UDS protocol version: {version}"),
                }
            }

            fn serialize_version(self, version: u16) -> Result<Vec<u8>> {
                match (self, version) {
                    (Self::V1(value), 1) => Ok(serde_bare::to_vec(&value)?),
                    (_, version) => bail!("unsupported actor UDS protocol version: {version}"),
                }
            }

            fn deserialize_converters() -> Vec<impl Fn(Self) -> Result<Self>> {
                Vec::<fn(Self) -> Result<Self>>::new()
            }

            fn serialize_converters() -> Vec<impl Fn(Self) -> Result<Self>> {
                Vec::<fn(Self) -> Result<Self>>::new()
            }
        }
    };
}

versioned_v1!(ClientHello, v1::ClientHello);
versioned_v1!(ServerHello, v1::ServerHello);
versioned_v1!(ClientFrame, v1::ClientFrame);
versioned_v1!(ServerFrame, v1::ServerFrame);
