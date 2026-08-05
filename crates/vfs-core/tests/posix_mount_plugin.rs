use agentos_vfs_core::posix::MountedVirtualFileSystem;
use agentos_vfs_core::posix::{
    FileSystemPluginFactory, FileSystemPluginRegistry, OpenFileSystemPluginRequest, PluginError,
};
use agentos_vfs_core::posix::{MemoryFileSystem, VirtualFileSystem};
use serde_json::json;

#[derive(Debug)]
struct SeededMemoryPlugin;

#[derive(Debug)]
struct NamedPlugin(&'static str);

impl FileSystemPluginFactory<()> for SeededMemoryPlugin {
    fn plugin_id(&self) -> &'static str {
        "seeded_memory"
    }

    fn open(
        &self,
        _request: OpenFileSystemPluginRequest<'_, ()>,
    ) -> Result<Box<dyn agentos_vfs_core::posix::MountedFileSystem>, PluginError> {
        let mut filesystem = MemoryFileSystem::new();
        filesystem
            .write_file("/hello.txt", b"hello".to_vec())
            .expect("seed plugin filesystem");
        Ok(Box::new(MountedVirtualFileSystem::new(filesystem)))
    }
}

impl FileSystemPluginFactory<()> for NamedPlugin {
    fn plugin_id(&self) -> &'static str {
        self.0
    }

    fn open(
        &self,
        _request: OpenFileSystemPluginRequest<'_, ()>,
    ) -> Result<Box<dyn agentos_vfs_core::posix::MountedFileSystem>, PluginError> {
        Ok(Box::new(MountedVirtualFileSystem::new(
            MemoryFileSystem::new(),
        )))
    }
}

#[test]
fn plugin_registry_opens_registered_plugins() {
    let mut registry = FileSystemPluginRegistry::new();
    registry
        .register(SeededMemoryPlugin)
        .expect("register seeded plugin");

    let mut filesystem = registry
        .open(
            "seeded_memory",
            OpenFileSystemPluginRequest {
                vm_id: "vm-1",
                guest_path: "/workspace",
                read_only: false,
                config: &json!({}),
                context: &(),
            },
        )
        .expect("open seeded plugin");

    assert_eq!(
        filesystem
            .read_file("/hello.txt")
            .expect("read plugin file"),
        b"hello".to_vec()
    );
}

#[test]
fn plugin_registry_rejects_ids_that_are_not_mount_type_tokens() {
    for plugin_id in ["", "bad/id", "bad id", "bad\nid", "bad:id", "bad😀id"] {
        let mut registry = FileSystemPluginRegistry::new();
        let error = registry
            .register(NamedPlugin(plugin_id))
            .expect_err("invalid plugin id should be rejected");

        assert_eq!(error.code(), "EINVAL");
        assert!(
            error.message().contains("invalid filesystem plugin id"),
            "unexpected error: {error}"
        );
        assert!(registry.plugin_ids().is_empty());
    }
}

#[test]
fn plugin_registry_rejects_duplicate_or_unknown_plugins() {
    let mut registry = FileSystemPluginRegistry::new();
    registry
        .register(SeededMemoryPlugin)
        .expect("register initial plugin");

    let duplicate = registry
        .register(SeededMemoryPlugin)
        .expect_err("duplicate registration should fail");
    assert_eq!(duplicate.code(), "EEXIST");

    let missing = match registry.open(
        "missing",
        OpenFileSystemPluginRequest {
            vm_id: "vm-1",
            guest_path: "/workspace",
            read_only: false,
            config: &json!({}),
            context: &(),
        },
    ) {
        Ok(_) => panic!("missing plugin should fail"),
        Err(error) => error,
    };
    assert_eq!(missing.code(), "ENOSYS");
}
