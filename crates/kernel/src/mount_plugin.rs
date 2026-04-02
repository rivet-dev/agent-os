use crate::mount_table::MountedFileSystem;
use crate::vfs::VfsError;
use serde_json::Value;
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

#[derive(Debug)]
pub struct OpenFileSystemPluginRequest<'a, Context> {
    pub vm_id: &'a str,
    pub guest_path: &'a str,
    pub read_only: bool,
    pub config: &'a Value,
    pub context: &'a Context,
}

pub trait FileSystemPluginFactory<Context>: Send + Sync {
    fn plugin_id(&self) -> &'static str;
    fn open(
        &self,
        request: OpenFileSystemPluginRequest<'_, Context>,
    ) -> Result<Box<dyn MountedFileSystem>, PluginError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginError {
    code: &'static str,
    message: String,
}

impl PluginError {
    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new("ENOSYS", message)
    }

    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::new("EINVAL", message)
    }

    pub fn already_exists(message: impl Into<String>) -> Self {
        Self::new("EEXIST", message)
    }
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl Error for PluginError {}

impl From<VfsError> for PluginError {
    fn from(error: VfsError) -> Self {
        Self::new(error.code(), error.message().to_owned())
    }
}

pub struct FileSystemPluginRegistry<Context> {
    factories: BTreeMap<String, Box<dyn FileSystemPluginFactory<Context>>>,
}

impl<Context> Default for FileSystemPluginRegistry<Context> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Context> FileSystemPluginRegistry<Context> {
    pub fn new() -> Self {
        Self {
            factories: BTreeMap::new(),
        }
    }

    pub fn register(
        &mut self,
        factory: impl FileSystemPluginFactory<Context> + 'static,
    ) -> Result<(), PluginError> {
        let plugin_id = factory.plugin_id();
        if self.factories.contains_key(plugin_id) {
            return Err(PluginError::already_exists(format!(
                "filesystem plugin already registered: {plugin_id}"
            )));
        }

        self.factories
            .insert(plugin_id.to_owned(), Box::new(factory));
        Ok(())
    }

    pub fn open(
        &self,
        plugin_id: &str,
        request: OpenFileSystemPluginRequest<'_, Context>,
    ) -> Result<Box<dyn MountedFileSystem>, PluginError> {
        let Some(factory) = self.factories.get(plugin_id) else {
            return Err(PluginError::unsupported(format!(
                "filesystem plugin is not registered: {plugin_id}"
            )));
        };

        factory.open(request)
    }

    pub fn plugin_ids(&self) -> Vec<String> {
        self.factories.keys().cloned().collect()
    }
}
