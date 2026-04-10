use agent_os_kernel::command_registry::CommandDriver;
use agent_os_kernel::kernel::{KernelProcessHandle, KernelVm, KernelVmConfig, SpawnOptions};
use agent_os_kernel::permissions::Permissions;
use agent_os_kernel::resource_accounting::ResourceLimits;
use agent_os_kernel::socket_table::{SocketSpec, SocketState};
use agent_os_kernel::vfs::MemoryFileSystem;

fn spawn_shell(kernel: &mut KernelVm<MemoryFileSystem>) -> KernelProcessHandle {
    kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                ..SpawnOptions::default()
            },
        )
        .expect("spawn shell")
}

#[test]
fn socket_resources_appear_in_kernel_resource_snapshot_and_cleanup_with_process_exit() {
    let mut config = KernelVmConfig::new("vm-socket-resources");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");

    let process = spawn_shell(&mut kernel);
    let listener = kernel
        .socket_create("shell", process.pid(), SocketSpec::tcp())
        .expect("create listener socket");
    kernel
        .socket_set_state("shell", process.pid(), listener, SocketState::Listening)
        .expect("mark listener");

    let connected = kernel
        .socket_create("shell", process.pid(), SocketSpec::tcp())
        .expect("create connected socket");
    kernel
        .socket_set_state("shell", process.pid(), connected, SocketState::Connected)
        .expect("mark connected");

    let snapshot = kernel.resource_snapshot();
    assert_eq!(snapshot.sockets, 2);
    assert_eq!(snapshot.socket_listeners, 1);
    assert_eq!(snapshot.socket_connections, 1);

    process.finish(0);

    let snapshot_after_exit = kernel.resource_snapshot();
    assert_eq!(snapshot_after_exit.sockets, 0);
    assert_eq!(snapshot_after_exit.socket_listeners, 0);
    assert_eq!(snapshot_after_exit.socket_connections, 0);
}

#[test]
fn socket_resource_limits_reject_extra_sockets_and_connections() {
    let mut config = KernelVmConfig::new("vm-socket-limits");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_sockets: Some(2),
        max_connections: Some(1),
        ..ResourceLimits::default()
    };

    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");

    let process = spawn_shell(&mut kernel);
    let listener = kernel
        .socket_create("shell", process.pid(), SocketSpec::tcp())
        .expect("create listener socket");
    kernel
        .socket_set_state("shell", process.pid(), listener, SocketState::Listening)
        .expect("mark listener");

    let first_connection = kernel
        .socket_create("shell", process.pid(), SocketSpec::tcp())
        .expect("create first connection socket");
    kernel
        .socket_set_state(
            "shell",
            process.pid(),
            first_connection,
            SocketState::Connected,
        )
        .expect("mark first connection");

    let socket_error = kernel
        .socket_create("shell", process.pid(), SocketSpec::tcp())
        .expect_err("third socket should exceed max_sockets");
    assert_eq!(socket_error.code(), "EAGAIN");

    kernel
        .socket_close("shell", process.pid(), listener)
        .expect("close listener");
    let second_connection = kernel
        .socket_create("shell", process.pid(), SocketSpec::tcp())
        .expect("create replacement socket");
    let connection_error = kernel
        .socket_set_state(
            "shell",
            process.pid(),
            second_connection,
            SocketState::Connected,
        )
        .expect_err("second connection should exceed max_connections");
    assert_eq!(connection_error.code(), "EAGAIN");
}
