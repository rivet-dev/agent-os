#![forbid(unsafe_code)]

pub mod callback_store;
#[cfg(feature = "local")]
pub mod local;
#[cfg(all(not(target_arch = "wasm32"), feature = "mounted"))]
mod mounted_fs;
#[cfg(feature = "s3")]
pub mod s3;

pub use callback_store::{CallbackMetadataClient, CallbackMetadataStore};
#[cfg(feature = "local")]
pub use local::{FileBlockStore, SqliteMetadataStore};
#[cfg(all(not(target_arch = "wasm32"), feature = "mounted"))]
pub use mounted_fs::MountedEngineFileSystem;
#[cfg(feature = "s3")]
pub use s3::{S3BlockStore, S3BlockStoreOptions, S3ObjectBackend, S3ObjectBackendOptions};
