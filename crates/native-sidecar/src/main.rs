use std::os::fd::{FromRawFd, OwnedFd};

use nix::fcntl::{fcntl, FcntlArg};

const CONTROL_FD: i32 = 3;

fn main() {
    // Default to WARN so near-limit / backpressure warnings actually surface
    // (they were swallowed at ERROR-only); operators can tune via AGENTOS_LOG
    // (e.g. `error` to quiet, `debug` for queue snapshots). Logs MUST go to stderr:
    // stdout is the framed wire-protocol channel, so logging there would corrupt it.
    let level = std::env::var("AGENTOS_LOG")
        .ok()
        .and_then(|value| value.parse::<tracing::Level>().ok())
        .unwrap_or(tracing::Level::WARN);
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(level)
        .init();
    if std::env::args().nth(1).as_deref()
        == Some(agentos_execution::WASMTIME_THREAD_WORKER_ARGUMENT)
    {
        if let Err(error) = agentos_execution::run_wasmtime_thread_worker() {
            tracing::error!(code = %error.code, message = %error.message, "Wasmtime thread worker failed");
            std::process::exit(1);
        }
        return;
    }
    if let Err(error) = fcntl(CONTROL_FD, FcntlArg::F_GETFD) {
        tracing::error!(
            ?error,
            fd = CONTROL_FD,
            "missing inherited sidecar response/control descriptor"
        );
        std::process::exit(1);
    }
    // SAFETY: the process launch contract reserves fd 3 for the inherited
    // response/control socket and transfers its sole ownership to the sidecar.
    // The fcntl probe above establishes that the descriptor is open before it
    // is adopted.
    let control_fd = unsafe { OwnedFd::from_raw_fd(CONTROL_FD) };
    if let Err(error) = agentos_native_sidecar::stdio::run(control_fd) {
        tracing::error!(?error, "agentos-native-sidecar startup failed");
        std::process::exit(1);
    }
}
