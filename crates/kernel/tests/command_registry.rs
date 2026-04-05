use agent_os_kernel::command_registry::{CommandDriver, CommandRegistry};
use agent_os_kernel::vfs::{MemoryFileSystem, VirtualFileSystem};

#[test]
fn registers_and_resolves_commands() {
    let mut registry = CommandRegistry::new();
    let driver = CommandDriver::new("wasmvm", ["grep", "sed", "cat"]);

    registry.register(driver.clone());

    assert_eq!(registry.resolve("grep"), Some(&driver));
    assert_eq!(registry.resolve("sed"), Some(&driver));
    assert_eq!(registry.resolve("cat"), Some(&driver));
}

#[test]
fn returns_none_for_unknown_commands() {
    let registry = CommandRegistry::new();

    assert!(registry.resolve("nonexistent").is_none());
}

#[test]
fn last_registered_driver_wins_on_conflict() {
    let mut registry = CommandRegistry::new();
    registry.register(CommandDriver::new("wasmvm", ["node"]));
    registry.register(CommandDriver::new("node", ["node"]));

    assert_eq!(
        registry
            .resolve("node")
            .expect("node should resolve")
            .name(),
        "node"
    );
}

#[test]
fn list_returns_command_to_driver_name_mapping() {
    let mut registry = CommandRegistry::new();
    registry.register(CommandDriver::new("wasmvm", ["grep", "cat"]));
    registry.register(CommandDriver::new("node", ["node", "npm"]));

    let commands = registry.list();
    assert_eq!(commands.get("grep"), Some(&String::from("wasmvm")));
    assert_eq!(commands.get("node"), Some(&String::from("node")));
    assert_eq!(commands.len(), 4);
}

#[test]
fn records_warning_when_overriding_existing_command() {
    let mut registry = CommandRegistry::new();
    registry.register(CommandDriver::new("wasmvm", ["sh", "grep"]));
    registry.register(CommandDriver::new("node", ["sh"]));

    let warnings = registry.warnings();
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("sh"));
    assert!(warnings[0].contains("wasmvm"));
    assert!(warnings[0].contains("node"));
}

#[test]
fn populate_bin_creates_stub_entries() {
    let mut vfs = MemoryFileSystem::new();
    let mut registry = CommandRegistry::new();
    registry.register(CommandDriver::new("wasmvm", ["grep", "cat"]));

    registry.populate_bin(&mut vfs).expect("populate /bin");

    assert!(vfs.exists("/bin/grep"));
    assert!(vfs.exists("/bin/cat"));
    assert_eq!(
        vfs.read_text_file("/bin/grep").expect("read stub"),
        "#!/bin/sh\n# kernel command stub\n"
    );
    assert_eq!(
        vfs.stat("/bin/grep").expect("stat stub").mode & 0o777,
        0o755
    );
}
