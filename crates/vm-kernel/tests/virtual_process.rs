use agentos_vm_kernel::kernel::{
    KernelVm, KernelVmConfig, VirtualProcessOptions, WaitPidEvent, WaitPidFlags,
};
use agentos_vm_kernel::permissions::Permissions;
use agentos_vm_kernel::socket_table::{InetSocketAddress, SocketSpec};
use agentos_vm_kernel::vfs::MemoryFileSystem;
use std::time::Duration;

fn assert_kernel_error_code<T: std::fmt::Debug>(
    result: agentos_vm_kernel::kernel::KernelResult<T>,
    expected: &str,
) {
    let error = result.expect_err("operation should fail");
    assert_eq!(error.code(), expected);
}

fn new_kernel(vm_id: &str) -> KernelVm<MemoryFileSystem> {
    let mut config = KernelVmConfig::new(vm_id);
    config.permissions = Permissions::allow_all();
    KernelVm::new(MemoryFileSystem::new(), config)
}

#[test]
fn virtual_processes_appear_in_process_listings_and_wait_like_children() {
    let mut kernel = new_kernel("vm-virtual-process-tree");

    let parent = kernel
        .create_virtual_process(
            "binding-dispatch",
            "binding",
            "agentos-binding",
            vec![String::from("parent")],
            VirtualProcessOptions::default(),
        )
        .expect("create virtual parent");
    let child = kernel
        .create_virtual_process(
            "binding-dispatch",
            "binding",
            "agentos-binding",
            vec![String::from("child")],
            VirtualProcessOptions {
                parent_pid: Some(parent.pid()),
                ..VirtualProcessOptions::default()
            },
        )
        .expect("create virtual child");

    let processes = kernel.list_processes();
    assert_eq!(processes.get(&parent.pid()).expect("parent info").ppid, 0);
    let child_info = processes.get(&child.pid()).expect("child info");
    assert_eq!(child_info.ppid, parent.pid());
    assert_eq!(child_info.driver, "binding");
    assert_eq!(child_info.command, "agentos-binding");

    kernel
        .exit_process("binding-dispatch", child.pid(), 23)
        .expect("exit virtual child");
    let waited = kernel
        .waitpid_with_options(
            "binding-dispatch",
            parent.pid(),
            child.pid() as i32,
            WaitPidFlags::empty(),
        )
        .expect("wait child event")
        .expect("child exit event");
    assert_eq!(waited.pid, child.pid());
    assert_eq!(waited.status, 23);
    assert_eq!(waited.event, WaitPidEvent::Exited);
    assert!(
        !kernel.list_processes().contains_key(&child.pid()),
        "waited child should be reaped"
    );

    kernel
        .exit_process("binding-dispatch", parent.pid(), 0)
        .expect("exit virtual parent");
    assert_eq!(
        kernel.waitpid(parent.pid()).expect("wait parent"),
        agentos_vm_kernel::kernel::WaitPidResult {
            pid: parent.pid(),
            status: 0,
        }
    );
}

#[test]
fn virtual_process_stdio_uses_standard_fd_helpers_and_owner_checks() {
    let mut kernel = new_kernel("vm-virtual-process-stdio");
    let process = kernel
        .create_virtual_process(
            "binding-dispatch",
            "binding",
            "agentos-binding",
            vec![String::from("echo")],
            VirtualProcessOptions::default(),
        )
        .expect("create virtual process");

    let (stdin_read_fd, stdin_write_fd) = kernel
        .open_pipe("binding-dispatch", process.pid())
        .expect("open stdin pipe");
    kernel
        .fd_dup2("binding-dispatch", process.pid(), stdin_read_fd, 0)
        .expect("wire stdin pipe");
    kernel
        .fd_write("binding-dispatch", process.pid(), stdin_write_fd, b"input")
        .expect("write stdin payload");
    assert_eq!(
        kernel
            .read_process_stdin(
                "binding-dispatch",
                process.pid(),
                32,
                Some(Duration::from_millis(5))
            )
            .expect("read virtual stdin")
            .expect("stdin bytes"),
        b"input".to_vec()
    );

    let (stdout_read_fd, stdout_write_fd) = kernel
        .open_pipe("binding-dispatch", process.pid())
        .expect("open stdout pipe");
    kernel
        .fd_dup2("binding-dispatch", process.pid(), stdout_write_fd, 1)
        .expect("wire stdout pipe");
    kernel
        .write_process_stdout("binding-dispatch", process.pid(), b"stdout-data")
        .expect("write virtual stdout");
    assert_eq!(
        kernel
            .fd_read("binding-dispatch", process.pid(), stdout_read_fd, 64)
            .expect("read stdout pipe"),
        b"stdout-data".to_vec()
    );

    let (stderr_read_fd, stderr_write_fd) = kernel
        .open_pipe("binding-dispatch", process.pid())
        .expect("open stderr pipe");
    kernel
        .fd_dup2("binding-dispatch", process.pid(), stderr_write_fd, 2)
        .expect("wire stderr pipe");
    kernel
        .write_process_stderr("binding-dispatch", process.pid(), b"stderr-data")
        .expect("write virtual stderr");
    assert_eq!(
        kernel
            .fd_read("binding-dispatch", process.pid(), stderr_read_fd, 64)
            .expect("read stderr pipe"),
        b"stderr-data".to_vec()
    );

    assert_kernel_error_code(
        kernel.write_process_stdout("other-driver", process.pid(), b"denied"),
        "EPERM",
    );

    kernel
        .exit_process("binding-dispatch", process.pid(), 9)
        .expect("exit virtual process");
    assert_eq!(
        kernel.waitpid(process.pid()).expect("wait virtual process"),
        agentos_vm_kernel::kernel::WaitPidResult {
            pid: process.pid(),
            status: 9,
        }
    );
}

#[test]
fn virtual_process_exit_reclaims_owned_sockets() {
    let mut kernel = new_kernel("vm-virtual-process-socket-cleanup");
    let process = kernel
        .create_virtual_process(
            "binding-dispatch",
            "binding",
            "agentos-binding",
            Vec::new(),
            VirtualProcessOptions::default(),
        )
        .expect("create virtual process");
    let socket = kernel
        .socket_create("binding-dispatch", process.pid(), SocketSpec::tcp())
        .expect("create virtual-process socket");
    kernel
        .socket_bind_inet(
            "binding-dispatch",
            process.pid(),
            socket,
            InetSocketAddress::new("127.0.0.1", 43107),
        )
        .expect("bind virtual-process socket");
    kernel
        .socket_listen("binding-dispatch", process.pid(), socket, 1)
        .expect("listen on virtual-process socket");

    let snapshot = kernel.resource_snapshot();
    assert_eq!(snapshot.sockets, 1);
    assert_eq!(snapshot.socket_listeners, 1);

    kernel
        .exit_process("binding-dispatch", process.pid(), 0)
        .expect("exit virtual process");

    assert!(kernel.socket_get(socket).is_none());
    let snapshot = kernel.resource_snapshot();
    assert_eq!(snapshot.sockets, 0);
    assert_eq!(snapshot.socket_listeners, 0);

    let replacement = kernel
        .create_virtual_process(
            "binding-dispatch",
            "binding",
            "agentos-binding",
            Vec::new(),
            VirtualProcessOptions::default(),
        )
        .expect("create replacement virtual process");
    let replacement_socket = kernel
        .socket_create("binding-dispatch", replacement.pid(), SocketSpec::tcp())
        .expect("create replacement socket");
    kernel
        .socket_bind_inet(
            "binding-dispatch",
            replacement.pid(),
            replacement_socket,
            InetSocketAddress::new("127.0.0.1", 43107),
        )
        .expect("rebind address after virtual process exit cleanup");

    assert_eq!(
        kernel.waitpid(process.pid()).expect("wait virtual process"),
        agentos_vm_kernel::kernel::WaitPidResult {
            pid: process.pid(),
            status: 0,
        }
    );
}
