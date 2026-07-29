//! Native floor for the differential perf harness.
//!
//! Runs one logical op many times, timing each with a monotonic clock, and emits
//! the raw per-iteration sample array (nanoseconds) as JSON to stdout. The TypeScript
//! harness reduces these samples with the SAME `stats()` it applies to the node and
//! guest layers, so the percentile math is identical across all three layers and the
//! "emulation tax" ratio is honest.
//!
//! Ops (held byte-identical to the node + guest layers):
//!   spawn_exit    -> /bin/sh -c 'exit 0'      (fork/posix_spawn + execve + reap)
//!   exec_capture  -> /bin/sh -c 'printf hi'   (same, plus stdout capture)
//!   fs_stat       -> stat a small host file
//!   fs_write      -> overwrite a small host file
//!   fs_read       -> read a 64 KiB host file
//!   dns_lookup    -> resolve localhost
//!   tcp_connect   -> localhost TCP connect+close
//!   tcp_echo      -> localhost TCP connect+echo
//!   pipe_echo     -> shell pipe echo through cat
//!   cpu_loop      -> bounded integer loop
//!   alloc_free    -> allocate/drop a 64 KiB Vec
//!
//! Usage: agentos-native-baseline --op spawn_exit|exec_capture --iters N --warmup W

use std::fs::File;
use std::io::{Read, Write};
#[cfg(not(target_family = "wasm"))]
use std::net::{TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
#[cfg(all(unix, not(target_family = "wasm")))]
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
#[cfg(not(target_family = "wasm"))]
use std::process::{Command, Stdio};
#[cfg(not(target_family = "wasm"))]
use std::thread;
#[cfg(target_family = "wasm")]
use std::time::SystemTime;
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
enum Op {
    SpawnExit,
    ExecCapture,
    NodeStdoutDiscard2b,
    NodeStdoutCapture2b,
    NodeStdoutListenerOnly2b,
    NodeExit,
    NodeFanout,
    NodeReapStorm,
    NodeStdinRoundtrip,
    PipeChain,
    FsStat,
    FsStatX32,
    FsWrite,
    FsRead,
    StreamCopy,
    FsOpenClose,
    FsMkdirRmdir,
    FsRename,
    FsReaddir,
    FsFsync,
    DnsLookup,
    DnsLookupX2,
    DnsConcurrent,
    TcpConnect,
    TcpEcho,
    UnixConnect,
    UnixEcho,
    HttpLoopbackGet,
    TcpConcurrent,
    TcpThroughput,
    TcpTinyWrites,
    UdpEcho,
    PipeEcho,
    PipeThroughput,
    PipeBackpressure,
    StdioWriteSync,
    CpuLoop,
    SleepTimer,
    YieldLoop,
    AllocFree,
}

struct BenchConfig {
    size_bytes: Option<usize>,
    entry_count: Option<usize>,
    timer_count: Option<usize>,
    sleep_ns: Option<u64>,
    chunk_count: Option<usize>,
}

const OP_NAMES: &[&str] = &[
    "spawn_exit",
    "exec_capture",
    "node_stdout_discard_2b",
    "node_stdout_capture_2b",
    "node_stdout_listener_only_2b",
    "node_exit",
    "node_fanout",
    "node_reap_storm",
    "node_stdin_roundtrip",
    "pipe_chain",
    "fs_stat",
    "fs_stat_x32",
    "fs_write",
    "fs_read",
    "stream_copy",
    "fs_open_close",
    "fs_mkdir_rmdir",
    "fs_rename",
    "fs_readdir",
    "fs_fsync",
    "dns_lookup",
    "dns_lookup_x2",
    "dns_concurrent",
    "tcp_connect",
    "tcp_echo",
    "unix_connect",
    "unix_echo",
    "http_loopback_get",
    "tcp_concurrent",
    "tcp_throughput",
    "tcp_tiny_writes",
    "udp_echo",
    "pipe_echo",
    "pipe_throughput",
    "pipe_backpressure",
    "stdio_write_sync",
    "cpu_loop",
    "sleep_timer",
    "yield_loop",
    "alloc_free",
];

impl Op {
    #[cfg(target_family = "wasm")]
    fn supported_on_wasm(self) -> bool {
        matches!(
            self,
            Op::FsStat
                | Op::FsStatX32
                | Op::FsWrite
                | Op::FsRead
                | Op::StreamCopy
                | Op::FsOpenClose
                | Op::FsMkdirRmdir
                | Op::FsRename
                | Op::FsReaddir
                | Op::FsFsync
                | Op::CpuLoop
                | Op::SleepTimer
                | Op::YieldLoop
                | Op::AllocFree
        )
    }
}

fn parse_op(name: &str) -> Option<Op> {
    Some(match name {
        "spawn_exit" => Op::SpawnExit,
        "exec_capture" => Op::ExecCapture,
        "node_stdout_discard_2b" => Op::NodeStdoutDiscard2b,
        "node_stdout_capture_2b" => Op::NodeStdoutCapture2b,
        "node_stdout_listener_only_2b" => Op::NodeStdoutListenerOnly2b,
        "node_exit" => Op::NodeExit,
        "node_fanout" => Op::NodeFanout,
        "node_reap_storm" => Op::NodeReapStorm,
        "node_stdin_roundtrip" => Op::NodeStdinRoundtrip,
        "pipe_chain" => Op::PipeChain,
        "fs_stat" => Op::FsStat,
        "fs_stat_x32" => Op::FsStatX32,
        "fs_write" => Op::FsWrite,
        "fs_read" => Op::FsRead,
        "stream_copy" => Op::StreamCopy,
        "fs_open_close" => Op::FsOpenClose,
        "fs_mkdir_rmdir" => Op::FsMkdirRmdir,
        "fs_rename" => Op::FsRename,
        "fs_readdir" => Op::FsReaddir,
        "fs_fsync" => Op::FsFsync,
        "dns_lookup" => Op::DnsLookup,
        "dns_lookup_x2" => Op::DnsLookupX2,
        "dns_concurrent" => Op::DnsConcurrent,
        "tcp_connect" => Op::TcpConnect,
        "tcp_echo" => Op::TcpEcho,
        "unix_connect" => Op::UnixConnect,
        "unix_echo" => Op::UnixEcho,
        "http_loopback_get" => Op::HttpLoopbackGet,
        "tcp_concurrent" => Op::TcpConcurrent,
        "tcp_throughput" => Op::TcpThroughput,
        "tcp_tiny_writes" => Op::TcpTinyWrites,
        "udp_echo" => Op::UdpEcho,
        "pipe_echo" => Op::PipeEcho,
        "pipe_throughput" => Op::PipeThroughput,
        "pipe_backpressure" => Op::PipeBackpressure,
        "stdio_write_sync" => Op::StdioWriteSync,
        "cpu_loop" => Op::CpuLoop,
        "sleep_timer" => Op::SleepTimer,
        "yield_loop" => Op::YieldLoop,
        "alloc_free" => Op::AllocFree,
        _ => return None,
    })
}

fn op_name(op: Op) -> &'static str {
    match op {
        Op::SpawnExit => "spawn_exit",
        Op::ExecCapture => "exec_capture",
        Op::NodeStdoutDiscard2b => "node_stdout_discard_2b",
        Op::NodeStdoutCapture2b => "node_stdout_capture_2b",
        Op::NodeStdoutListenerOnly2b => "node_stdout_listener_only_2b",
        Op::NodeExit => "node_exit",
        Op::NodeFanout => "node_fanout",
        Op::NodeReapStorm => "node_reap_storm",
        Op::NodeStdinRoundtrip => "node_stdin_roundtrip",
        Op::PipeChain => "pipe_chain",
        Op::FsStat => "fs_stat",
        Op::FsStatX32 => "fs_stat_x32",
        Op::FsWrite => "fs_write",
        Op::FsRead => "fs_read",
        Op::StreamCopy => "stream_copy",
        Op::FsOpenClose => "fs_open_close",
        Op::FsMkdirRmdir => "fs_mkdir_rmdir",
        Op::FsRename => "fs_rename",
        Op::FsReaddir => "fs_readdir",
        Op::FsFsync => "fs_fsync",
        Op::DnsLookup => "dns_lookup",
        Op::DnsLookupX2 => "dns_lookup_x2",
        Op::DnsConcurrent => "dns_concurrent",
        Op::TcpConnect => "tcp_connect",
        Op::TcpEcho => "tcp_echo",
        Op::UnixConnect => "unix_connect",
        Op::UnixEcho => "unix_echo",
        Op::HttpLoopbackGet => "http_loopback_get",
        Op::TcpConcurrent => "tcp_concurrent",
        Op::TcpThroughput => "tcp_throughput",
        Op::TcpTinyWrites => "tcp_tiny_writes",
        Op::UdpEcho => "udp_echo",
        Op::PipeEcho => "pipe_echo",
        Op::PipeThroughput => "pipe_throughput",
        Op::PipeBackpressure => "pipe_backpressure",
        Op::StdioWriteSync => "stdio_write_sync",
        Op::CpuLoop => "cpu_loop",
        Op::SleepTimer => "sleep_timer",
        Op::YieldLoop => "yield_loop",
        Op::AllocFree => "alloc_free",
    }
}

#[cfg(not(target_family = "wasm"))]
type Timer = Instant;

#[cfg(not(target_family = "wasm"))]
fn timer_start() -> Timer {
    Instant::now()
}

#[cfg(not(target_family = "wasm"))]
fn elapsed_ns(timer: Timer) -> u128 {
    timer.elapsed().as_nanos()
}

#[cfg(target_family = "wasm")]
struct Timer {
    instant: Option<Instant>,
    system: SystemTime,
}

#[cfg(target_family = "wasm")]
fn timer_start() -> Timer {
    Timer {
        instant: std::panic::catch_unwind(Instant::now).ok(),
        system: SystemTime::now(),
    }
}

#[cfg(target_family = "wasm")]
fn elapsed_ns(timer: Timer) -> u128 {
    if let Some(instant) = timer.instant {
        if let Ok(elapsed) = std::panic::catch_unwind(|| instant.elapsed()) {
            return elapsed.as_nanos();
        }
    }
    timer.system.elapsed().map(|d| d.as_nanos()).unwrap_or(0)
}

fn run_once(op: Op, iter: usize, base_dir: &Path, config: &BenchConfig) {
    match op {
        #[cfg(not(target_family = "wasm"))]
        Op::SpawnExit => {
            let status = Command::new("/bin/sh")
                .args(["-c", "exit 0"])
                .status()
                .expect("spawn /bin/sh failed");
            assert!(status.success(), "expected exit 0, got {status:?}");
        }
        #[cfg(not(target_family = "wasm"))]
        Op::ExecCapture => {
            let out = Command::new("/bin/sh")
                .args(["-c", "printf hi"])
                .output()
                .expect("spawn /bin/sh failed");
            assert_eq!(out.stdout, b"hi", "unexpected stdout");
        }
        #[cfg(not(target_family = "wasm"))]
        Op::NodeStdoutDiscard2b => {
            let size_bytes = config.size_bytes.unwrap_or(2);
            let script = format!("process.stdout.write(Buffer.alloc({size_bytes}, 55))");
            let status = Command::new("node")
                .arg("-e")
                .arg(&script)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .expect("spawn node failed");
            assert!(status.success(), "expected exit 0, got {status:?}");
        }
        #[cfg(not(target_family = "wasm"))]
        Op::NodeStdoutCapture2b => {
            let size_bytes = config.size_bytes.unwrap_or(2);
            let script = format!("process.stdout.write(Buffer.alloc({size_bytes}, 55))");
            let out = Command::new("node")
                .arg("-e")
                .arg(&script)
                .output()
                .expect("spawn node failed");
            assert!(
                out.status.success(),
                "expected exit 0, got {:?}",
                out.status
            );
            assert_eq!(out.stdout.len(), size_bytes, "unexpected stdout byte count");
        }
        #[cfg(not(target_family = "wasm"))]
        Op::NodeStdoutListenerOnly2b => {
            let size_bytes = config.size_bytes.unwrap_or(2);
            let script = format!("process.stdout.write(Buffer.alloc({size_bytes}, 55))");
            let mut child = Command::new("node")
                .arg("-e")
                .arg(&script)
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn node failed");
            let mut stdout = child.stdout.take().expect("stdout pipe");
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).expect("read stdout");
            let status = child.wait().expect("wait node child");
            assert!(status.success(), "expected exit 0, got {status:?}");
            assert_eq!(bytes.len(), size_bytes, "unexpected stdout byte count");
        }
        // Real host node process that immediately exits. This is the apples-to-apples
        // floor for the guest layer, where the same logical op spins a V8 isolate.
        #[cfg(not(target_family = "wasm"))]
        Op::NodeExit => {
            let status = Command::new("node")
                .args(["-e", "process.exit(0)"])
                .status()
                .expect("spawn node failed");
            assert!(status.success(), "expected exit 0, got {status:?}");
        }
        #[cfg(not(target_family = "wasm"))]
        Op::NodeFanout | Op::NodeReapStorm => {
            let mut children = Vec::new();
            for _ in 0..8 {
                children.push(
                    Command::new("node")
                        .args(["-e", "process.exit(0)"])
                        .spawn()
                        .expect("spawn node failed"),
                );
            }
            for mut child in children {
                let status = child.wait().expect("wait node child");
                assert!(status.success(), "expected exit 0, got {status:?}");
            }
        }
        #[cfg(not(target_family = "wasm"))]
        Op::NodeStdinRoundtrip => {
            let size_bytes = config.size_bytes.unwrap_or(4096);
            let payload = vec![9_u8; size_bytes];
            let mut child = Command::new("node")
                .args([
                    "-e",
                    "process.stdin.on('data', (chunk) => process.stdout.write(chunk)); process.stdin.on('end', () => process.exit(0));",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn node failed");
            child
                .stdin
                .take()
                .expect("child stdin")
                .write_all(&payload)
                .expect("write child stdin");
            let out = child.wait_with_output().expect("wait node child");
            assert!(
                out.status.success(),
                "expected exit 0, got {:?}",
                out.status
            );
            assert_eq!(out.stdout, payload, "stdin roundtrip mismatch");
        }
        #[cfg(not(target_family = "wasm"))]
        Op::PipeChain => {
            let out = Command::new("/bin/sh")
                .args(["-c", "printf hello | cat | cat >/dev/null"])
                .output()
                .expect("run pipe chain");
            assert!(out.status.success(), "pipe chain failed: {out:?}");
        }
        Op::FsStat => {
            let path = base_dir.join("agentos-native-fs-stat.txt");
            std::fs::write(&path, b"hi").expect("write stat fixture");
            let meta = std::fs::metadata(&path).expect("stat fixture");
            assert!(meta.len() >= 2);
        }
        Op::FsStatX32 => {
            let path = base_dir.join("agentos-native-fs-stat-x32.txt");
            if !path.exists() {
                std::fs::write(&path, b"hi").expect("write stat fixture");
            }
            // Every lane must measure the SAME work (32 stats): under wasm the
            // post-quota-fix batch runs faster than the guest clock granularity,
            // so samples may read 0-1ms quantized — that is the truthful value;
            // never batch one lane harder than the others.
            let batches = config.chunk_count.unwrap_or(1);
            for _ in 0..batches {
                for _ in 0..32 {
                    let meta = std::fs::metadata(&path).expect("stat fixture");
                    assert!(meta.len() >= 2);
                }
            }
        }
        Op::FsWrite => {
            let path = base_dir.join("agentos-native-fs-write.txt");
            if let Some(size_bytes) = config.size_bytes {
                std::fs::write(path, vec![(iter & 255) as u8; size_bytes]).expect("write fixture");
            } else {
                std::fs::write(path, format!("hello-{iter:08}")).expect("write fixture");
            }
        }
        Op::FsRead => {
            let size_bytes = config.size_bytes.unwrap_or(64 * 1024);
            let path = base_dir.join("agentos-native-fs-read.bin");
            let rewrite = std::fs::metadata(&path)
                .map(|meta| meta.len() != size_bytes as u64)
                .unwrap_or(true);
            if rewrite {
                std::fs::write(&path, vec![7_u8; size_bytes]).expect("write read fixture");
            }
            let data = std::fs::read(path).expect("read fixture");
            assert_eq!(data.len(), size_bytes);
        }
        Op::StreamCopy => {
            let size_bytes = config.size_bytes.unwrap_or(64 * 1024);
            let src = base_dir.join(format!("agentos-native-stream-copy-src-{size_bytes}.bin"));
            let dst = base_dir.join(format!(
                "agentos-native-stream-copy-dst-{size_bytes}-{iter}.bin"
            ));
            let rewrite = std::fs::metadata(&src)
                .map(|meta| meta.len() != size_bytes as u64)
                .unwrap_or(true);
            if rewrite {
                std::fs::write(&src, vec![7_u8; size_bytes]).expect("write stream source");
            }
            let mut input = File::open(&src).expect("open stream source");
            let mut output = File::create(&dst).expect("create stream destination");
            let mut copied = 0_usize;
            let mut buf = vec![0_u8; 16 * 1024];
            loop {
                let n = input.read(&mut buf).expect("read stream source");
                if n == 0 {
                    break;
                }
                output
                    .write_all(&buf[..n])
                    .expect("write stream destination");
                copied += n;
            }
            drop(output);
            let meta = std::fs::metadata(&dst).expect("stat stream destination");
            std::fs::remove_file(&dst).expect("remove stream destination");
            assert_eq!(copied, size_bytes);
            assert_eq!(meta.len(), size_bytes as u64);
        }
        Op::FsOpenClose => {
            let path = base_dir.join("agentos-native-fs-open-close.txt");
            std::fs::write(&path, b"hi").expect("write open fixture");
            let file = File::open(path).expect("open fixture");
            drop(file);
        }
        Op::FsMkdirRmdir => {
            let path = base_dir.join(format!("agentos-native-dir-{iter}"));
            std::fs::create_dir(&path).expect("create dir");
            std::fs::remove_dir(&path).expect("remove dir");
        }
        Op::FsRename => {
            let from = base_dir.join(format!("agentos-native-rename-{iter}.a"));
            let to = base_dir.join(format!("agentos-native-rename-{iter}.b"));
            std::fs::write(&from, b"hi").expect("write rename fixture");
            std::fs::rename(&from, &to).expect("rename fixture");
            std::fs::remove_file(&to).expect("remove rename fixture");
        }
        Op::FsReaddir => {
            let entry_count = config.entry_count.unwrap_or(32);
            let dir = base_dir.join("agentos-native-readdir");
            std::fs::create_dir_all(&dir).expect("create readdir dir");
            let marker = dir.join(format!(".fixture-ready-{entry_count}"));
            if !marker.exists() {
                for i in 0..entry_count {
                    let path = dir.join(format!("{i}.txt"));
                    if !path.exists() {
                        std::fs::write(&path, b"hi").expect("write readdir fixture");
                    }
                }
                std::fs::write(&marker, b"ready").expect("write readdir fixture marker");
            }
            let count = std::fs::read_dir(dir).expect("read dir").count();
            assert!(count > entry_count);
        }
        Op::FsFsync => {
            let path = base_dir.join("agentos-native-fsync.txt");
            let mut file = File::create(path).expect("create fsync fixture");
            file.write_all(b"hello").expect("write fsync fixture");
            file.sync_all().expect("fsync fixture");
        }
        #[cfg(not(target_family = "wasm"))]
        Op::DnsLookup => {
            let addrs: Vec<_> = ("localhost", 80)
                .to_socket_addrs()
                .expect("resolve localhost")
                .collect();
            assert!(!addrs.is_empty());
        }
        #[cfg(not(target_family = "wasm"))]
        Op::DnsLookupX2 => {
            for _ in 0..2 {
                let addrs: Vec<_> = ("localhost", 80)
                    .to_socket_addrs()
                    .expect("resolve localhost")
                    .collect();
                assert!(!addrs.is_empty());
            }
        }
        #[cfg(not(target_family = "wasm"))]
        Op::DnsConcurrent => {
            let threads: Vec<_> = (0..4)
                .map(|_| {
                    thread::spawn(|| {
                        let addrs: Vec<_> = ("localhost", 80)
                            .to_socket_addrs()
                            .expect("resolve localhost")
                            .collect();
                        assert!(!addrs.is_empty());
                    })
                })
                .collect();
            for handle in threads {
                handle.join().expect("join resolver");
            }
        }
        #[cfg(not(target_family = "wasm"))]
        Op::TcpConnect => {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind tcp listener");
            let addr = listener.local_addr().expect("listener addr");
            let server = thread::spawn(move || {
                let _ = listener.accept().expect("accept tcp connect");
            });
            let _stream = TcpStream::connect(addr).expect("connect tcp listener");
            server.join().expect("join tcp server");
        }
        #[cfg(not(target_family = "wasm"))]
        Op::TcpEcho => {
            let payload = vec![7_u8; config.size_bytes.unwrap_or(5)];
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind tcp listener");
            let addr = listener.local_addr().expect("listener addr");
            let expected_len = payload.len();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept tcp echo");
                let mut buf = vec![0_u8; expected_len];
                stream.read_exact(&mut buf).expect("read tcp echo");
                stream.write_all(&buf).expect("write tcp echo");
            });
            let mut stream = TcpStream::connect(addr).expect("connect tcp echo");
            stream.write_all(&payload).expect("write client echo");
            let mut buf = vec![0_u8; payload.len()];
            stream.read_exact(&mut buf).expect("read client echo");
            assert_eq!(buf, payload);
            server.join().expect("join tcp server");
        }
        #[cfg(all(unix, not(target_family = "wasm")))]
        Op::UnixConnect => {
            let sock = base_dir.join(format!("agentos-native-unix-connect-{iter}.sock"));
            let _ = std::fs::remove_file(&sock);
            let listener = UnixListener::bind(&sock).expect("bind unix listener");
            let server = thread::spawn(move || {
                let (_stream, _) = listener.accept().expect("accept unix connect");
            });
            let stream = UnixStream::connect(&sock).expect("connect unix listener");
            drop(stream);
            server.join().expect("join unix server");
            let _ = std::fs::remove_file(&sock);
        }
        #[cfg(all(unix, not(target_family = "wasm")))]
        Op::UnixEcho => {
            let payload = vec![7_u8; config.size_bytes.unwrap_or(16)];
            let sock = base_dir.join(format!("agentos-native-unix-echo-{iter}.sock"));
            let _ = std::fs::remove_file(&sock);
            let listener = UnixListener::bind(&sock).expect("bind unix listener");
            let expected_len = payload.len();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept unix echo");
                let mut buf = vec![0_u8; expected_len];
                stream.read_exact(&mut buf).expect("read unix echo");
                stream.write_all(&buf).expect("write unix echo");
            });
            let mut stream = UnixStream::connect(&sock).expect("connect unix echo");
            stream.write_all(&payload).expect("write client unix echo");
            let mut buf = vec![0_u8; payload.len()];
            stream.read_exact(&mut buf).expect("read client unix echo");
            assert_eq!(buf, payload);
            server.join().expect("join unix server");
            let _ = std::fs::remove_file(&sock);
        }
        #[cfg(not(target_family = "wasm"))]
        Op::HttpLoopbackGet => {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind http listener");
            let addr = listener.local_addr().expect("listener addr");
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept http");
                let mut request = Vec::new();
                let mut byte = [0_u8; 1];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).expect("read http request");
                    request.push(byte[0]);
                }
                assert!(
                    request.starts_with(b"GET / HTTP/1.1\r\n"),
                    "unexpected HTTP request"
                );
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    )
                    .expect("write http response");
            });
            let mut stream = TcpStream::connect(addr).expect("connect http listener");
            stream
                .write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
                .expect("write http request");
            let mut response = Vec::new();
            stream
                .read_to_end(&mut response)
                .expect("read http response");
            assert!(
                response.ends_with(b"\r\n\r\nok"),
                "unexpected HTTP response body"
            );
            server.join().expect("join http server");
        }
        #[cfg(not(target_family = "wasm"))]
        Op::TcpConcurrent => {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind tcp listener");
            let addr = listener.local_addr().expect("listener addr");
            let server = thread::spawn(move || {
                for _ in 0..4 {
                    let (mut stream, _) = listener.accept().expect("accept tcp connect");
                    let mut buf = [0_u8; 1];
                    let _ = stream.read(&mut buf);
                }
            });
            let mut clients = Vec::new();
            for _ in 0..4 {
                clients.push(TcpStream::connect(addr).expect("connect tcp listener"));
            }
            for mut client in clients {
                client.write_all(b"x").expect("write connect byte");
            }
            server.join().expect("join tcp server");
        }
        #[cfg(not(target_family = "wasm"))]
        Op::TcpThroughput => {
            let payload = vec![7_u8; config.size_bytes.unwrap_or(64 * 1024)];
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind tcp listener");
            let addr = listener.local_addr().expect("listener addr");
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept tcp throughput");
                let mut buf = vec![0_u8; 64 * 1024];
                stream.read_exact(&mut buf).expect("read tcp throughput");
                stream.write_all(&buf).expect("write tcp throughput");
            });
            let mut stream = TcpStream::connect(addr).expect("connect tcp throughput");
            stream.write_all(&payload).expect("write client throughput");
            let mut out = vec![0_u8; payload.len()];
            stream.read_exact(&mut out).expect("read client throughput");
            assert_eq!(out.len(), payload.len());
            server.join().expect("join tcp server");
        }
        #[cfg(not(target_family = "wasm"))]
        Op::TcpTinyWrites => {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind tcp listener");
            let addr = listener.local_addr().expect("listener addr");
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept tcp tiny");
                let mut buf = [0_u8; 16];
                stream.read_exact(&mut buf).expect("read tcp tiny");
                stream.write_all(&buf).expect("write tcp tiny");
            });
            let mut stream = TcpStream::connect(addr).expect("connect tcp tiny");
            for _ in 0..16 {
                stream.write_all(b"x").expect("write tiny byte");
            }
            let mut out = [0_u8; 16];
            stream.read_exact(&mut out).expect("read tiny echo");
            server.join().expect("join tcp server");
        }
        #[cfg(not(target_family = "wasm"))]
        Op::UdpEcho => {
            let payload = vec![7_u8; config.size_bytes.unwrap_or(5)];
            let server = UdpSocket::bind("127.0.0.1:0").expect("bind udp server");
            let addr = server.local_addr().expect("udp addr");
            let expected_len = payload.len();
            let handle = thread::spawn(move || {
                let mut buf = vec![0_u8; expected_len];
                let (n, peer) = server.recv_from(&mut buf).expect("recv udp");
                server.send_to(&buf[..n], peer).expect("send udp");
            });
            let client = UdpSocket::bind("127.0.0.1:0").expect("bind udp client");
            client.send_to(&payload, addr).expect("send udp client");
            let mut buf = vec![0_u8; payload.len()];
            let (n, _) = client.recv_from(&mut buf).expect("recv udp client");
            assert_eq!(n, payload.len());
            assert_eq!(buf, payload);
            handle.join().expect("join udp server");
        }
        #[cfg(not(target_family = "wasm"))]
        Op::PipeEcho => {
            let out = Command::new("/bin/sh")
                .args(["-c", "printf hello | cat >/dev/null"])
                .output()
                .expect("run pipe echo");
            assert!(out.status.success(), "pipe command failed: {out:?}");
        }
        #[cfg(not(target_family = "wasm"))]
        Op::PipeThroughput | Op::PipeBackpressure => {
            #[cfg(unix)]
            {
                let (mut left, mut right) = UnixStream::pair().expect("unix stream pair");
                let payload = vec![9_u8; config.size_bytes.unwrap_or(64 * 1024)];
                let expected_len = payload.len();
                let reader = thread::spawn(move || {
                    let mut out = vec![0_u8; expected_len];
                    right.read_exact(&mut out).expect("pipe read");
                    out
                });
                left.write_all(&payload).expect("pipe write");
                let out = reader.join().expect("join pipe reader");
                assert_eq!(out.len(), payload.len());
            }
            #[cfg(not(unix))]
            {
                let out = Command::new("/bin/sh")
                    .args(["-c", "printf hello | cat >/dev/null"])
                    .output()
                    .expect("run pipe fallback");
                assert!(out.status.success(), "pipe fallback failed: {out:?}");
            }
        }
        Op::StdioWriteSync => {
            let size_bytes = config.size_bytes.unwrap_or(64 * 1024);
            let chunk_count = config.chunk_count.unwrap_or(8);
            let payload = vec![7_u8; size_bytes];
            #[cfg(not(target_family = "wasm"))]
            let mut sink: Box<dyn Write> = Box::new(
                std::fs::OpenOptions::new()
                    .write(true)
                    .open("/dev/null")
                    .expect("open /dev/null"),
            );
            #[cfg(target_family = "wasm")]
            let mut sink: Box<dyn Write> = Box::new(std::io::sink());
            for _ in 0..chunk_count {
                sink.write_all(&payload).expect("write stdio payload");
            }
            sink.flush().expect("flush stdio payload");
        }
        Op::CpuLoop => {
            let mut acc = 0_u64;
            for i in 0..2_000_000_u64 {
                acc = acc.wrapping_add(i ^ (acc.rotate_left(7)));
            }
            std::hint::black_box(acc);
        }
        Op::SleepTimer => {
            let count = config.timer_count.unwrap_or(50);
            let sleep_ns = config.sleep_ns.unwrap_or(1_000_000);
            let duration = Duration::from_nanos(sleep_ns);
            for _ in 0..count {
                std::thread::sleep(duration);
            }
        }
        Op::YieldLoop => {
            let count = config.timer_count.unwrap_or(1000);
            for _ in 0..count {
                // This is the closest native analogue to setImmediate cadence: it
                // yields scheduler turn-taking, but it does not model JS task queues.
                std::thread::yield_now();
            }
        }
        Op::AllocFree => {
            let mut data = vec![0_u8; 4 * 1024 * 1024];
            for (i, byte) in data.iter_mut().enumerate() {
                *byte = (i % 251) as u8;
            }
            std::hint::black_box(data);
        }
        #[cfg(target_family = "wasm")]
        _ => unreachable!("unsupported wasm op checked before execution"),
    }
}

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

#[cfg(not(target_family = "wasm"))]
fn run_node_exit_phases() -> Vec<(&'static str, u128)> {
    let total_start = timer_start();
    let spawn_start = timer_start();
    let mut child = Command::new("node")
        .args(["-e", "process.exit(0)"])
        .spawn()
        .expect("spawn node failed");
    let spawn_ns = elapsed_ns(spawn_start);

    let wait_start = timer_start();
    let status = child.wait().expect("wait node child");
    let wait_ns = elapsed_ns(wait_start);
    assert!(status.success(), "expected exit 0, got {status:?}");

    vec![
        ("total", elapsed_ns(total_start)),
        ("spawn", spawn_ns),
        ("wait_reap", wait_ns),
    ]
}

#[cfg(not(target_family = "wasm"))]
fn run_node_fanout_phases() -> Vec<(&'static str, u128)> {
    let total_start = timer_start();
    let spawn_start = timer_start();
    let mut children = Vec::new();
    for _ in 0..8 {
        children.push(
            Command::new("node")
                .args(["-e", "process.exit(0)"])
                .spawn()
                .expect("spawn node failed"),
        );
    }
    let spawn_ns = elapsed_ns(spawn_start);

    let wait_start = timer_start();
    for mut child in children {
        let status = child.wait().expect("wait node child");
        assert!(status.success(), "expected exit 0, got {status:?}");
    }
    let wait_ns = elapsed_ns(wait_start);

    vec![
        ("total", elapsed_ns(total_start)),
        ("spawn_batch", spawn_ns),
        ("wait_reap_batch", wait_ns),
    ]
}

#[cfg(not(target_family = "wasm"))]
fn run_phases_once(op: Op) -> Option<Vec<(&'static str, u128)>> {
    match op {
        Op::NodeExit => Some(run_node_exit_phases()),
        Op::NodeFanout | Op::NodeReapStorm => Some(run_node_fanout_phases()),
        _ => None,
    }
}

#[cfg(target_family = "wasm")]
fn run_phases_once(_op: Op) -> Option<Vec<(&'static str, u128)>> {
    None
}

fn write_phase_json(op_name: &str, samples: &[(String, Vec<u128>)]) {
    let mut out = String::with_capacity(1024);
    out.push_str("{\"layer\":\"native\",\"op\":\"");
    out.push_str(op_name);
    out.push_str("\",\"unit\":\"ns\",\"phases\":{");
    for (phase_index, (phase, values)) in samples.iter().enumerate() {
        if phase_index > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(phase);
        out.push_str("\":[");
        for (sample_index, value) in values.iter().enumerate() {
            if sample_index > 0 {
                out.push(',');
            }
            out.push_str(&value.to_string());
        }
        out.push(']');
    }
    out.push_str("}}");
    println!("{out}");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|arg| arg == "--list-ops") {
        for name in OP_NAMES {
            println!("{name}");
        }
        return;
    }

    let op_arg = arg_value(&args, "--op");
    let op = match op_arg.as_deref().and_then(parse_op) {
        Some(op) => op,
        None => {
            eprintln!("unknown --op {:?}", op_arg.as_deref());
            std::process::exit(2);
        }
    };
    let op_name = op_name(op);
    #[cfg(target_family = "wasm")]
    if !op.supported_on_wasm() {
        println!("{{\"unsupported\":true,\"op\":\"{op_name}\"}}");
        return;
    }
    let iters: usize = arg_value(&args, "--iters")
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);
    let warmup: usize = arg_value(&args, "--warmup")
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);
    let base_dir = if let Some(path) = arg_value(&args, "--base-dir") {
        let path = std::path::PathBuf::from(path);
        if !path.exists() {
            std::fs::create_dir_all(&path).expect("create base dir");
        }
        path
    } else {
        std::env::temp_dir()
    };
    let config = BenchConfig {
        size_bytes: arg_value(&args, "--size-bytes").and_then(|s| s.parse().ok()),
        entry_count: arg_value(&args, "--entry-count").and_then(|s| s.parse().ok()),
        timer_count: arg_value(&args, "--timer-count").and_then(|s| s.parse().ok()),
        sleep_ns: arg_value(&args, "--sleep-ns").and_then(|s| s.parse().ok()),
        chunk_count: arg_value(&args, "--chunk-count").and_then(|s| s.parse().ok()),
    };
    let phases = args.iter().any(|arg| arg == "--phases");

    let total = warmup + iters;
    if phases {
        let Some(first) = run_phases_once(op) else {
            eprintln!("--phases is not supported for --op {op_name}");
            std::process::exit(2);
        };
        let mut phase_samples = first
            .into_iter()
            .map(|(name, _)| (name.to_string(), Vec::with_capacity(iters)))
            .collect::<Vec<_>>();
        for i in 0..total {
            let phase_values = run_phases_once(op).expect("checked phase support");
            if i >= warmup {
                for (phase_name, value) in phase_values {
                    if let Some((_, values)) = phase_samples
                        .iter_mut()
                        .find(|(name, _)| name == phase_name)
                    {
                        values.push(value);
                    }
                }
            }
        }
        write_phase_json(op_name, &phase_samples);
        return;
    }

    let mut samples: Vec<u128> = Vec::with_capacity(iters);
    for i in 0..total {
        let t = timer_start();
        run_once(op, i, &base_dir, &config);
        let ns = elapsed_ns(t);
        if i >= warmup {
            samples.push(ns);
        }
    }

    // Hand-built JSON (no serde dep): {"layer":"native","op":..,"unit":"ns","samples":[..]}
    let mut out = String::with_capacity(samples.len() * 8 + 64);
    out.push_str("{\"layer\":\"native\",\"op\":\"");
    out.push_str(op_name);
    out.push_str("\",\"unit\":\"ns\",\"samples\":[");
    for (i, s) in samples.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&s.to_string());
    }
    out.push_str("]}");
    println!("{out}");
}
