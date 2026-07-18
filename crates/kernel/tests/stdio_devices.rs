use agentos_kernel::command_registry::CommandDriver;
use agentos_kernel::kernel::{KernelVm, KernelVmConfig, SpawnOptions};
use agentos_kernel::permissions::Permissions;
use agentos_kernel::vfs::MemoryFileSystem;

#[test]
fn default_process_stdout_and_stderr_accept_writes_without_pipe_rewiring() {
    let mut config = KernelVmConfig::new("vm-stdio-devices");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");

    let process = kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                ..SpawnOptions::default()
            },
        )
        .expect("spawn shell");

    assert_eq!(
        kernel
            .write_process_stdout("shell", process.pid(), b"stdout-data")
            .expect("write stdout"),
        "stdout-data".len()
    );
    assert_eq!(
        kernel
            .write_process_stderr("shell", process.pid(), b"stderr-data")
            .expect("write stderr"),
        "stderr-data".len()
    );

    assert_eq!(
        kernel
            .read_file("/dev/stdout")
            .expect_err("stdout should not persist")
            .code(),
        "ENOENT"
    );
    assert_eq!(
        kernel
            .read_file("/dev/stderr")
            .expect_err("stderr should not persist")
            .code(),
        "ENOENT"
    );
}

#[test]
fn terminal_size_distinguishes_valid_non_tty_from_invalid_fd() {
    let mut config = KernelVmConfig::new("vm-terminal-size-errno");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");
    let process = kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                ..SpawnOptions::default()
            },
        )
        .expect("spawn shell");

    assert_eq!(
        kernel
            .pty_window_size("shell", process.pid(), 1)
            .expect_err("valid stdout is not a terminal")
            .code(),
        "ENOTTY"
    );
    assert_eq!(
        kernel
            .pty_window_size("shell", process.pid(), 999)
            .expect_err("unknown descriptor remains EBADF")
            .code(),
        "EBADF"
    );
}
