// V8 runtime process entry point — UDS listener with socket path security

mod bridge;
mod execution;
mod host_call;
mod ipc;
mod ipc_binary;
mod isolate;
mod session;
mod snapshot;
mod stream;
mod timeout;

use std::collections::HashMap;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use host_call::CallIdRouter;
use ipc_binary::BinaryFrame;
use session::SessionManager;
use snapshot::SnapshotCache;

/// Close all file descriptors > 2 (stdin/stdout/stderr preserved).
/// Called at process startup to prevent the parent from leaking FDs into the V8 runtime.
fn close_inherited_fds() {
    // Collect open FDs from /proc/self/fd, then close all > 2
    let fds: Vec<i32> = fs::read_dir("/proc/self/fd")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_string_lossy().parse::<i32>().ok())
        .filter(|&fd| fd > 2)
        .collect();
    for fd in fds {
        unsafe {
            libc::close(fd);
        }
    }
}

/// Set FD_CLOEXEC on a file descriptor so it won't be inherited by child processes.
fn set_cloexec(fd: i32) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Create a self-pipe for signal-driven wakeup of poll(2).
/// Returns (read_fd, write_fd), both set to non-blocking and CLOEXEC.
fn create_self_pipe() -> io::Result<(RawFd, RawFd)> {
    let mut fds = [0i32; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // Set non-blocking and CLOEXEC on both ends
    for &fd in &fds {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            unsafe {
                libc::close(fds[0]);
                libc::close(fds[1]);
            }
            return Err(io::Error::last_os_error());
        }
        set_cloexec(fd)?;
    }
    Ok((fds[0], fds[1]))
}

/// Drain all bytes from a non-blocking FD (self-pipe read end after wakeup).
fn drain_pipe(fd: RawFd) {
    let mut buf = [0u8; 64];
    loop {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n <= 0 {
            break;
        }
    }
}

/// Generate a 64-bit random hex string from /dev/urandom.
fn random_hex_64() -> io::Result<String> {
    let mut buf = [0u8; 8];
    let mut f = fs::File::open("/dev/urandom")?;
    f.read_exact(&mut buf)?;
    Ok(buf.iter().fold(String::with_capacity(16), |mut s, b| {
        use std::fmt::Write;
        let _ = write!(s, "{:02x}", b);
        s
    }))
}

/// Create a secure tmpdir with 0700 permissions and return the socket path inside it.
/// Uses DirBuilder::mode() to set permissions atomically via mkdir(2), avoiding
/// a TOCTOU race between create_dir and set_permissions.
fn create_socket_dir() -> io::Result<(PathBuf, PathBuf)> {
    let suffix = random_hex_64()?;
    let tmpdir = std::env::temp_dir().join(format!("secure-exec-{}", suffix));
    fs::DirBuilder::new().mode(0o700).create(&tmpdir)?;
    let socket_path = tmpdir.join("secure-exec.sock");
    Ok((tmpdir, socket_path))
}

/// Clean up socket file and directory
fn cleanup(socket_path: &PathBuf, tmpdir: &PathBuf) {
    let _ = fs::remove_file(socket_path);
    let _ = fs::remove_dir(tmpdir);
}

/// Constant-time byte comparison to prevent timing oracle on auth token.
/// Returns true if both slices have equal length and identical contents.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Authenticate a new connection by reading the first message as an Authenticate token.
/// Returns true if authentication succeeds, false otherwise.
fn authenticate_connection(stream: &mut UnixStream, expected_token: &str) -> bool {
    // Connection is blocking — read the first message
    match ipc_binary::read_frame(stream) {
        Ok(BinaryFrame::Authenticate { token }) => {
            if constant_time_eq(token.as_bytes(), expected_token.as_bytes()) {
                true
            } else {
                eprintln!("auth failed: invalid token");
                false
            }
        }
        Ok(_) => {
            eprintln!("auth failed: first message must be Authenticate");
            false
        }
        Err(e) => {
            eprintln!("auth failed: read error: {}", e);
            false
        }
    }
}

/// Dedicated writer thread per connection: drains the frame channel and
/// writes complete frames atomically to the socket. Session threads send
/// pre-serialized byte vectors through the channel, so no shared mutex
/// is held during V8 serialization or frame construction.
fn ipc_writer_thread(rx: crossbeam_channel::Receiver<Vec<u8>>, mut writer: UnixStream) {
    while let Ok(bytes) = rx.recv() {
        if let Err(e) = writer.write_all(&bytes) {
            eprintln!("IPC writer thread: write error: {}", e);
            break;
        }
    }
}

/// Global connection ID counter
static NEXT_CONNECTION_ID: AtomicU64 = AtomicU64::new(1);

/// Handle an authenticated connection: read messages and dispatch to sessions.
fn handle_connection(
    mut stream: UnixStream,
    connection_id: u64,
    session_mgr: Arc<Mutex<SessionManager>>,
    snapshot_cache: Arc<SnapshotCache>,
) {
    loop {
        // Read next binary frame from connection
        let frame = match ipc_binary::read_frame(&mut stream) {
            Ok(f) => f,
            Err(ref e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                // Client disconnected — clean up sessions
                break;
            }
            Err(e) => {
                eprintln!("connection {}: read error: {}", connection_id, e);
                break;
            }
        };

        // Dispatch frame
        match frame {
            BinaryFrame::Authenticate { .. } => {
                eprintln!(
                    "connection {}: unexpected Authenticate after handshake",
                    connection_id
                );
                break;
            }
            BinaryFrame::CreateSession {
                session_id,
                heap_limit_mb,
                cpu_time_limit_ms,
            } => {
                let hlm = if heap_limit_mb == 0 {
                    None
                } else {
                    Some(heap_limit_mb)
                };
                let ctl = if cpu_time_limit_ms == 0 {
                    None
                } else {
                    Some(cpu_time_limit_ms)
                };
                let mut mgr = session_mgr.lock().unwrap();
                if let Err(e) = mgr.create_session(session_id.clone(), connection_id, hlm, ctl) {
                    eprintln!(
                        "connection {}: create session {} failed: {}",
                        connection_id, session_id, e
                    );
                }
            }
            BinaryFrame::DestroySession { session_id } => {
                let mut mgr = session_mgr.lock().unwrap();
                if let Err(e) = mgr.destroy_session(&session_id, connection_id) {
                    eprintln!(
                        "connection {}: destroy session {} failed: {}",
                        connection_id, session_id, e
                    );
                }
            }
            // Route BridgeResponse via call_id → session_id routing table
            BinaryFrame::BridgeResponse { call_id, .. } => {
                let mgr = session_mgr.lock().unwrap();
                let router = mgr.call_id_router();
                let session_id = router.lock().unwrap().remove(&call_id);

                if let Some(sid) = session_id {
                    if let Err(e) = mgr.send_to_session(&sid, connection_id, frame) {
                        eprintln!(
                            "connection {}: route BridgeResponse call_id={} to session {} failed: {}",
                            connection_id, call_id, sid, e
                        );
                    }
                } else {
                    eprintln!(
                        "connection {}: no session found for BridgeResponse call_id={}",
                        connection_id, call_id
                    );
                }
            }
            // Forward session-scoped messages to the session thread
            frame @ (BinaryFrame::Execute { .. }
            | BinaryFrame::InjectGlobals { .. }
            | BinaryFrame::StreamEvent { .. }
            | BinaryFrame::TerminateExecution { .. }) => {
                let session_id = match &frame {
                    BinaryFrame::Execute { session_id, .. }
                    | BinaryFrame::InjectGlobals { session_id, .. }
                    | BinaryFrame::StreamEvent { session_id, .. }
                    | BinaryFrame::TerminateExecution { session_id } => session_id.clone(),
                    _ => unreachable!(),
                };
                let mgr = session_mgr.lock().unwrap();
                if let Err(e) = mgr.send_to_session(&session_id, connection_id, frame) {
                    eprintln!(
                        "connection {}: send to session {} failed: {}",
                        connection_id, session_id, e
                    );
                }
            }
            // Handle WarmSnapshot: pre-warm the snapshot cache (fire-and-forget, no response)
            BinaryFrame::WarmSnapshot { bridge_code } => {
                if let Err(e) = snapshot_cache.get_or_create(&bridge_code) {
                    eprintln!("connection {}: WarmSnapshot failed: {}", connection_id, e);
                }
            }
            _ => {
                eprintln!("connection {}: unexpected frame type", connection_id);
            }
        }
    }

    // Connection closed — clean up all sessions owned by this connection
    let mut mgr = session_mgr.lock().unwrap();
    mgr.destroy_connection_sessions(connection_id);
}

fn main() {
    // Close all inherited FDs > 2 before doing anything else
    close_inherited_fds();

    // Initialize codec from env before any sessions are created
    bridge::init_codec();

    // Initialize V8 platform on the main thread before any session threads
    isolate::init_v8_platform();

    // Shared snapshot cache for fast isolate creation across all connections/sessions
    let snapshot_cache = Arc::new(SnapshotCache::new(4));

    // Read auth token from environment
    let auth_token = std::env::var("SECURE_EXEC_V8_TOKEN")
        .expect("SECURE_EXEC_V8_TOKEN environment variable must be set");

    // Determine max concurrency from env or default to available CPUs
    let max_concurrency = std::env::var("SECURE_EXEC_V8_MAX_SESSIONS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
        });

    // Create socket directory with 64-bit random suffix and 0700 permissions
    let (tmpdir, socket_path) = create_socket_dir().expect("failed to create socket directory");

    // Bind UDS listener
    let listener = match UnixListener::bind(&socket_path) {
        Ok(l) => l,
        Err(e) => {
            cleanup(&socket_path, &tmpdir);
            panic!("failed to bind UDS: {}", e);
        }
    };
    set_cloexec(listener.as_raw_fd()).expect("failed to set CLOEXEC on listener");

    // Print socket path to stdout so host process can connect
    println!("{}", socket_path.display());
    io::stdout().flush().expect("failed to flush stdout");

    // Create self-pipe for signal-driven poll(2) wakeup
    let (sig_read_fd, sig_write_fd) =
        create_self_pipe().expect("failed to create signal self-pipe");

    // Register SIGTERM/SIGINT to write to the self-pipe, waking poll(2)
    signal_hook::flag::register_conditional_default(
        signal_hook::consts::SIGTERM,
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
    )
    .ok();
    unsafe {
        signal_hook::low_level::register(signal_hook::consts::SIGTERM, move || {
            // Async-signal-safe: write(2) a single byte to the self-pipe
            let b: u8 = 1;
            libc::write(sig_write_fd, &b as *const u8 as *const libc::c_void, 1);
        })
        .expect("failed to register SIGTERM handler");
        signal_hook::low_level::register(signal_hook::consts::SIGINT, move || {
            let b: u8 = 1;
            libc::write(sig_write_fd, &b as *const u8 as *const libc::c_void, 1);
        })
        .expect("failed to register SIGINT handler");
    }

    // Listener stays blocking — poll(2) handles readiness
    let listener_fd = listener.as_raw_fd();
    let mut pollfds = [
        libc::pollfd {
            fd: listener_fd,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: sig_read_fd,
            events: libc::POLLIN,
            revents: 0,
        },
    ];

    // Accept connections via poll(2)
    loop {
        pollfds[0].revents = 0;
        pollfds[1].revents = 0;

        let ret = unsafe { libc::poll(pollfds.as_mut_ptr(), 2, -1) };
        if ret < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                // EINTR — re-check signal pipe
                if pollfds[1].revents & libc::POLLIN != 0 {
                    break;
                }
                continue;
            }
            eprintln!("poll error: {}", err);
            break;
        }

        // Signal pipe readable — shutdown requested
        if pollfds[1].revents & libc::POLLIN != 0 {
            drain_pipe(sig_read_fd);
            break;
        }

        // Listener readable — accept new connection
        if pollfds[0].revents & libc::POLLIN != 0 {
            match listener.accept() {
                Ok((mut stream, _addr)) => {
                    // Set CLOEXEC on accepted connection
                    set_cloexec(stream.as_raw_fd()).expect("failed to set CLOEXEC on connection");

                    // Accepted stream is already blocking (listener is blocking)

                    // Require authentication as the first message
                    if !authenticate_connection(&mut stream, &auth_token) {
                        drop(stream);
                        continue;
                    }

                    // Create per-connection writer thread and IPC channel
                    let writer_stream = stream.try_clone().expect("failed to clone UDS stream");
                    set_cloexec(writer_stream.as_raw_fd())
                        .expect("failed to set CLOEXEC on cloned stream");
                    let (ipc_tx, ipc_rx) = crossbeam_channel::bounded::<Vec<u8>>(1024);
                    let call_id_router: CallIdRouter = Arc::new(Mutex::new(HashMap::new()));

                    // Spawn dedicated writer thread — only this thread writes to the socket
                    let conn_id = NEXT_CONNECTION_ID.fetch_add(1, Ordering::Relaxed);
                    std::thread::Builder::new()
                        .name(format!("writer-{}", conn_id))
                        .spawn(move || {
                            ipc_writer_thread(ipc_rx, writer_stream);
                        })
                        .expect("failed to spawn IPC writer thread");

                    // Create shared session manager for this connection
                    let session_mgr = Arc::new(Mutex::new(SessionManager::new(
                        max_concurrency,
                        ipc_tx,
                        call_id_router,
                        Arc::clone(&snapshot_cache),
                    )));

                    // Authenticated — spawn connection handler thread
                    let mgr = Arc::clone(&session_mgr);
                    let snap = Arc::clone(&snapshot_cache);
                    std::thread::Builder::new()
                        .name(format!("conn-{}", conn_id))
                        .spawn(move || {
                            handle_connection(stream, conn_id, mgr, snap);
                        })
                        .expect("failed to spawn connection handler");
                }
                Err(e) => {
                    // Transient errors: log and continue accepting
                    let is_transient = matches!(
                        e.raw_os_error(),
                        Some(libc::EMFILE)
                            | Some(libc::ENFILE)
                            | Some(libc::ECONNABORTED)
                            | Some(libc::EINTR)
                            | Some(libc::EAGAIN)
                    );
                    if is_transient {
                        eprintln!("transient accept error (continuing): {}", e);
                        continue;
                    }
                    eprintln!("fatal accept error: {}", e);
                    break;
                }
            }
        }
    }

    // Close self-pipe FDs
    unsafe {
        libc::close(sig_read_fd);
        libc::close(sig_write_fd);
    }

    // Graceful shutdown: close listener, remove socket
    drop(listener);
    cleanup(&socket_path, &tmpdir);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::io::AsRawFd;
    use std::os::unix::net::UnixStream;

    /// Helper: bind a temp UDS listener and return (listener, socket_path, tmpdir)
    fn temp_listener() -> (UnixListener, PathBuf, PathBuf) {
        let (tmpdir, socket_path) = create_socket_dir().expect("create socket dir");
        let listener = UnixListener::bind(&socket_path).expect("bind");
        (listener, socket_path, tmpdir)
    }

    #[test]
    fn set_cloexec_sets_flag_on_fd() {
        // pipe() does NOT set CLOEXEC, so this is a good test target
        let mut fds = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);

        let flags_before = unsafe { libc::fcntl(fds[0], libc::F_GETFD) };
        assert_eq!(
            flags_before & libc::FD_CLOEXEC,
            0,
            "pipe should not have CLOEXEC initially"
        );

        set_cloexec(fds[0]).expect("set_cloexec");

        let flags_after = unsafe { libc::fcntl(fds[0], libc::F_GETFD) };
        assert_ne!(
            flags_after & libc::FD_CLOEXEC,
            0,
            "CLOEXEC should be set after set_cloexec"
        );

        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }

    #[test]
    fn set_cloexec_returns_error_for_bad_fd() {
        assert!(set_cloexec(-1).is_err());
        assert!(set_cloexec(9999).is_err());
    }

    #[test]
    fn listener_has_cloexec_after_set() {
        let (listener, socket_path, tmpdir) = temp_listener();
        set_cloexec(listener.as_raw_fd()).expect("set cloexec on listener");

        let flags = unsafe { libc::fcntl(listener.as_raw_fd(), libc::F_GETFD) };
        assert!(flags >= 0, "fcntl should succeed");
        assert_ne!(flags & libc::FD_CLOEXEC, 0, "listener should have CLOEXEC");

        cleanup(&socket_path, &tmpdir);
    }

    #[test]
    fn accepted_stream_has_cloexec_after_set() {
        let (listener, socket_path, tmpdir) = temp_listener();

        let _client = UnixStream::connect(&socket_path).expect("connect");
        let (stream, _) = listener.accept().expect("accept");
        set_cloexec(stream.as_raw_fd()).expect("set cloexec on stream");

        let flags = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_GETFD) };
        assert!(flags >= 0, "fcntl should succeed");
        assert_ne!(
            flags & libc::FD_CLOEXEC,
            0,
            "accepted stream should have CLOEXEC"
        );

        cleanup(&socket_path, &tmpdir);
    }

    #[test]
    fn cloned_stream_has_cloexec_after_set() {
        let (listener, socket_path, tmpdir) = temp_listener();

        let _client = UnixStream::connect(&socket_path).expect("connect");
        let (stream, _) = listener.accept().expect("accept");
        let clone = stream.try_clone().expect("clone");
        set_cloexec(clone.as_raw_fd()).expect("set cloexec on clone");

        let flags = unsafe { libc::fcntl(clone.as_raw_fd(), libc::F_GETFD) };
        assert!(flags >= 0, "fcntl should succeed");
        assert_ne!(
            flags & libc::FD_CLOEXEC,
            0,
            "cloned stream should have CLOEXEC"
        );

        cleanup(&socket_path, &tmpdir);
    }

    #[test]
    fn auth_accepts_valid_token() {
        let (listener, socket_path, tmpdir) = temp_listener();
        let token = "test-secret-token-abc123";

        // Client connects and sends valid Authenticate
        let mut client = UnixStream::connect(&socket_path).expect("connect");
        let (mut server_stream, _) = listener.accept().expect("accept");

        ipc_binary::write_frame(
            &mut client,
            &BinaryFrame::Authenticate {
                token: token.into(),
            },
        )
        .expect("write auth");

        assert!(authenticate_connection(&mut server_stream, token));

        cleanup(&socket_path, &tmpdir);
    }

    #[test]
    fn auth_rejects_wrong_token() {
        let (listener, socket_path, tmpdir) = temp_listener();

        let mut client = UnixStream::connect(&socket_path).expect("connect");
        let (mut server_stream, _) = listener.accept().expect("accept");

        ipc_binary::write_frame(
            &mut client,
            &BinaryFrame::Authenticate {
                token: "wrong-token".into(),
            },
        )
        .expect("write auth");

        assert!(!authenticate_connection(
            &mut server_stream,
            "correct-token"
        ));

        cleanup(&socket_path, &tmpdir);
    }

    #[test]
    fn auth_rejects_non_authenticate_message() {
        let (listener, socket_path, tmpdir) = temp_listener();

        let mut client = UnixStream::connect(&socket_path).expect("connect");
        let (mut server_stream, _) = listener.accept().expect("accept");

        // Send a CreateSession instead of Authenticate
        ipc_binary::write_frame(
            &mut client,
            &BinaryFrame::CreateSession {
                session_id: "1".into(),
                heap_limit_mb: 0,
                cpu_time_limit_ms: 0,
            },
        )
        .expect("write");

        assert!(!authenticate_connection(&mut server_stream, "any-token"));

        cleanup(&socket_path, &tmpdir);
    }

    #[test]
    fn auth_rejects_empty_connection() {
        let (listener, socket_path, tmpdir) = temp_listener();

        let client = UnixStream::connect(&socket_path).expect("connect");
        let (mut server_stream, _) = listener.accept().expect("accept");

        // Drop client immediately — server will get EOF
        drop(client);

        assert!(!authenticate_connection(&mut server_stream, "any-token"));

        cleanup(&socket_path, &tmpdir);
    }

    #[test]
    fn self_pipe_creation_and_wakeup() {
        let (read_fd, write_fd) = create_self_pipe().expect("create self-pipe");

        // Both FDs should have CLOEXEC
        let read_flags = unsafe { libc::fcntl(read_fd, libc::F_GETFD) };
        assert_ne!(read_flags & libc::FD_CLOEXEC, 0, "read end needs CLOEXEC");
        let write_flags = unsafe { libc::fcntl(write_fd, libc::F_GETFD) };
        assert_ne!(write_flags & libc::FD_CLOEXEC, 0, "write end needs CLOEXEC");

        // Both FDs should be non-blocking
        let read_fl = unsafe { libc::fcntl(read_fd, libc::F_GETFL) };
        assert_ne!(read_fl & libc::O_NONBLOCK, 0, "read end needs O_NONBLOCK");
        let write_fl = unsafe { libc::fcntl(write_fd, libc::F_GETFL) };
        assert_ne!(write_fl & libc::O_NONBLOCK, 0, "write end needs O_NONBLOCK");

        // Write to pipe should wake poll
        let b: u8 = 1;
        let n = unsafe { libc::write(write_fd, &b as *const u8 as *const libc::c_void, 1) };
        assert_eq!(n, 1);

        let mut pfd = libc::pollfd {
            fd: read_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ret = unsafe { libc::poll(&mut pfd, 1, 100) };
        assert_eq!(ret, 1, "poll should return ready");
        assert_ne!(pfd.revents & libc::POLLIN, 0);

        drain_pipe(read_fd);

        unsafe {
            libc::close(read_fd);
            libc::close(write_fd);
        }
    }

    #[test]
    fn poll_accept_wakes_on_connection() {
        let (listener, socket_path, tmpdir) = temp_listener();
        let listener_fd = listener.as_raw_fd();

        // Start a client connection in another thread
        let sp = socket_path.clone();
        let handle = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(10));
            UnixStream::connect(&sp).expect("connect")
        });

        // Poll the listener — should wake when client connects
        let mut pfd = libc::pollfd {
            fd: listener_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ret = unsafe { libc::poll(&mut pfd, 1, 2000) };
        assert!(ret > 0, "poll should return ready when client connects");
        assert_ne!(pfd.revents & libc::POLLIN, 0);

        let (_, _) = listener.accept().expect("accept after poll");
        let _client = handle.join().expect("client thread");

        cleanup(&socket_path, &tmpdir);
    }

    #[test]
    fn constant_time_eq_matches_equal_strings() {
        assert!(constant_time_eq(b"hello", b"hello"));
        assert!(constant_time_eq(b"", b""));
        assert!(constant_time_eq(b"abc123xyz", b"abc123xyz"));
    }

    #[test]
    fn constant_time_eq_rejects_different_strings() {
        assert!(!constant_time_eq(b"hello", b"world"));
        assert!(!constant_time_eq(b"hello", b"hellx"));
        // Single-bit difference
        assert!(!constant_time_eq(b"\x00", b"\x01"));
    }

    #[test]
    fn constant_time_eq_rejects_different_lengths() {
        assert!(!constant_time_eq(b"hello", b"hell"));
        assert!(!constant_time_eq(b"", b"x"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }

    #[test]
    fn socket_dir_has_0700_permissions() {
        let (tmpdir, socket_path) = create_socket_dir().expect("create socket dir");
        let meta = fs::metadata(&tmpdir).expect("stat tmpdir");
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o700,
            "socket dir should have 0700 permissions, got {:o}",
            mode
        );
        cleanup(&socket_path, &tmpdir);
    }

    #[test]
    fn poll_wakes_on_self_pipe_not_listener() {
        let (listener, socket_path, tmpdir) = temp_listener();
        let listener_fd = listener.as_raw_fd();
        let (sig_read, sig_write) = create_self_pipe().expect("self-pipe");

        // Write to signal pipe
        let b: u8 = 1;
        unsafe {
            libc::write(sig_write, &b as *const u8 as *const libc::c_void, 1);
        }

        let mut pollfds = [
            libc::pollfd {
                fd: listener_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: sig_read,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let ret = unsafe { libc::poll(pollfds.as_mut_ptr(), 2, 100) };
        assert!(ret > 0, "poll should wake");
        // Signal pipe should be readable, not listener
        assert_ne!(
            pollfds[1].revents & libc::POLLIN,
            0,
            "signal pipe should be ready"
        );
        assert_eq!(
            pollfds[0].revents & libc::POLLIN,
            0,
            "listener should not be ready"
        );

        drain_pipe(sig_read);
        unsafe {
            libc::close(sig_read);
            libc::close(sig_write);
        }
        cleanup(&socket_path, &tmpdir);
    }
}
