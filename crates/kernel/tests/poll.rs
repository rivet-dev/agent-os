use agent_os_kernel::command_registry::CommandDriver;
use agent_os_kernel::kernel::{KernelVm, KernelVmConfig, SpawnOptions};
use agent_os_kernel::permissions::Permissions;
use agent_os_kernel::poll::{PollFd, POLLERR, POLLHUP, POLLIN, POLLOUT};
use agent_os_kernel::vfs::MemoryFileSystem;
use std::time::{Duration, Instant};

fn kernel_vm(vm_id: &str) -> KernelVm<MemoryFileSystem> {
    let mut config = KernelVmConfig::new(vm_id);
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell driver");
    kernel
}

fn spawn_shell(kernel: &mut KernelVm<MemoryFileSystem>) -> u32 {
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
        .pid()
}

#[test]
fn poll_reports_pipe_readiness_and_hangup() {
    let mut kernel = kernel_vm("vm-poll-pipe");
    let pid = spawn_shell(&mut kernel);
    let (read_fd, write_fd) = kernel.open_pipe("shell", pid).expect("open pipe");

    let initial = kernel
        .poll_fds(
            "shell",
            pid,
            vec![PollFd::new(read_fd, POLLIN), PollFd::new(write_fd, POLLOUT)],
            0,
        )
        .expect("poll initial pipe state");
    assert_eq!(initial.ready_count, 1);
    assert_eq!(initial.fds[0].revents.bits(), 0);
    assert_eq!(initial.fds[1].revents, POLLOUT);

    kernel
        .fd_write("shell", pid, write_fd, b"hello")
        .expect("write pipe payload");
    kernel
        .fd_close("shell", pid, write_fd)
        .expect("close pipe writer");

    let ready = kernel
        .poll_fds("shell", pid, vec![PollFd::new(read_fd, POLLIN)], 0)
        .expect("poll readable pipe");
    assert_eq!(ready.ready_count, 1);
    assert!(ready.fds[0].revents.contains(POLLIN));
    assert!(ready.fds[0].revents.contains(POLLHUP));
}

#[test]
fn poll_reports_pipe_peer_close_as_pollerr_on_writer() {
    let mut kernel = kernel_vm("vm-poll-pipe-err");
    let pid = spawn_shell(&mut kernel);
    let (read_fd, write_fd) = kernel.open_pipe("shell", pid).expect("open pipe");

    kernel
        .fd_close("shell", pid, read_fd)
        .expect("close pipe reader");

    let ready = kernel
        .poll_fds("shell", pid, vec![PollFd::new(write_fd, POLLOUT)], 0)
        .expect("poll closed writer peer");
    assert_eq!(ready.ready_count, 1);
    assert!(ready.fds[0].revents.contains(POLLERR));
    assert!(!ready.fds[0].revents.contains(POLLOUT));
}

#[test]
fn poll_supports_mixed_fd_sets_and_infinite_timeout_when_ready() {
    let mut kernel = kernel_vm("vm-poll-mixed");
    let pid = spawn_shell(&mut kernel);
    let (pipe_read_fd, _pipe_write_fd) = kernel.open_pipe("shell", pid).expect("open pipe");
    let (master_fd, slave_fd, _path) = kernel.open_pty("shell", pid).expect("open pty");

    kernel
        .fd_write("shell", pid, slave_fd, b"tty-ready")
        .expect("write pty output");

    let ready = kernel
        .poll_fds(
            "shell",
            pid,
            vec![
                PollFd::new(pipe_read_fd, POLLIN),
                PollFd::new(master_fd, POLLIN),
            ],
            -1,
        )
        .expect("poll mixed fd set");
    assert_eq!(ready.ready_count, 1);
    assert_eq!(ready.fds[0].revents.bits(), 0);
    assert_eq!(ready.fds[1].revents, POLLIN);
}

#[test]
fn poll_respects_finite_timeouts() {
    let mut kernel = kernel_vm("vm-poll-timeout");
    let pid = spawn_shell(&mut kernel);
    let (read_fd, _write_fd) = kernel.open_pipe("shell", pid).expect("open pipe");

    let start = Instant::now();
    let ready = kernel
        .poll_fds("shell", pid, vec![PollFd::new(read_fd, POLLIN)], 30)
        .expect("poll timeout");
    let elapsed = start.elapsed();

    assert_eq!(ready.ready_count, 0);
    assert_eq!(ready.fds[0].revents.bits(), 0);
    assert!(
        elapsed >= Duration::from_millis(20),
        "expected poll to wait, observed {elapsed:?}"
    );
}
