use agent_os_kernel::kernel::{
    KernelVm, KernelVmConfig, VirtualProcessOptions, WaitPidEvent, WaitPidFlags,
};
use agent_os_kernel::vfs::MemoryFileSystem;
use std::time::Duration;

fn assert_kernel_error_code<T: std::fmt::Debug>(
    result: agent_os_kernel::kernel::KernelResult<T>,
    expected: &str,
) {
    let error = result.expect_err("operation should fail");
    assert_eq!(error.code(), expected);
}

#[test]
fn virtual_processes_appear_in_process_listings_and_wait_like_children() {
    let mut kernel = KernelVm::new(
        MemoryFileSystem::new(),
        KernelVmConfig::new("vm-virtual-process-tree"),
    );

    let parent = kernel
        .create_virtual_process(
            "tool-dispatch",
            "tool",
            "agentos-toolkit",
            vec![String::from("parent")],
            VirtualProcessOptions::default(),
        )
        .expect("create virtual parent");
    let child = kernel
        .create_virtual_process(
            "tool-dispatch",
            "tool",
            "agentos-toolkit",
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
    assert_eq!(child_info.driver, "tool");
    assert_eq!(child_info.command, "agentos-toolkit");

    kernel
        .exit_process("tool-dispatch", child.pid(), 23)
        .expect("exit virtual child");
    let waited = kernel
        .waitpid_with_options(
            "tool-dispatch",
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
        .exit_process("tool-dispatch", parent.pid(), 0)
        .expect("exit virtual parent");
    assert_eq!(
        kernel.waitpid(parent.pid()).expect("wait parent"),
        agent_os_kernel::kernel::WaitPidResult {
            pid: parent.pid(),
            status: 0,
        }
    );
}

#[test]
fn virtual_process_stdio_uses_standard_fd_helpers_and_owner_checks() {
    let mut kernel = KernelVm::new(
        MemoryFileSystem::new(),
        KernelVmConfig::new("vm-virtual-process-stdio"),
    );
    let process = kernel
        .create_virtual_process(
            "tool-dispatch",
            "tool",
            "agentos-toolkit",
            vec![String::from("echo")],
            VirtualProcessOptions::default(),
        )
        .expect("create virtual process");

    let (stdin_read_fd, stdin_write_fd) = kernel
        .open_pipe("tool-dispatch", process.pid())
        .expect("open stdin pipe");
    kernel
        .fd_dup2("tool-dispatch", process.pid(), stdin_read_fd, 0)
        .expect("wire stdin pipe");
    kernel
        .fd_write("tool-dispatch", process.pid(), stdin_write_fd, b"input")
        .expect("write stdin payload");
    assert_eq!(
        kernel
            .read_process_stdin(
                "tool-dispatch",
                process.pid(),
                32,
                Some(Duration::from_millis(5))
            )
            .expect("read virtual stdin")
            .expect("stdin bytes"),
        b"input".to_vec()
    );

    let (stdout_read_fd, stdout_write_fd) = kernel
        .open_pipe("tool-dispatch", process.pid())
        .expect("open stdout pipe");
    kernel
        .fd_dup2("tool-dispatch", process.pid(), stdout_write_fd, 1)
        .expect("wire stdout pipe");
    kernel
        .write_process_stdout("tool-dispatch", process.pid(), b"stdout-data")
        .expect("write virtual stdout");
    assert_eq!(
        kernel
            .fd_read("tool-dispatch", process.pid(), stdout_read_fd, 64)
            .expect("read stdout pipe"),
        b"stdout-data".to_vec()
    );

    let (stderr_read_fd, stderr_write_fd) = kernel
        .open_pipe("tool-dispatch", process.pid())
        .expect("open stderr pipe");
    kernel
        .fd_dup2("tool-dispatch", process.pid(), stderr_write_fd, 2)
        .expect("wire stderr pipe");
    kernel
        .write_process_stderr("tool-dispatch", process.pid(), b"stderr-data")
        .expect("write virtual stderr");
    assert_eq!(
        kernel
            .fd_read("tool-dispatch", process.pid(), stderr_read_fd, 64)
            .expect("read stderr pipe"),
        b"stderr-data".to_vec()
    );

    assert_kernel_error_code(
        kernel.write_process_stdout("other-driver", process.pid(), b"denied"),
        "EPERM",
    );

    kernel
        .exit_process("tool-dispatch", process.pid(), 9)
        .expect("exit virtual process");
    assert_eq!(
        kernel.waitpid(process.pid()).expect("wait virtual process"),
        agent_os_kernel::kernel::WaitPidResult {
            pid: process.pid(),
            status: 9,
        }
    );
}
