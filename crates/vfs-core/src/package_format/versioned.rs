use anyhow::{bail, Context, Result};
use vbare::OwnedVersionedData;

use super::generated::v1;

pub const PACKAGE_MANIFEST_VERSION: u16 = 1;

pub enum PackageManifest {
    V1(v1::PackageManifest),
}

impl OwnedVersionedData for PackageManifest {
    type Latest = v1::PackageManifest;

    fn wrap_latest(latest: Self::Latest) -> Self {
        Self::V1(latest)
    }

    fn unwrap_latest(self) -> Result<Self::Latest> {
        match self {
            Self::V1(data) => Ok(data),
        }
    }

    fn deserialize_version(payload: &[u8], version: u16) -> Result<Self> {
        match version {
            PACKAGE_MANIFEST_VERSION => Ok(Self::V1(serde_bare::from_slice(payload)?)),
            _ => bail!("invalid package manifest version: {version}"),
        }
    }

    fn serialize_version(self, _version: u16) -> Result<Vec<u8>> {
        match self {
            Self::V1(data) => serde_bare::to_vec(&data).map_err(Into::into),
        }
    }
}

pub enum MountIndex {
    V1(v1::MountIndex),
}

impl OwnedVersionedData for MountIndex {
    type Latest = v1::MountIndex;

    fn wrap_latest(latest: Self::Latest) -> Self {
        Self::V1(latest)
    }

    fn unwrap_latest(self) -> Result<Self::Latest> {
        match self {
            Self::V1(data) => Ok(data),
        }
    }

    fn deserialize_version(payload: &[u8], version: u16) -> Result<Self> {
        match version {
            PACKAGE_MANIFEST_VERSION => Ok(Self::V1(serde_bare::from_slice(payload)?)),
            _ => bail!("invalid package mount index version: {version}"),
        }
    }

    fn serialize_version(self, _version: u16) -> Result<Vec<u8>> {
        match self {
            Self::V1(data) => serde_bare::to_vec(&data).map_err(Into::into),
        }
    }
}

/// Encode the latest package manifest with an embedded 2-byte schema version.
pub fn encode_package_manifest(manifest: v1::PackageManifest) -> Result<Vec<u8>> {
    PackageManifest::wrap_latest(manifest)
        .serialize_with_embedded_version(PACKAGE_MANIFEST_VERSION)
        .context("encode package manifest")
}

/// Decode a versioned package manifest payload into the latest schema variant.
pub fn decode_package_manifest(payload: &[u8]) -> Result<v1::PackageManifest> {
    PackageManifest::deserialize_with_embedded_version(payload).context("decode package manifest")
}

/// Encode the latest package mount index with an embedded 2-byte schema version.
pub fn encode_mount_index(index: v1::MountIndex) -> Result<Vec<u8>> {
    MountIndex::wrap_latest(index)
        .serialize_with_embedded_version(PACKAGE_MANIFEST_VERSION)
        .context("encode package mount index")
}

/// Decode a versioned package mount index payload into the latest schema variant.
pub fn decode_mount_index(payload: &[u8]) -> Result<v1::MountIndex> {
    MountIndex::deserialize_with_embedded_version(payload).context("decode package mount index")
}
