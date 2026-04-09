use agent_os_kernel::command_registry::{CommandDriver, CommandRegistry};
use agent_os_kernel::kernel::{KernelVm, KernelVmConfig, SpawnOptions};
use agent_os_kernel::permissions::Permissions;
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

#[test]
fn mounted_agentos_command_paths_resolve_to_registered_drivers() {
    let mut config = KernelVmConfig::new("vm-mounted-command-path");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("wasmvm", ["sh", "xu"]))
        .expect("register drivers");

    kernel
        .mkdir("/__agentos/commands/0", true)
        .expect("create mounted command root");
    kernel
        .write_file(
            "/__agentos/commands/0/xu",
            b"#!/bin/sh\n# kernel command stub\n".to_vec(),
        )
        .expect("write mounted command stub");
    kernel
        .chmod("/__agentos/commands/0/xu", 0o755)
        .expect("chmod mounted command stub");

    let process = kernel
        .spawn_process(
            "/__agentos/commands/0/xu",
            vec![String::from("hello-agent-os")],
            SpawnOptions::default(),
        )
        .expect("spawn mounted command path");

    let info = kernel
        .list_processes()
        .get(&process.pid())
        .cloned()
        .expect("process info");
    assert_eq!(info.command, "xu");
    assert_eq!(info.driver, "wasmvm");
}
