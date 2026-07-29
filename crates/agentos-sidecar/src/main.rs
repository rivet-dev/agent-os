use std::os::fd::{FromRawFd, OwnedFd};

use nix::fcntl::{fcntl, FcntlArg};
use tracing_subscriber::fmt::writer::BoxMakeWriter;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

const CONTROL_FD: i32 = 3;

fn main() {
    init_tracing();
    tracing::info!(target: "agentos_native_sidecar::perf", "sidecar process started");
    if env_flag("AGENTOS_SIDECAR_COMBINED_STDIO") {
        if let Err(error) = agentos_native_sidecar::stdio::run_combined_with_extensions(
            agentos_sidecar_wrapper::extensions(),
        ) {
            tracing::error!(?error, "agentos-sidecar startup failed");
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
    // response/control socket and transfers its sole ownership to this binary.
    // The fcntl probe above establishes that the descriptor is open.
    let control_fd = unsafe { OwnedFd::from_raw_fd(CONTROL_FD) };
    if let Err(error) = agentos_native_sidecar::stdio::run_with_extensions(
        agentos_sidecar_wrapper::extensions(),
        control_fd,
    ) {
        tracing::error!(?error, "agentos-sidecar startup failed");
        std::process::exit(1);
    }
}

/// `1` => true, anything else => false. Mirrors rivet's `env_flag`.
fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| v == "1")
}

/// Initialize tracing for the sidecar.
///
/// Mirrors the rivet logging setup (`rivetkit-napi::init_tracing`): an
/// `EnvFilter`-gated subscriber with a logfmt formatter, so log level and
/// verbosity are runtime-configurable instead of hardcoded.
///
/// Configuration (all optional):
/// - level: `AGENTOS_LOG_LEVEL` > `LOG_LEVEL` > `RUST_LOG` > `"info"`.
/// - format: `RUST_LOG_FORMAT=logfmt` (default) or `text`.
/// - sink: `AGENTOS_LOG_FILE` (append to that file) else stderr.
/// - field toggles: `RUST_LOG_{SPAN_NAME,SPAN_PATH,TARGET,LOCATION,MODULE_PATH,ANSI_COLOR}`.
///
/// The sink MUST be stderr or a file — NEVER stdout, which carries the
/// sidecar's binary frame protocol (see crates/CLAUDE.md: "Control channels
/// must be out-of-band").
fn init_tracing() {
    // Level priority: AGENTOS_LOG_LEVEL > LOG_LEVEL > RUST_LOG > "info".
    let directive = std::env::var("AGENTOS_LOG_LEVEL")
        .ok()
        .or_else(|| std::env::var("LOG_LEVEL").ok())
        .or_else(|| std::env::var("RUST_LOG").ok())
        .unwrap_or_else(|| "info".to_string());
    let env_filter = EnvFilter::try_new(&directive).unwrap_or_else(|_| EnvFilter::new("info"));

    // Sink: a file if AGENTOS_LOG_FILE is set, else stderr. Never stdout.
    let writer: BoxMakeWriter = match std::env::var("AGENTOS_LOG_FILE") {
        Ok(path) if !path.is_empty() => {
            let path = std::path::PathBuf::from(path);
            let dir = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            let name = path
                .file_name()
                .map(std::ffi::OsString::from)
                .unwrap_or_else(|| std::ffi::OsString::from("agentos-sidecar.log"));
            BoxMakeWriter::new(tracing_appender::rolling::never(dir, name))
        }
        _ => BoxMakeWriter::new(std::io::stderr),
    };

    let registry = tracing_subscriber::registry().with(env_filter);

    let text_format = std::env::var("RUST_LOG_FORMAT")
        .map(|v| v.eq_ignore_ascii_case("text"))
        .unwrap_or(false);

    if text_format {
        registry
            .with(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(writer),
            )
            .init();
    } else {
        registry
            .with(
                tracing_logfmt::builder()
                    .with_span_name(env_flag("RUST_LOG_SPAN_NAME"))
                    .with_span_path(env_flag("RUST_LOG_SPAN_PATH"))
                    .with_target(env_flag("RUST_LOG_TARGET"))
                    .with_location(env_flag("RUST_LOG_LOCATION"))
                    .with_module_path(env_flag("RUST_LOG_MODULE_PATH"))
                    .with_ansi_color(env_flag("RUST_LOG_ANSI_COLOR"))
                    .layer()
                    .with_writer(writer),
            )
            .init();
    }
}
