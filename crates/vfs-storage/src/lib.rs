#![forbid(unsafe_code)]

pub mod callback_store;
pub mod local;
#[cfg(not(target_arch = "wasm32"))]
mod mounted_fs;
pub mod s3;

pub use callback_store::{CallbackMetadataClient, CallbackMetadataStore};
pub use local::{FileBlockStore, SqliteMetadataStore};
#[cfg(not(target_arch = "wasm32"))]
pub use mounted_fs::MountedEngineFileSystem;
pub use s3::{S3BlockStore, S3BlockStoreOptions, S3ObjectBackend, S3ObjectBackendOptions};
