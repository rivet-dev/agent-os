use agent_os_kernel::mount_plugin::{
    FileSystemPluginFactory, FileSystemPluginRegistry, OpenFileSystemPluginRequest, PluginError,
};
use agent_os_kernel::mount_table::MountedVirtualFileSystem;
use agent_os_kernel::vfs::{MemoryFileSystem, VirtualFileSystem};
use serde_json::json;

#[derive(Debug)]
struct SeededMemoryPlugin;

impl FileSystemPluginFactory<()> for SeededMemoryPlugin {
    fn plugin_id(&self) -> &'static str {
        "seeded_memory"
    }

    fn open(
        &self,
        _request: OpenFileSystemPluginRequest<'_, ()>,
    ) -> Result<Box<dyn agent_os_kernel::mount_table::MountedFileSystem>, PluginError> {
        let mut filesystem = MemoryFileSystem::new();
        filesystem
            .write_file("/hello.txt", b"hello".to_vec())
            .expect("seed plugin filesystem");
        Ok(Box::new(MountedVirtualFileSystem::new(filesystem)))
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
