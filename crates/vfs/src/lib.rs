#![deny(unsafe_code)]

#[cfg(not(target_arch = "wasm32"))]
pub mod adapter;
pub mod engine;
mod extent;
pub mod package_format;
pub mod posix;
