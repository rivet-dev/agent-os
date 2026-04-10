use agent_os_kernel::command_registry::CommandDriver;
use agent_os_kernel::kernel::{KernelVm, KernelVmConfig, SpawnOptions};
use agent_os_kernel::permissions::Permissions;
use agent_os_kernel::poll::{PollFd, PollTargetEntry, POLLERR, POLLHUP, POLLIN, POLLOUT};
use agent_os_kernel::socket_table::{InetSocketAddress, SocketShutdown, SocketSpec};
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

fn bind_udp_socket(kernel: &mut KernelVm<MemoryFileSystem>, pid: u32, port: u16) -> u64 {
    let socket_id = kernel
        .socket_create("shell", pid, SocketSpec::udp())
        .expect("create UDP socket");
    kernel
        .socket_bind_inet(
            "shell",
            pid,
            socket_id,
            InetSocketAddress::new("127.0.0.1", port),
        )
        .expect("bind UDP socket");
    socket_id
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
fn poll_targets_report_socket_stream_readiness_and_hangup() {
    let mut kernel = kernel_vm("vm-poll-socket-stream");
    let client_pid = spawn_shell(&mut kernel);
    let server_pid = spawn_shell(&mut kernel);

    let client_socket = kernel
        .socket_create("shell", client_pid, SocketSpec::tcp())
        .expect("create client socket");
    let server_socket = kernel
        .socket_create("shell", server_pid, SocketSpec::tcp())
        .expect("create server socket");
    kernel
        .socket_connect_pair("shell", client_pid, client_socket, server_socket)
        .expect("connect socket pair");

    let initial = kernel
        .poll_targets(
            "shell",
            client_pid,
            vec![PollTargetEntry::socket(client_socket, POLLOUT)],
            0,
        )
        .expect("poll writable client socket");
    assert_eq!(initial.ready_count, 1);
    assert_eq!(initial.targets[0].revents, POLLOUT);

    kernel
        .socket_write("shell", client_pid, client_socket, b"socket-ready")
        .expect("write client payload");
    let readable = kernel
        .poll_targets(
            "shell",
            server_pid,
            vec![PollTargetEntry::socket(server_socket, POLLIN)],
            0,
        )
        .expect("poll readable server socket");
    assert_eq!(readable.ready_count, 1);
    assert_eq!(readable.targets[0].revents, POLLIN);

    kernel
        .socket_shutdown("shell", client_pid, client_socket, SocketShutdown::Write)
        .expect("shutdown client write side");
    let hung_up = kernel
        .poll_targets(
            "shell",
            server_pid,
            vec![PollTargetEntry::socket(server_socket, POLLIN | POLLOUT)],
            0,
        )
        .expect("poll server after peer shutdown");
    assert_eq!(hung_up.ready_count, 1);
    assert!(hung_up.targets[0].revents.contains(POLLIN));
    assert!(hung_up.targets[0].revents.contains(POLLHUP));
    assert!(hung_up.targets[0].revents.contains(POLLOUT));
}

#[test]
fn poll_targets_support_mixed_fd_and_socket_sets() {
    let mut kernel = kernel_vm("vm-poll-mixed");
    let pid = spawn_shell(&mut kernel);
    let sender_pid = spawn_shell(&mut kernel);
    let (pipe_read_fd, _pipe_write_fd) = kernel.open_pipe("shell", pid).expect("open pipe");
    let (master_fd, slave_fd, _path) = kernel.open_pty("shell", pid).expect("open pty");
    let receiver_socket = bind_udp_socket(&mut kernel, pid, 43161);
    let sender_socket = bind_udp_socket(&mut kernel, sender_pid, 54061);

    kernel
        .fd_write("shell", pid, slave_fd, b"tty-ready")
        .expect("write pty output");
    kernel
        .socket_send_to_inet_loopback(
            "shell",
            sender_pid,
            sender_socket,
            InetSocketAddress::new("localhost", 43161),
            b"udp-ready",
        )
        .expect("send UDP payload");

    let ready = kernel
        .poll_targets(
            "shell",
            pid,
            vec![
                PollTargetEntry::fd(pipe_read_fd, POLLIN),
                PollTargetEntry::fd(master_fd, POLLIN),
                PollTargetEntry::socket(receiver_socket, POLLIN),
            ],
            -1,
        )
        .expect("poll mixed target set");
    assert_eq!(ready.ready_count, 2);
    assert_eq!(ready.targets[0].revents.bits(), 0);
    assert_eq!(ready.targets[1].revents, POLLIN);
    assert_eq!(ready.targets[2].revents, POLLIN);
}

#[test]
fn poll_targets_respect_finite_timeouts_across_fd_and_socket_sets() {
    let mut kernel = kernel_vm("vm-poll-timeout");
    let pid = spawn_shell(&mut kernel);
    let _peer_pid = spawn_shell(&mut kernel);
    let (read_fd, _write_fd) = kernel.open_pipe("shell", pid).expect("open pipe");
    let listener = kernel
        .socket_create("shell", pid, SocketSpec::tcp())
        .expect("create listener socket");
    kernel
        .socket_bind_inet(
            "shell",
            pid,
            listener,
            InetSocketAddress::new("127.0.0.1", 43162),
        )
        .expect("bind listener");
    kernel
        .socket_listen("shell", pid, listener, 1)
        .expect("listen");

    let start = Instant::now();
    let ready = kernel
        .poll_targets(
            "shell",
            pid,
            vec![
                PollTargetEntry::fd(read_fd, POLLIN),
                PollTargetEntry::socket(listener, POLLIN),
            ],
            30,
        )
        .expect("poll timeout");
    let elapsed = start.elapsed();

    assert_eq!(ready.ready_count, 0);
    assert_eq!(ready.targets[0].revents.bits(), 0);
    assert_eq!(ready.targets[1].revents.bits(), 0);
    assert!(
        elapsed >= Duration::from_millis(20),
        "expected poll to wait, observed {elapsed:?}"
    );
}
