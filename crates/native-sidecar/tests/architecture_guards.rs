//! Architecture / boundary guards (CI hardening, item #2).
//!
//! This is a *chokepoint lint*: it scans the AgentOS Rust source tree and
//! FAILS if a security-sensitive host API ("banned API") appears OUTSIDE an
//! explicit allowlist of sanctioned modules. The goal is to keep host access
//! funnelled through a small, reviewable set of files so that a NEW use of
//! `std::fs`, raw sockets, `Command::new`, or process-environment reads cannot
//! be introduced without either landing in a sanctioned module or consciously
//! updating this allowlist (which forces review of the boundary).
//!
//! The four banned classes mirror the kernel/sidecar trust boundary:
//!
//!   * fs      -- `std::fs` / `tokio::fs` / `File::open` / `File::create` /
//!     `OpenOptions` / raw `openat`. Sanctioned only in the sidecar host-FS
//!     plumbing, the VFS-backed runtime modules, and runtime asset/module
//!     loaders.
//!   * net     -- `std::net` / `tokio::net` socket constructors, `reqwest`,
//!     `hyper`, `to_socket_addrs`, `UnixStream::pair`. Sanctioned only in the
//!     kernel DNS/socket plane, the sidecar host-net chokepoint
//!     (`sidecar::execution`), the embedded V8 runtime IPC pair, and
//!     host-backed storage plugins.
//!   * process -- `std::process::Command` / `tokio::process` / OS `fork`.
//!     Sanctioned only where agentos spawns its own helper process (the
//!     client transport that launches the sidecar). Guest "process" spawns are
//!     dispatched through the kernel `CommandDriver` registry and never touch
//!     `Command::new`.
//!   * env     -- `std::env::var` / `var_os` / `vars`. Sanctioned only at the
//!     scrubbed env-assembly / bootstrap points that read host configuration
//!     before a VM is constructed.
//!
//! IMPORTANT MAINTENANCE NOTES
//! ---------------------------
//! * The allowlist is built from the CURRENT legitimate uses so the test is
//!   GREEN today; it is designed to catch only *new* uses.
//! * Build scripts (`build.rs`, `*_build_support.rs`, ...), `tests/` and
//!   `benches/` directories, and inline `#[cfg(test)]` modules are excluded
//!   from the scan (they are not production host-access surface).
//! * `crates/execution/src/benchmark.rs`, `crates/execution/src/bin/`, and
//!   `crates/native-baseline/` hold benchmarking/dev tooling and are excluded
//!   for the same reason.
//!
//! If you are adding a genuinely new sanctioned chokepoint, add its
//! repo-relative path to the relevant allowlist below WITH a comment
//! explaining why the host access is safe. If you are adding host access
//! anywhere else, route it through an existing chokepoint instead.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Repo root = `<root>/crates/native-sidecar` -> up two levels.
fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .expect("sidecar crate should live two levels under the repo root")
        .to_path_buf()
}

#[test]
fn managed_v8_filesystem_state_is_kernel_authoritative() {
    let root = repo_root();
    let runner =
        std::fs::read_to_string(root.join("crates/execution/assets/runners/wasm-runner.mjs"))
            .expect("read managed WASM runner");
    let wasi = std::fs::read_to_string(root.join("crates/execution/assets/runners/wasi-module.js"))
        .expect("read WASI module");
    let filesystem = std::fs::read_to_string(root.join("crates/native-sidecar/src/filesystem.rs"))
        .expect("read sidecar filesystem service");
    let rpc =
        std::fs::read_to_string(root.join("crates/native-sidecar/src/execution/javascript/rpc.rs"))
            .expect("read JavaScript process RPC service");

    for stale_shadow in [
        "hostFsSizeByGuestPath",
        "rememberHostFsSize",
        "rememberedHostFsSize",
        "forgetHostFsSize",
    ] {
        assert!(
            !runner.contains(stale_shadow),
            "managed runner must not retain mutable path shadow {stale_shadow}"
        );
    }
    assert!(
        runner.contains(
            "if (!Number.isFinite(nextSize) || nextSize < 0) {\n        return WASI_ERRNO_INVAL;"
        ),
        "host ftruncate must return the typed EINVAL value, never a numeric sentinel"
    );

    let path_open = runner
        .find("  wasiImport.path_open = (")
        .expect("managed path_open wrapper");
    let path_open = &runner[path_open..];
    let kernel_open = path_open
        .find("callSyncRpc('process.path_open_at'")
        .expect("dirfd-aware kernel path_open");
    let kernel_registration = path_open
        .find("registerKernelDelegateFd(kernelFd)")
        .expect("kernel descriptor registration");
    let ambient_delegate = path_open
        .find("() => delegatePathOpen(")
        .expect("standalone WASI path_open fallback");
    assert!(
        kernel_open < kernel_registration && kernel_registration < ambient_delegate,
        "managed path_open must return a kernel open description before standalone WASI fallback"
    );

    assert!(
        path_open.contains(
            "const procFdResult = SIDECAR_MANAGED_PROCESS\n      ? null\n      : openProcSelfFdAlias("
        ),
        "managed /proc/self/fd aliases must flow through capability-aware kernel path_open"
    );

    for (start, end) in [
        ("  path_owner(", "  fd_owner("),
        ("  path_mode(", "  path_size("),
        ("  path_size(", "  path_blocks("),
        ("  path_blocks(", "  path_rdev("),
        ("  path_rdev(", "  chmod("),
    ] {
        let start = runner
            .find(start)
            .unwrap_or_else(|| panic!("missing {start}"));
        let section = &runner[start..];
        let end = section.find(end).unwrap_or_else(|| panic!("missing {end}"));
        assert!(
            section[..end].contains("callSyncRpc('process.path_stat_at'"),
            "{start} must read dirfd-relative metadata from the kernel"
        );
    }

    let wasi_path_open = wasi.find("    _pathOpen(").expect("WASI path_open");
    let wasi_path_open = &wasi[wasi_path_open..];
    assert!(
        wasi_path_open
            .find("if (this._sidecarManagedProcess())")
            .expect("managed ambient-open rejection")
            < wasi_path_open
                .find("__agentOSFs().openSync")
                .expect("standalone Node-fs open"),
        "managed WASI must reject ambient Node-fs path_open fallback"
    );

    for field in ["mode", "uid", "gid", "blocks", "rdev"] {
        assert!(
            rpc.contains(&format!("\"{field}\": stat.{field}")),
            "kernel stat RPC must expose {field} without a runner-side metadata shadow"
        );
    }
    assert!(
        filesystem.contains("struct ProcessModuleFsReader")
            && filesystem.contains("read_file_for_process(")
            && filesystem.contains("let reader = ProcessModuleFsReader"),
        "production JavaScript module loading must resolve against the live kernel VFS"
    );
}

#[test]
fn managed_wasm_uses_one_sidecar_owned_posix_poll() {
    let root = repo_root();
    let runner =
        std::fs::read_to_string(root.join("crates/execution/assets/runners/wasm-runner.mjs"))
            .expect("read managed WASM runner");
    let sidecar = std::fs::read_to_string(
        root.join("crates/native-sidecar/src/execution/host_dispatch/network_compat.rs"),
    )
    .expect("read POSIX poll dispatcher");

    let net_poll = runner
        .split("  net_poll(fdsPtr, nfds, timeoutMs, retReadyPtr, temporarySignalMask = null) {")
        .nth(1)
        .expect("managed net_poll implementation");
    let managed = net_poll
        .split("    const startedAt = Date.now();")
        .next()
        .expect("managed poll fast path");
    assert!(
        managed.contains("callSyncRpc('process.posix_poll'")
            && managed.contains("temporarySignalMask"),
        "managed poll and ppoll must use one typed sidecar RPC carrying the optional mask"
    );
    for split_wait in [
        "callSyncRpc('__kernel_poll'",
        "callSyncRpc('process.hostnet_poll'",
        "pumpSpawnedChildren(",
        "Atomics.wait(",
    ] {
        assert!(
            !managed.contains(split_wait),
            "managed poll must not regress to a split/pumped wait: {split_wait}"
        );
    }

    let ppoll = runner
        .split("        proc_ppoll_v1(")
        .nth(1)
        .expect("ppoll ABI implementation")
        .split("        },")
        .next()
        .expect("ppoll ABI body");
    assert!(ppoll.contains("hostNetImport.net_poll("));
    assert!(!ppoll.contains("signal_mask_scope_begin"));
    assert!(!ppoll.contains("signal_mask_scope_end"));

    assert!(
        sidecar.contains("pub(in crate::execution) fn service_deferred_posix_poll")
            && sidecar.contains("task_notify.notified()")
            && sidecar.contains("wait_handle.wait_for_change_async(observed)"),
        "the sidecar must own one coalesced managed/kernel/deadline wait task"
    );
}

#[test]
fn managed_blocking_socket_operations_wait_through_posix_poll() {
    let root = repo_root();
    let runner =
        std::fs::read_to_string(root.join("crates/execution/assets/runners/wasm-runner.mjs"))
            .expect("read managed WASM runner");
    let helper = runner
        .split("function waitManagedHostNetReadable(")
        .nth(1)
        .expect("managed socket wait helper")
        .split("\n}")
        .next()
        .expect("managed socket wait body");
    assert!(helper.contains("callSyncRpc('process.posix_poll'"));
    assert!(helper.contains("dispatchPendingWasmSignals(restartableOperation === true)"));
    assert!(helper.contains("pumpSpawnedChildren(0)"));
    assert!(helper.contains("Math.min(SPAWNED_CHILD_WAIT_SLICE_MS"));
    assert!(helper.contains("deadline - Date.now()"));

    for (operation, managed_end) in [
        ("net_accept", "\n    if (!socket.serverId"),
        ("net_recv", "\n      if (hostNetSocketBaseType"),
        ("net_recvfrom", "\n      const udpSocketId"),
    ] {
        let body = runner
            .split(&format!("  {operation}("))
            .nth(1)
            .unwrap_or_else(|| panic!("{operation} implementation"));
        let body = body
            .split("\n  },")
            .next()
            .expect("operation body boundary");
        let managed = body
            .split("managed === true")
            .nth(1)
            .unwrap_or_else(|| panic!("{operation} managed path"));
        let managed = managed
            .split(managed_end)
            .next()
            .unwrap_or_else(|| panic!("{operation} managed path boundary"));
        assert!(
            managed.contains("waitManagedHostNetReadable"),
            "managed {operation} must use the interruptible combined wait"
        );
        assert!(
            !managed.contains("pumpSpawnedChildrenOrWaitRestartable"),
            "managed {operation} must not poll/pump on a timer"
        );
    }
}

#[test]
fn sidecar_publishes_only_one_signal_delivery_scope_at_a_time() {
    let root = repo_root();
    let process =
        std::fs::read_to_string(root.join("crates/native-sidecar/src/execution/process.rs"))
            .expect("read process owner");
    let published = process
        .split("Ok(SignalCheckpointOutcome::Published) => {")
        .nth(1)
        .expect("published signal branch")
        .split("Ok(SignalCheckpointOutcome::ForwardToProcess")
        .next()
        .expect("published signal branch end");
    assert!(
        published.contains("break;") && !published.contains("continue;"),
        "kernel signal delivery tokens are strict LIFO; publish one and wait for signal_end"
    );
}

#[test]
fn phase_one_contains_no_wasmtime_implementation_or_dependency() {
    let root = repo_root();
    let lock = std::fs::read_to_string(root.join("Cargo.lock")).expect("read Cargo.lock");
    assert!(
        !lock.contains("name = \"wasmtime") && !lock.contains("name = \"wasmtime-"),
        "Wasmtime dependencies belong to the later executor revision, not the prerequisite refactor"
    );

    let mut manifests = vec![root.join("Cargo.toml")];
    for entry in std::fs::read_dir(root.join("crates")).expect("read workspace crates") {
        let path = entry
            .expect("read workspace crate entry")
            .path()
            .join("Cargo.toml");
        if path.is_file() {
            manifests.push(path);
        }
    }
    for manifest in manifests {
        let source = std::fs::read_to_string(&manifest)
            .unwrap_or_else(|error| panic!("read {}: {error}", manifest.display()));
        for line in source.lines().map(strip_line_comment) {
            let compact = line
                .chars()
                .filter(|ch| !ch.is_whitespace())
                .collect::<String>();
            assert!(
                !compact.starts_with("wasmtime=")
                    && !compact.starts_with("wasmtime-")
                    && !compact.contains("package=\"wasmtime"),
                "Wasmtime dependency appeared in {}: {}",
                manifest.display(),
                line.trim()
            );
        }
    }

    for relative in production_source_files(&root) {
        let source = std::fs::read_to_string(root.join(&relative))
            .unwrap_or_else(|error| panic!("read {}: {error}", relative.display()));
        let production = production_source_text(&source);
        assert!(
            !production.contains("wasmtime::") && !production.contains("extern crate wasmtime"),
            "Wasmtime implementation code appeared before Phase 1 closed in {}",
            relative.display()
        );
    }
}

#[test]
fn kernel_process_table_is_the_only_durable_signal_state_owner() {
    let root = repo_root();
    let process_table = std::fs::read_to_string(root.join("crates/kernel/src/process_table.rs"))
        .expect("read kernel process table");
    let record = rust_braced_item(&process_table, "struct ProcessRecord {");
    for field in [
        "blocked_signals: SignalSet",
        "pending_signals: SignalSet",
        "signal_actions: [SignalAction",
        "signal_deliveries: Vec<InProgressSignalDelivery>",
        "temporary_signal_masks: Vec<TemporarySignalMask>",
    ] {
        assert!(
            record.contains(field),
            "kernel ProcessRecord is missing {field}"
        );
    }
    let schedule = rust_braced_item(&process_table, "fn queue_or_schedule_signal(");
    assert!(
        schedule.contains("record.pending_signals.insert(signal)?")
            && schedule.contains("ProcessControlRequest::Checkpoint")
            && schedule.contains("ProcessControlRequest::Stop")
            && schedule.contains("ProcessControlRequest::Terminate"),
        "one kernel path must decide pending, caught, stop, and terminating signal behavior"
    );
    let delivery = rust_braced_item(&process_table, "fn deliver_signals(");
    assert!(
        delivery.contains("delivery.runtime_endpoint.request_control(*request)"),
        "kernel signal decisions must reach adapters only through the runtime endpoint"
    );

    let signals =
        std::fs::read_to_string(root.join("crates/native-sidecar/src/execution/signals.rs"))
            .expect("read signal adapter");
    let registration =
        rust_braced_item(&signals, "pub(crate) fn apply_kernel_signal_registration(");
    assert!(
        registration.contains("process")
            && registration.contains(".kernel_handle")
            && registration.contains(".signal_action(signal, Some(action))"),
        "adapter registrations must update the authoritative kernel record directly"
    );

    let non_kernel_files = production_source_files(&root)
        .into_iter()
        .filter(|path| {
            path.starts_with("crates/execution/src")
                || path.starts_with("crates/native-sidecar/src")
        })
        .collect::<Vec<_>>();
    let duplicated_owner_fields = production_matches(
        &root,
        &non_kernel_files,
        &[
            "blocked_signals:",
            "pending_signals:",
            "signal_actions:",
            "signal_deliveries:",
            "temporary_signal_masks:",
        ],
    );
    assert!(
        duplicated_owner_fields.is_empty(),
        "durable signal state escaped the kernel process table:\n{}",
        duplicated_owner_fields.join("\n")
    );
}

#[test]
fn common_posix_semantics_do_not_switch_on_executor_variants() {
    let root = repo_root();

    // Capability owners and reactors are engine-blind without an allowlist.
    // Any future occurrence in these directories is a hard architecture
    // regression, not a line to append to a sampled list.
    for relative in production_source_files(&root).into_iter().filter(|path| {
        path.starts_with("crates/native-sidecar/src/execution/host_dispatch")
            || path.starts_with("crates/native-sidecar/src/execution/network")
            || matches!(
                path.to_string_lossy().as_ref(),
                "crates/native-sidecar/src/execution/signals.rs"
                    | "crates/native-sidecar/src/execution/stdio.rs"
                    | "crates/native-sidecar/src/execution/process_events.rs"
                    | "crates/native-sidecar/src/execution/coordinator.rs"
                    | "crates/native-sidecar/src/filesystem.rs"
            )
    }) {
        let source = std::fs::read_to_string(root.join(&relative))
            .unwrap_or_else(|error| panic!("read {}: {error}", relative.display()));
        let production = production_source_text(&source);
        for forbidden in [
            "GuestRuntimeKind",
            "ActiveExecution::",
            "ExecutionBackendKind",
        ] {
            assert!(
                !production.contains(forbidden),
                "common POSIX owner {} contains executor switch token {forbidden}",
                relative.display()
            );
        }
    }

    let child = production_source_text(
        &std::fs::read_to_string(root.join("crates/native-sidecar/src/execution/child_process.rs"))
            .expect("read child process service"),
    );
    for forbidden in [
        "process.runtime == GuestRuntimeKind",
        "process.runtime != GuestRuntimeKind",
        "resolved.runtime == GuestRuntimeKind",
        "resolved.runtime != GuestRuntimeKind",
        "current_runtime == GuestRuntimeKind",
        "current_runtime != GuestRuntimeKind",
    ] {
        assert!(
            !child.contains(forbidden),
            "child process semantics switched on executor identity: {forbidden}"
        );
    }
    assert_eq!(
        child.matches("match resolved.runtime {").count(),
        3,
        "resolved-runtime matches are confined to direct spawn, exec replacement, and nested spawn adapter construction"
    );
    assert!(
        child.contains("resolved.adapter_policy.accepts_inherited_host_network_fds")
            && child.contains("resolved.adapter_policy.materializes_direct_runtime_stdio")
            && child.contains("resolved.adapter_policy.canonicalizes_runtime_stdin")
            && child.contains("supports_prepared_in_place_exec"),
        "common process code must consume explicit adapter capabilities"
    );

    let process = production_source_text(
        &std::fs::read_to_string(root.join("crates/native-sidecar/src/execution/process.rs"))
            .expect("read ActiveProcess implementation"),
    );
    let adapter_impl = process
        .find("impl ActiveExecution {")
        .expect("ActiveExecution adapter implementation");
    assert!(
        !process[..adapter_impl].contains("ActiveExecution::"),
        "ActiveProcess common lifecycle must call the backend contract instead of matching its storage enum"
    );

    let rpc = production_source_text(
        &std::fs::read_to_string(
            root.join("crates/native-sidecar/src/execution/javascript/rpc.rs"),
        )
        .expect("read compatibility RPC decoder"),
    );
    assert!(
        !rpc.contains("process.runtime == GuestRuntimeKind")
            && rpc.contains("process.execution.synchronous_fd_write_policy()"),
        "descriptor write semantics must use an explicit backend policy"
    );
}

#[test]
fn production_backends_route_typed_host_calls_through_family_capabilities() {
    let root = repo_root();
    let process =
        std::fs::read_to_string(root.join("crates/native-sidecar/src/execution/process.rs"))
            .expect("read active execution adapter");
    let events =
        std::fs::read_to_string(root.join("crates/native-sidecar/src/execution/process_events.rs"))
            .expect("read execution event service");
    let dispatcher = std::fs::read_to_string(
        root.join("crates/native-sidecar/src/execution/host_dispatch/mod.rs"),
    )
    .expect("read shared host dispatcher");

    // JavaScript, Python's embedded-JavaScript bridge, and compatibility WASM
    // all use the same decoder before a sidecar semantic operation is chosen.
    assert_eq!(
        process
            .matches("return route_compatibility_host_call(")
            .count(),
        3,
        "every production V8-backed adapter must use the bound common host-call submission path"
    );
    assert!(
        process.contains("ExecutionBackend::configure_host_services")
            && process.contains("poll_event_with_host(")
            && process.contains("try_poll_event_with_host("),
        "production backends must receive host services before start and submit through them"
    );
    assert!(
        events.contains("ActiveExecutionEvent::Common(ExecutionEvent::HostCall"),
        "the production event pump must consume common host-call events"
    );
    assert!(
        events.contains("dispatch_host_operation(generation, kernel, process, operation, reply)"),
        "common host calls must reach the shared sidecar dispatcher"
    );

    for family in [
        "FilesystemCapability",
        "NetworkCapability",
        "ProcessCapability",
        "TerminalCapability",
        "SignalCapability",
        "IdentityCapability",
        "ClockCapability",
        "EntropyCapability",
    ] {
        assert!(
            dispatcher.contains(family),
            "shared dispatcher is missing production {family} routing"
        );
    }
    for family in [
        "filesystem",
        "network",
        "process",
        "terminal",
        "signal",
        "identity",
        "clock",
        "entropy",
    ] {
        let path = root.join(format!(
            "crates/native-sidecar/src/execution/host_dispatch/{family}.rs"
        ));
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        assert!(
            source.contains("impl SidecarHostCapability<"),
            "{family} must have a production capability implementation"
        );
    }
}

#[test]
fn loopback_vm_fetch_uses_the_vm_scoped_event_pump() {
    let coordinator = include_str!("../src/execution/coordinator.rs");
    let http = include_str!("../src/execution/javascript/http.rs");
    let vm_fetch = coordinator
        .split_once("pub(crate) async fn vm_fetch(")
        .expect("vm.fetch coordinator")
        .1
        .split_once("pub(crate) async fn get_signal_state(")
        .expect("end of vm.fetch coordinator")
        .0;

    for required in [
        "begin_loopback_http_request",
        "self.pump_process_events(&ownership).await",
        "process_event_notify.notified()",
        "take_loopback_http_response",
    ] {
        assert!(
            vm_fetch.contains(required),
            "loopback vm.fetch must retain main event-pump step {required}"
        );
    }
    assert!(
        !http.contains("dispatch_host_operation"),
        "loopback HTTP must not bypass VM-scoped context dispatch through the kernel-only fallback"
    );
}

#[test]
fn shared_tcp_connect_has_no_blocking_native_fallback() {
    let root = repo_root();
    let tcp =
        std::fs::read_to_string(root.join("crates/native-sidecar/src/execution/network/tcp.rs"))
            .expect("read shared TCP implementation");

    assert!(
        !tcp.contains("TcpStream::connect_timeout"),
        "native TCP connects must be deferred to the shared Tokio reactor"
    );
    assert!(
        tcp.contains(
            "native TCP connect reached the synchronous constructor without reactor deferral"
        ),
        "the synchronous constructor must fail closed when a caller misses reactor deferral"
    );
}

#[test]
fn compatibility_wasm_rpc_inventory_matches_runner_literals() {
    let root = repo_root();
    let runner =
        std::fs::read_to_string(root.join("crates/execution/assets/runners/wasm-runner.mjs"))
            .expect("read compatibility WASM runner");
    let inventory = std::fs::read_to_string(
        root.join("crates/native-sidecar/src/execution/host_dispatch/inventory.rs"),
    )
    .expect("read reviewed compatibility WASM RPC inventory");

    fn quoted_values(section: &str) -> BTreeSet<String> {
        section
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                line.strip_prefix('"')
                    .and_then(|line| line.strip_suffix(","))
                    .and_then(|line| line.strip_suffix('"'))
                    .map(str::to_owned)
            })
            .collect()
    }

    let mut dynamic_targets = Vec::new();
    let runner_methods = runner
        .match_indices("callSyncRpc(")
        .filter_map(|(offset, marker)| {
            let tail = runner[offset + marker.len()..].trim_start();
            let quote = tail.chars().next()?;
            if quote != '\'' && quote != '"' {
                if runner[..offset].ends_with("function ") {
                    return None;
                }
                let line = runner[..offset]
                    .rsplit_once('\n')
                    .map(|(_, line)| line)
                    .unwrap_or(&runner[..offset]);
                dynamic_targets.push(format!(
                    "{}{}",
                    line.trim(),
                    tail.lines().next().unwrap_or("")
                ));
                return None;
            }
            let tail = &tail[quote.len_utf8()..];
            let end = tail.find(quote)?;
            Some(tail[..end].to_owned())
        })
        .collect::<BTreeSet<_>>();
    assert!(dynamic_targets.is_empty(),
        "compatibility WASM runner callSyncRpc targets must be literal; add every target to the frozen inventory: {dynamic_targets:?}"
    );
    let inventory_section = inventory
        .split_once("WASM_RUNNER_RPC_INVENTORY: &[&str] = &[")
        .expect("inventory declaration")
        .1
        .split_once("\n];")
        .expect("inventory terminator")
        .0;
    let frozen_methods = quoted_values(inventory_section);
    assert_eq!(
        frozen_methods, runner_methods,
        "update and review the typed/adapter-only WASM RPC inventory whenever the runner changes"
    );

    // The compatibility bootstrap hides a second semantic surface behind one
    // generic `process.wasm_sync_rpc` method. Freeze both the wrapper switch
    // and its delta from the literal runner inventory; otherwise a Linux call
    // can bypass the typed decoder without changing wasm-runner.mjs.
    let bootstrap = std::fs::read_to_string(root.join("crates/execution/src/wasm.rs"))
        .expect("read compatibility WASM bootstrap");
    let rpc =
        std::fs::read_to_string(root.join("crates/native-sidecar/src/execution/javascript/rpc.rs"))
            .expect("read compatibility RPC allowlist");
    let allowed_section = rpc
        .split_once("const ALLOWED_WASM_PROCESS_SYNC_RPCS: &[&str] = &[")
        .expect("wrapped RPC allowlist")
        .1
        .split_once("\n];")
        .expect("wrapped RPC allowlist terminator")
        .0;
    let allowed = quoted_values(allowed_section);
    let wrapped_switch = bootstrap
        .split_once("case \"process.exec_image_open\":")
        .expect("wrapped RPC switch")
        .1
        .split_once("_processWasmSyncRpc.applySync")
        .expect("wrapped RPC dispatch")
        .0;
    let mut emitted_wrapped = wrapped_switch
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("case \"")
                .and_then(|line| line.strip_suffix("\":"))
                .map(str::to_owned)
        })
        .collect::<BTreeSet<_>>();
    emitted_wrapped.insert(String::from("process.exec_image_open"));
    assert_eq!(
        allowed, emitted_wrapped,
        "the generic WASM wrapper switch and sidecar allowlist must remain exact"
    );

    let wrapped_only_section = inventory
        .split_once("WASM_WRAPPED_ONLY_RPC_INVENTORY: &[&str] = &[")
        .expect("wrapped-only inventory declaration")
        .1
        .split_once("\n];")
        .expect("wrapped-only inventory terminator")
        .0;
    let frozen_wrapped_only = quoted_values(wrapped_only_section);
    let actual_wrapped_only = allowed
        .difference(&runner_methods)
        .cloned()
        .collect::<BTreeSet<_>>();
    assert_eq!(
        frozen_wrapped_only, actual_wrapped_only,
        "review every semantic RPC hidden behind process.wasm_sync_rpc"
    );

    let adapter_only_section = inventory
        .split_once("WASM_ADAPTER_ONLY_RPCS: &[&str] = &[")
        .expect("adapter-only inventory declaration")
        .1
        .split_once("\n];")
        .expect("adapter-only inventory terminator")
        .0;
    assert_eq!(
        quoted_values(adapter_only_section),
        BTreeSet::from([
            String::from("fs.blockingIoTimeoutMsSync"),
            String::from("process.fd_description_alias_count"),
            String::from("process.fd_description_identity"),
            String::from("process.fd_snapshot"),
        ]),
        "only read-only V8 projection/configuration queries may bypass typed Linux operations"
    );

    let filesystem = std::fs::read_to_string(
        root.join("crates/native-sidecar/src/execution/host_dispatch/filesystem.rs"),
    )
    .expect("read typed filesystem decoder");
    assert!(
        filesystem.contains("for method in semantic_rpc_inventory()")
            && filesystem.contains("must not fall through to the legacy bridge"),
        "the typed decoder proof must cover the union of literal and wrapped-only RPCs"
    );

    let legacy_exec_routes = production_matches(
        &root,
        &[
            PathBuf::from("crates/native-sidecar/src/execution/child_process.rs"),
            PathBuf::from("crates/native-sidecar/src/service.rs"),
        ],
        &[
            "request.method == \"process.exec\"",
            "request.method == \"process.exec_fd_image_commit\"",
            "\"process.exec\" =>",
            "\"process.exec_fd_image_commit\" =>",
        ],
    );
    assert!(
        legacy_exec_routes.is_empty(),
        "execve/fexecve must enter as typed ProcessOperation::Exec, never a legacy HostRpcRequest:\n{}",
        legacy_exec_routes.join("\n")
    );
}

#[test]
fn compatibility_wasm_filesystem_inventory_uses_only_typed_kernel_dispatch() {
    let root = repo_root();
    let router = std::fs::read_to_string(
        root.join("crates/native-sidecar/src/execution/host_dispatch/mod.rs"),
    )
    .expect("read host dispatcher");
    let filesystem = std::fs::read_to_string(
        root.join("crates/native-sidecar/src/execution/host_dispatch/filesystem.rs"),
    )
    .expect("read filesystem capability");
    let process =
        std::fs::read_to_string(root.join("crates/native-sidecar/src/execution/process.rs"))
            .expect("read execution event mapper");

    assert!(
        router.contains("_ if full_filesystem =>")
            && router.contains(
                "filesystem::decode(request, full_filesystem, max_reply_bytes)?"
            ),
        "the compatibility-WASM router must delegate unmatched calls to the bounded typed filesystem decoder"
    );
    let wasm_mapper = process
        .split_once("fn map_wasm_execution_event_with_host(")
        .expect("WASM mapper")
        .1
        .split_once("pub(super) fn find_socket_state_entry")
        .expect("end WASM mapper")
        .0;
    assert!(
        wasm_mapper.contains("route_compatibility_host_call(")
            && wasm_mapper.contains("true,")
            && wasm_mapper.contains("max_reply_bytes,"),
        "compatibility WASM must enable complete typed filesystem decoding"
    );
    for forbidden in [
        "service_javascript_fs_sync_rpc",
        "service_javascript_sync_rpc",
        "JavascriptSyncRpcServiceRequest",
        "guest_filesystem_call",
        "handle_guest_filesystem_call",
        "javascript::rpc",
    ] {
        assert!(
            !filesystem.contains(forbidden),
            "typed filesystem capability must not delegate to legacy RPC service {forbidden}"
        );
    }
}

#[test]
fn typed_process_dispatch_cannot_reconstruct_javascript_launch_protocol_types() {
    let dispatcher = include_str!("../src/execution/host_dispatch/mod.rs");
    let process_capability = include_str!("../src/execution/host_dispatch/process.rs");
    for (label, source) in [
        ("host dispatcher", dispatcher),
        ("process capability", process_capability),
    ] {
        for forbidden in [
            "JavascriptChildProcessSpawnRequest",
            "JavascriptChildProcessSpawnOptions",
            "JavascriptPosixSpawnFileAction",
            "JavascriptSpawnHostNetFd",
        ] {
            assert!(
                !source.contains(forbidden),
                "{label} reconstructs compatibility-only {forbidden} below the adapter decoder"
            );
        }
    }

    let contract = include_str!("../../execution/src/host/process.rs");
    assert!(
        contract.contains("Spawn(BoundedProcessLaunchRequest)")
            && contract.contains("Exec(BoundedProcessLaunchRequest)"),
        "queued process launch operations must retain their payload admission proof"
    );
}

#[test]
fn neutral_capability_execution_files_do_not_depend_on_executor_protocol_types() {
    let network = include_str!("../src/execution/host_dispatch/network.rs");
    let filesystem = include_str!("../src/execution/host_dispatch/filesystem.rs");
    let filesystem_execution = filesystem
        .split_once("pub(super) struct FilesystemCapability")
        .expect("filesystem capability marker")
        .1
        .split_once("#[cfg(test)]")
        .expect("filesystem capability test marker")
        .0;
    for (label, source) in [
        ("network", network),
        ("filesystem execution", filesystem_execution),
    ] {
        for forbidden in [
            "Javascript",
            "V8",
            "Python",
            "Wasmtime",
            "HostRpcRequest",
            "HostRpcServiceResponse",
        ] {
            assert!(
                !source.contains(forbidden),
                "neutral {label} capability depends on adapter type {forbidden}"
            );
        }
    }
    assert!(
        !filesystem.contains("process.runtime == GuestRuntimeKind"),
        "filesystem write semantics must be explicit in the typed operation"
    );
    let compatibility = include_str!("../src/execution/host_dispatch/network_compat.rs");
    assert!(
        compatibility.contains("HostRpcRequest")
            && compatibility.contains("dispatch_context_managed_network_operation"),
        "compatibility payload adaptation must stay in the explicitly named adapter module"
    );
}

#[test]
fn unix_listener_close_is_lossless_and_acknowledged() {
    let root = repo_root();
    let unix =
        std::fs::read_to_string(root.join("crates/native-sidecar/src/execution/network/unix.rs"))
            .expect("read Unix reactor source");
    let managed = std::fs::read_to_string(
        root.join("crates/native-sidecar/src/execution/network/managed.rs"),
    )
    .expect("read shared managed-network source");
    let compact_unix: String = unix
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    let compact_managed: String = managed
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();

    assert!(
        compact_unix.contains("self.close_notify.notify_one();")
            && compact_unix.contains("self.close_completion")
            && !compact_unix.contains("self.close_notify.notify_waiters()"),
        "Unix listener close must retain a notification permit between acceptor select points"
    );
    assert!(
        compact_unix.contains("UnixListenerTaskCompletion(Some(close_complete))")
            && compact_unix.contains("completion.send(())"),
        "the Unix listener owner must acknowledge every terminal path after dropping its FD"
    );
    assert!(
        compact_managed.contains(
            "operation_deadline_timeout(\"Unixlistenerclose\",deadline,completion,).await"
        ) && compact_managed.contains("HostServiceResponse::Deferred"),
        "the shared listener-close operation must await bounded owner-task completion"
    );
}

/// Every production Rust source file under `crates/*/src/`, repo-relative,
/// excluding build scripts, benches, bins, and `tests/` trees.
fn production_source_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let crates_dir = root.join("crates");
    let mut crate_dirs: Vec<PathBuf> = std::fs::read_dir(&crates_dir)
        .expect("crates/ directory should exist")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    crate_dirs.sort();
    for crate_dir in crate_dirs {
        let src = crate_dir.join("src");
        if src.is_dir() {
            collect_rs(&src, root, &mut out);
        }
    }
    out.sort();
    out
}

fn collect_rs(dir: &Path, root: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("read_dir {dir:?}: {err}"))
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            // Exclude bench/dev binaries that are not production runtime.
            if path.file_name().map(|n| n == "bin").unwrap_or(false) {
                continue;
            }
            collect_rs(&path, root, out);
        } else if path.extension().map(|e| e == "rs").unwrap_or(false) {
            let rel = path
                .strip_prefix(root)
                .expect("source path under repo root")
                .to_path_buf();
            out.push(rel);
        }
    }
}

/// Returns true if the file is excluded from scanning entirely.
fn is_excluded_file(rel: &Path) -> bool {
    let s = rel.to_string_lossy();
    s.ends_with("build.rs")
        || s.ends_with("build_support.rs")
        || s.ends_with("v8_bridge_build.rs")
        // Benchmarking / dev tooling, not production host-access surface.
        || s == "crates/execution/src/benchmark.rs"
        || s.starts_with("crates/native-baseline/")
        // Browser support is intentionally retained but disabled; dormant
        // browser sources must not gate the native reactor migration.
        || s.starts_with("crates/native-sidecar-browser/")
        || s.starts_with("crates/agentos-sidecar-browser/")
        || s.contains("/src/bin/")
}

/// Strip a trailing `//` line comment (good enough for this lint; we are not
/// trying to be a full Rust parser, only to avoid flagging commented examples).
fn strip_line_comment(line: &str) -> &str {
    match line.find("//") {
        Some(idx) => &line[..idx],
        None => line,
    }
}

/// Track whether a line is inside a top-level `#[cfg(test)]` module so test
/// code is excluded from the scan. We watch for `#[cfg(test)]` immediately
/// followed by a `mod ... {` and then balance braces until the module closes.
struct CfgTestTracker {
    pending_cfg_test: bool,
    depth: u32,
}

impl CfgTestTracker {
    fn new() -> Self {
        Self {
            pending_cfg_test: false,
            depth: 0,
        }
    }

    /// Feed a line. Returns true if this line is inside a `#[cfg(test)]` module.
    fn in_test(&mut self, raw: &str) -> bool {
        let line = strip_line_comment(raw);
        let trimmed = line.trim();

        if self.depth > 0 {
            // Already inside a cfg(test) module: update brace balance.
            self.depth += count_open(line);
            self.depth = self.depth.saturating_sub(count_close(line));
            return true;
        }

        if trimmed.starts_with("#[cfg(")
            && trimmed.contains("test")
            && !trimmed.contains("not(test)")
        {
            self.pending_cfg_test = true;
            return false;
        }

        if self.pending_cfg_test {
            if trimmed.is_empty() || trimmed.starts_with("#[") || trimmed.starts_with("//") {
                // Attributes/blank lines may sit between #[cfg(test)] and the item.
                return false;
            }
            // The attribute applies to the next item. Any braced item (module,
            // function, impl, etc.) creates a test-only region that must be
            // skipped wholesale; otherwise a production audit would count
            // fixture thread/runtime/channel sites inside cfg(test) functions.
            self.pending_cfg_test = false;
            if count_open(line) > count_close(line) {
                self.depth = count_open(line).saturating_sub(count_close(line));
                return true;
            }
            if !trimmed.ends_with(';') {
                // Multi-line item header: keep consuming test-only lines until
                // its opening brace appears.
                self.pending_cfg_test = true;
            }
            // A single `#[cfg(test)]` item (use/fn/const/static). Skip this line.
            return true;
        }

        false
    }
}

fn production_source_text(source: &str) -> String {
    let mut tracker = CfgTestTracker::new();
    source
        .lines()
        .filter(|line| !tracker.in_test(line))
        .map(strip_line_comment)
        .collect::<Vec<_>>()
        .join("\n")
}

fn count_open(s: &str) -> u32 {
    s.bytes().filter(|&b| b == b'{').count() as u32
}
fn count_close(s: &str) -> u32 {
    s.bytes().filter(|&b| b == b'}').count() as u32
}

/// A banned-API class and the regex-free matchers describing it.
struct BannedClass {
    name: &'static str,
    /// Substrings; a line matches the class if it contains any of them.
    needles: &'static [&'static str],
    /// Files (repo-relative) where this class is sanctioned.
    allowlist: &'static [&'static str],
}

fn line_matches(line: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| line.contains(n))
}

/// Run the chokepoint scan for one banned class and return offending
/// `path:line: text` strings that are NOT in the allowlist.
fn scan_class(root: &Path, files: &[PathBuf], class: &BannedClass) -> Vec<String> {
    let mut violations = Vec::new();

    for rel in files {
        if is_excluded_file(rel) {
            continue;
        }
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        let allowed = class.allowlist.iter().any(|entry| {
            entry
                .strip_suffix('/')
                .map_or(rel_str == *entry, |directory| {
                    rel_str.starts_with(directory)
                        && rel_str.as_bytes().get(directory.len()) == Some(&b'/')
                })
        });
        let abs = root.join(rel);
        let content =
            std::fs::read_to_string(&abs).unwrap_or_else(|err| panic!("read {abs:?}: {err}"));
        let mut tracker = CfgTestTracker::new();
        for (idx, raw) in content.lines().enumerate() {
            let in_test = tracker.in_test(raw);
            if allowed {
                continue; // still need to advance the tracker above
            }
            if in_test {
                continue;
            }
            let code = strip_line_comment(raw);
            if line_matches(code, class.needles) {
                violations.push(format!("{}:{}: {}", rel_str, idx + 1, raw.trim()));
            }
        }
    }
    violations
}

// ---------------------------------------------------------------------------
// Allowlists -- built from the CURRENT legitimate uses (green today).
// ---------------------------------------------------------------------------

/// fs: host filesystem access.
///
/// Sanctioned surface: the sidecar host-FS plumbing + VFS-backed runtime, the
/// JS/Python/WASM runtime asset & module loaders, the sidecar bootstrap
/// (stdio/service/state/vm), and runtime support glue. These modules read
/// real host files to seed the VFS, load runtime assets, and bridge guest FS
/// syscalls to the host-dir mount.
const FS_ALLOW: &[&str] = &[
    // sidecar host-FS chokepoint + bootstrap. `host_dir.rs` also contains the
    // universal host-mount confinement primitive (the `confine` module: the
    // single resolve-beneath walk using plain `openat(2)`, fd-anchored, no
    // `openat2`, running identically on Linux, macOS, and gVisor). It replaced
    // the deleted macOS-only `macos_fs.rs` cap-std fallback; see the `confine`
    // module docs for why `openat2` was removed.
    "crates/native-sidecar/src/filesystem.rs",
    "crates/native-sidecar/src/plugins/host_dir.rs",
    "crates/native-sidecar/src/plugins/module_access.rs",
    // agentOS package projection: the sidecar is the host-side TCB that reads a
    // trusted, client-configured package's tar + `agentos-package.json` from the
    // host to build the read-only `/opt/agentos` granular mounts (no extraction,
    // no on-disk symlink farm). Same sanctioned read-only host-source boundary as
    // filesystem.rs/host_dir.rs.
    "crates/native-sidecar/src/package_projection.rs",
    "crates/native-sidecar/src/stdio.rs",
    "crates/native-sidecar/src/state.rs",
    "crates/native-sidecar/src/vm.rs",
    "crates/native-sidecar/src/service.rs",
    "crates/native-sidecar/src/execution/",
    "crates/native-sidecar/src/plugins/chunked_local.rs",
    "crates/vfs-store/src/local/file_block_store.rs",
    "crates/vfs-store/src/local/sqlite_metadata_store.rs",
    // Package-format tooling reads and writes caller-selected host artifacts;
    // it never handles guest paths at runtime.
    "crates/vfs/src/package_format/mod.rs",
    "crates/vfs/src/package_format/pack.rs",
    // ACP trace output is an operator-selected host diagnostic sink. The
    // extension is split mechanically across its module root and restore path.
    "crates/agentos-sidecar/src/acp/mod.rs",
    "crates/agentos-sidecar/src/acp/restore.rs",
    // Tar-backed read-only VFS: mmaps the trusted, client-configured package
    // tar from the host and serves member byte ranges without extracting.
    // Same sanctioned read-only host-source boundary as host_dir.rs (the tar is
    // an immutable, content-addressed mount source); reads are SIGBUS-guarded.
    "crates/vfs/src/posix/tar_fs.rs",
    // language-runtime asset / module loaders (read host runtime assets)
    "crates/execution/src/python.rs",
    "crates/execution/src/wasm.rs",
    "crates/execution/src/javascript.rs",
    "crates/execution/src/node_import_cache.rs",
    "crates/execution/src/runtime_support.rs",
    // Host-side V8 diagnostics: module-trace and sync-RPC latency profilers
    // write to an operator-provided file path, and snapshot bootstrap reads the
    // userland bundle from PI_SNAPSHOT_BUNDLE_PATH. Host-only, not guest-reachable.
    "crates/v8-runtime/src/execution.rs",
    "crates/v8-runtime/src/host_call.rs",
    "crates/v8-runtime/src/snapshot.rs",
    // Session-phase perf recorder writes to an operator-provided file path
    // (AGENTOS_V8_SESSION_PHASES_FILE). Host-only diagnostics, same class as
    // execution.rs/host_call.rs above.
    "crates/v8-runtime/src/session.rs",
];

/// net: host network access.
///
/// Sanctioned surface: the kernel DNS/socket contract plane, the sidecar host-net
/// chokepoint (`execution.rs`, which owns all guest TCP/UDP/Unix sockets), the
/// host-backed storage/agent plugins (which open egress to S3 / Google Drive /
/// the sandbox-agent control plane), the embedded V8 runtime IPC socketpair,
/// and the client transport that talks to the spawned sidecar.
const NET_ALLOW: &[&str] = &[
    // Kernel DNS contract: address/config/result values only; host DNS
    // transport lives in the native-sidecar resolver module below.
    "crates/kernel/src/dns.rs",
    // Shared IP classifier only; no host sockets are opened here.
    "crates/kernel/src/network_policy.rs",
    // Shared socket-address formatting only; no host sockets are opened here.
    "crates/native-sidecar-core/src/net.rs",
    "crates/kernel/src/socket_table.rs",
    "crates/kernel/src/kernel.rs",
    // sidecar host-net chokepoint + bootstrap
    "crates/native-sidecar/src/execution/",
    "crates/native-sidecar/src/state.rs",
    "crates/native-sidecar/src/vm.rs",
    // Required inherited fd-3 response/control IPC stream; no external egress.
    "crates/native-sidecar/src/stdio.rs",
    // host-backed storage / agent plugins (network egress)
    "crates/native-sidecar/src/plugins/s3_common.rs",
    "crates/vfs-store/src/s3/block_store.rs",
    "crates/vfs-store/src/s3/object_backend.rs",
    "crates/native-sidecar/src/plugins/google_drive.rs",
    "crates/native-sidecar/src/plugins/sandbox_agent.rs",
    // embedded runtime IPC socketpair (not external egress)
    "crates/v8-runtime/src/embedded_runtime.rs",
    "crates/execution/src/v8_host.rs",
    "crates/execution/src/v8_runtime.rs",
    // client spawns + connects to the sidecar helper
    "crates/sidecar-client/src/transport.rs",
    // Authenticated local transport from the sidecar to the owning actor's
    // SQLite UDS endpoint. This is local IPC, not external network egress.
    "crates/actor-uds-client/src/lib.rs",
    // Test-only actor SQLite UDS fixture; it opens local Unix sockets but no
    // external network connection.
    "crates/agentos-sidecar/src/session_store/performance_tests.rs",
];

/// process: OS subprocess creation.
///
/// Sanctioned surface: only the client transport, which spawns agentos's
/// own sidecar helper binary. Guest "process" spawns go through the kernel
/// `CommandDriver` registry and never reach `Command::new`.
const PROCESS_ALLOW: &[&str] = &[
    "crates/sidecar-client/src/transport.rs",
    // V8 snapshot builder re-execs agentos's OWN binary as a helper
    // (SNAPSHOT_HELPER_ENV) so snapshot creation runs in a clean process.
    // Host-side bootstrap only; no guest-controlled input picks the program.
    "crates/v8-runtime/src/snapshot.rs",
];

/// env: process-environment reads.
///
/// Sanctioned surface: the scrubbed/bootstrap configuration readers that look
/// up host configuration (sidecar binary path, node binary path/PATH, codec
/// selection, subprocess re-exec markers, local-endpoint test escape hatch)
/// before a VM exists.
const ENV_ALLOW: &[&str] = &[
    "crates/sidecar-client/src/transport.rs",
    "crates/client/src/sidecar.rs",
    // Operator-selected ACP trace output path.
    "crates/agentos-sidecar/src/acp/restore.rs",
    "crates/agentos-sidecar/src/main.rs",
    "crates/execution/src/host_node.rs",
    // Node import cache reads an operator timeout knob before materializing
    // host-side runtime assets for VM startup.
    "crates/execution/src/node_import_cache.rs",
    // Host-side perf phase diagnostics toggles, read from operator env and not
    // guest-reachable.
    "crates/execution/src/javascript.rs",
    "crates/native-sidecar/src/filesystem.rs",
    "crates/v8-runtime/src/bridge.rs",
    "crates/native-sidecar/src/execution/",
    "crates/native-sidecar/src/plugins/s3_common.rs",
    // Host-process startup log-level knob, read before any VM exists.
    "crates/native-sidecar/src/main.rs",
    // Host-side V8 diagnostics toggles (module-trace + sync-RPC latency
    // profiling + snapshot-bundle path), read at runtime init from operator
    // env. Not guest-reachable.
    "crates/v8-runtime/src/execution.rs",
    "crates/v8-runtime/src/host_call.rs",
    "crates/v8-runtime/src/snapshot.rs",
    // Browser sidecar reads a test-only vm.fetch timeout override (bucket 1:
    // process-wide test/debug knob, native-only); not VM policy.
    // Warm-isolate pool sizing knob (AGENTOS_V8_WARM_ISOLATES), read at
    // executor init from operator env. Not guest-reachable.
    "crates/execution/src/v8_host.rs",
    // Wasm runner mode/cache knobs (AGENTOS_WASM_SNAPSHOT_RUNNER,
    // AGENTOS_WASM_RUNNER_NO_CACHE) + warm-pool sizing, read at executor init
    // from operator env. Not guest-reachable. (wasm.rs is already a sanctioned
    // FS asset-loading boundary above.)
    "crates/execution/src/wasm.rs",
    // Session-phase perf diagnostics toggles (AGENTOS_V8_SESSION_PHASES*),
    // read from operator env. Not guest-reachable.
    "crates/v8-runtime/src/session.rs",
];

fn fs_class() -> BannedClass {
    BannedClass {
        name: "fs",
        needles: &[
            "std::fs",
            "tokio::fs",
            "File::open",
            "File::create",
            "OpenOptions",
            "openat",
        ],
        allowlist: FS_ALLOW,
    }
}

fn net_class() -> BannedClass {
    BannedClass {
        name: "net",
        needles: &[
            "std::net::",
            "tokio::net::",
            "reqwest::",
            "reqwest ",
            "hyper::",
            "TcpStream::",
            "TcpListener::bind",
            "UdpSocket::bind",
            "UnixStream::connect",
            "UnixStream::pair",
            "UnixListener::bind",
            ".to_socket_addrs(",
            "std::os::unix::net",
        ],
        allowlist: NET_ALLOW,
    }
}

fn process_class() -> BannedClass {
    BannedClass {
        name: "process",
        needles: &[
            "std::process::Command",
            "process::Command",
            "tokio::process",
            "Command::new",
            "libc::fork",
            "nix::unistd::fork",
        ],
        allowlist: PROCESS_ALLOW,
    }
}

fn env_class() -> BannedClass {
    BannedClass {
        name: "env",
        needles: &[
            "env::var(",
            "env::var_os(",
            "env::vars(",
            "env::vars_os(",
            "std::env::var",
        ],
        allowlist: ENV_ALLOW,
    }
}

fn assert_green(root: &Path, files: &[PathBuf], class: BannedClass) {
    let violations = scan_class(root, files, &class);
    assert!(
        violations.is_empty(),
        "\n\nChokepoint lint ({}) found {} host-API use(s) OUTSIDE the sanctioned \
allowlist.\nEither route the access through an existing chokepoint, or -- if this \
is a genuinely new sanctioned boundary -- add the file to the `{}` allowlist in \
crates/native-sidecar/tests/architecture_guards.rs with a justifying comment.\n\n{}\n",
        class.name,
        violations.len(),
        match class.name {
            "fs" => "FS_ALLOW",
            "net" => "NET_ALLOW",
            "process" => "PROCESS_ALLOW",
            _ => "ENV_ALLOW",
        },
        violations.join("\n"),
    );
}

#[test]
fn fs_access_confined_to_chokepoints() {
    let root = repo_root();
    let files = production_source_files(&root);
    assert_green(&root, &files, fs_class());
}

#[test]
fn net_access_confined_to_chokepoints() {
    let root = repo_root();
    let files = production_source_files(&root);
    assert_green(&root, &files, net_class());
}

#[test]
fn process_spawn_confined_to_chokepoints() {
    let root = repo_root();
    let files = production_source_files(&root);
    assert_green(&root, &files, process_class());
}

#[test]
fn env_reads_confined_to_chokepoints() {
    let root = repo_root();
    let files = production_source_files(&root);
    assert_green(&root, &files, env_class());
}

#[test]
fn production_execution_lifecycle_is_runtime_neutral_and_delegated() {
    let root = repo_root();
    let lifecycle = std::fs::read_to_string(root.join("crates/execution/src/backend/lifecycle.rs"))
        .expect("read runtime-neutral lifecycle contract");
    for engine_type in [
        "JavascriptExecution",
        "PythonExecution",
        "WasmExecution",
        "BindingExecution",
        "V8SessionHandle",
    ] {
        assert!(
            !lifecycle.contains(engine_type),
            "common lifecycle contract must not name adapter type {engine_type}"
        );
    }

    let process =
        std::fs::read_to_string(root.join("crates/native-sidecar/src/execution/process.rs"))
            .expect("read ActiveExecution adapter");
    let compact: String = process
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    assert!(
        compact.contains("implExecutionBackendforActiveExecution")
            && compact.contains("self.backend().kind()")
            && compact.contains("self.backend().native_process_id()")
            && compact.contains("self.backend().is_prepared_for_start()")
            && compact.contains("self.backend_mut().start_prepared()")
            && compact.contains("self.backend_mut().begin_shutdown(reason)")
            && compact.contains("self.backend().set_paused(paused)")
            && compact.contains("self.backend_mut().write_stdin(bytes)")
            && compact.contains("self.backend_mut().close_stdin()")
            && compact.contains(
                "self.backend().deliver_signal_checkpoint(identity,signal,delivery_token,flags)"
            ),
        "ActiveExecution must delegate every common lifecycle method through ExecutionBackend"
    );

    for method in [
        "fn native_process_id(",
        "fn set_paused(",
        "fn write_stdin(",
        "fn close_stdin(",
        "fn deliver_signal_checkpoint(",
    ] {
        assert!(
            lifecycle.contains(method),
            "the runtime-neutral lifecycle contract must own {method}"
        );
    }

    let controls_start = process
        .find("pub(crate) fn apply_runtime_controls")
        .expect("find ActiveProcess runtime controls");
    let controls_end = process[controls_start..]
        .find("fn take_pending_runtime_exit_event")
        .map(|offset| controls_start + offset)
        .expect("find end of ActiveProcess runtime controls");
    let controls = &process[controls_start..controls_end];
    for executor_semantic in [
        "ActiveExecution::",
        "matches!(self.execution",
        "uses_shared_v8_runtime",
        "child_pid()",
    ] {
        assert!(
            !controls.contains(executor_semantic),
            "common runtime controls must not branch on executor semantic {executor_semantic}"
        );
    }
    assert!(
        controls.contains("if controls.checkpoint {")
            && controls.contains("deliver_signal_checkpoint("),
        "every backend, including compatibility WASM, must enter the kernel-owned signal checkpoint path"
    );

    let wasm = std::fs::read_to_string(root.join("crates/execution/src/wasm.rs"))
        .expect("read compatibility WASM adapter");
    let wasm_backend_start = wasm
        .find("impl ExecutionBackend for WasmExecution")
        .expect("find compatibility WASM backend adapter");
    let wasm_backend: String = wasm[wasm_backend_start..]
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    assert!(
        wasm_backend.contains("fndeliver_signal_checkpoint(")
            && wasm_backend.contains("self.wake_handle(identity)")
            && wasm_backend.contains("wake.publish_signal(signal,delivery_token)"),
        "compatibility WASM checkpoints must publish through the runtime-neutral kernel wake capability"
    );
}

#[test]
fn common_execution_lifecycle_has_no_backend_specific_signal_or_process_residence_debt() {
    let root = repo_root();
    let files = [
        "crates/execution/src",
        "crates/execution/tests",
        "crates/native-sidecar/src",
        "crates/native-sidecar/tests",
    ]
    .into_iter()
    .flat_map(|relative| {
        production_source_files(&root)
            .into_iter()
            .filter(move |path| path.starts_with(relative))
    })
    .collect::<Vec<_>>();
    let violations = production_matches(
        &root,
        &files,
        &[
            "NodeSignalDispositionAction",
            "NodeSignalHandlerRegistration",
            "WasmSignalDispositionAction",
            "WasmSignalHandlerRegistration",
            "JavascriptHostCall",
            "javascript_host_call",
            "map_node_signal_registration",
            "map_wasm_signal_registration",
            "closed_javascript_event_channel",
            "closed_python_event_channel",
            "closed_wasm_event_channel",
            "EventChannelClosed.to_string()",
        ],
    );
    assert!(
        violations.is_empty(),
        "common lifecycle naming or string-matched channel errors reappeared:\n{}",
        violations.join("\n")
    );

    let sidecar_files = production_source_files(&root)
        .into_iter()
        .filter(|path| path.starts_with("crates/native-sidecar/src"))
        .collect::<Vec<_>>();
    let residence_violations = production_matches(
        &root,
        &sidecar_files,
        &["uses_shared_v8_runtime", ".child_pid()"],
    );
    assert!(
        residence_violations.is_empty(),
        "common sidecar lifecycle must use ExecutionBackend::native_process_id:\n{}",
        residence_violations.join("\n")
    );

    let signals =
        std::fs::read_to_string(root.join("crates/native-sidecar/src/execution/signals.rs"))
            .expect("read native signal mapper");
    assert_eq!(
        signals
            .matches("fn map_execution_signal_registration(")
            .count(),
        1,
        "native sidecar must have exactly one runtime-neutral execution signal mapper"
    );

    let active_execution =
        std::fs::read_to_string(root.join("crates/native-sidecar/src/execution/process.rs"))
            .expect("read ActiveExecution error adapters");
    for exact_mapper in [
        ".map_err(javascript_error)?",
        ".map_err(python_error)?",
        ".map_err(wasm_error)?",
    ] {
        assert!(
            active_execution.contains(exact_mapper),
            "ActiveExecution must preserve typed engine errors through {exact_mapper}"
        );
    }

    let process =
        std::fs::read_to_string(root.join("crates/native-sidecar/src/execution/process_events.rs"))
            .expect("read root process-event recovery");
    let recovery = process
        .find("fn recover_closed_root_runtime_process_event(")
        .map(|offset| &process[offset..])
        .expect("find root channel-close recovery");
    let recovery = recovery
        .split("pub(super) fn active_process_by_path")
        .next()
        .expect("bound root channel-close recovery");
    for backend_branch in [
        "GuestRuntimeKind::",
        "uses_shared_v8_runtime",
        "child_pid()",
    ] {
        assert!(
            !recovery.contains(backend_branch),
            "root channel-close recovery must use native_process_id, not {backend_branch}"
        );
    }
    assert!(recovery.contains("execution.native_process_id()"));

    let child =
        std::fs::read_to_string(root.join("crates/native-sidecar/src/execution/child_process.rs"))
            .expect("read descendant process-event recovery");
    let recovery = child
        .find("fn recover_descendant_runtime_child_process_event(")
        .map(|offset| &child[offset..])
        .expect("find descendant channel-close recovery");
    let recovery = recovery
        .split("fn write_descendant_process_stdin(")
        .next()
        .expect("bound descendant channel-close recovery");
    for backend_branch in [
        "GuestRuntimeKind::",
        "uses_shared_v8_runtime",
        "child_pid()",
    ] {
        assert!(
            !recovery.contains(backend_branch),
            "descendant channel-close recovery must use native_process_id, not {backend_branch}"
        );
    }
    assert!(recovery.contains("execution.native_process_id()"));

    let state = std::fs::read_to_string(root.join("crates/native-sidecar/src/state.rs"))
        .expect("read typed sidecar errors");
    assert!(state.contains("ExecutionEventChannelClosed { backend: ExecutionBackendKind }"));
}

#[test]
fn every_production_active_process_attaches_the_real_kernel_runtime_endpoint() {
    let root = repo_root();

    // The old stub must not remain available for a production call site to
    // select accidentally. Deliberately virtual kernel processes use the same
    // durable RuntimeControlCell without attaching its consumer; once a real
    // backend is installed it is wrapped by ActiveProcess below.
    for relative in [
        "crates/kernel/src",
        "crates/execution/src",
        "crates/native-sidecar/src",
    ] {
        let files = production_source_files(&root)
            .into_iter()
            .filter(|path| path.starts_with(relative))
            .collect::<Vec<_>>();
        for path in files {
            let source = std::fs::read_to_string(root.join(&path))
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            assert!(
                !source.contains("StubDriverProcess"),
                "production runtime endpoint stub reappeared in {}",
                path.display()
            );
        }
    }

    let process_relative = PathBuf::from("crates/native-sidecar/src/execution/process.rs");
    let process = std::fs::read_to_string(root.join(&process_relative))
        .expect("read ActiveProcess implementation");
    let constructor = rust_braced_item(&process, "pub(crate) fn new(");
    let compact_constructor = constructor
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    assert!(
        compact_constructor.contains("Self::attach_runtime_control_before_start(")
            && compact_constructor.contains("Self::new_with_attached_runtime_control("),
        "ordinary ActiveProcess construction must attach and retain the real generation-bound kernel runtime endpoint"
    );
    let preattached_constructor =
        rust_braced_item(&process, "pub(crate) fn new_with_attached_runtime_control(");
    assert!(
        preattached_constructor
            .contains("runtime_control: agentos_kernel::process_runtime::RuntimeControlReceiver")
            && preattached_constructor.contains("runtime_control.identity()")
            && preattached_constructor.contains("kernel_handle.runtime_identity()"),
        "startup construction must accept and validate the receiver attached before engine start"
    );

    let launch =
        std::fs::read_to_string(root.join("crates/native-sidecar/src/execution/launch.rs"))
            .expect("read top-level launch implementation");
    let execute = launch
        .split_once("    pub(crate) async fn execute(")
        .expect("top-level execute implementation")
        .1;
    let allocated = execute
        .find("let kernel_handle = vm\n            .kernel\n            .spawn_process(")
        .expect("top-level kernel process allocation");
    let startup = &execute[allocated..];
    let attach = startup
        .find("ActiveProcess::attach_runtime_control_before_start(")
        .expect("pre-start runtime endpoint attachment");
    let pty_setup = startup
        .find("let tty_master_fd = if requested_tty")
        .expect("top-level PTY setup");
    let engine_start = startup
        .find("let (execution, process_env, started_context) = match resolved.runtime")
        .expect("top-level engine start");
    let publish = startup
        .find("new_with_attached_runtime_control(")
        .expect("pre-attached ActiveProcess publication");
    assert!(
        attach < pty_setup && pty_setup < engine_start && engine_start < publish,
        "the real endpoint must attach before any fallible setup or engine start"
    );
    let fallible_startup = &startup[attach..publish];
    assert!(
        startup[..publish].contains("macro_rules! top_level_start_step")
            && startup[..publish].contains("rollback_failed_top_level_process_start(")
            && !fallible_startup.contains("?;"),
        "every fallible post-allocation setup/start step must use the common rollback funnel"
    );
    let rollback = rust_braced_item(&launch, "fn rollback_failed_top_level_process_start(");
    assert!(
        rollback.contains("execution.terminate()")
            && rollback.contains("kernel_handle.finish(127)")
            && rollback.contains("kernel.waitpid(kernel_handle.pid())"),
        "failed top-level setup must terminate the engine, mark exit, and reap kernel resources"
    );
    let published_rollback =
        rust_braced_item(&launch, "fn rollback_published_top_level_process_start(");
    assert!(
        published_rollback.contains("vm.active_processes.remove(process_id)")
            && published_rollback.contains("rollback_failed_top_level_process_start("),
        "a failure after active-process publication must remove, terminate, and reap the process"
    );
    assert_eq!(
        launch
            .matches("if let Err(error) = self.bridge.emit_lifecycle(&vm_id, LifecycleState::Busy)")
            .count(),
        2,
        "both binding and engine lifecycle publications must handle bridge failure"
    );
    assert_eq!(
        launch
            .matches("rollback_published_top_level_process_start(")
            .count(),
        3,
        "the helper declaration and both lifecycle failure paths must remain wired"
    );

    // Every production backend-installation path attaches its generation-bound
    // runtime endpoint before engine or binding-producer start, then transfers
    // that receiver into ActiveProcess. Test fixtures are ignored.
    // A direct struct literal would bypass endpoint attachment, so it is
    // forbidden outside the declaration and impl header.
    let mut constructors = Vec::new();
    let mut preattached_constructors = Vec::new();
    let mut direct_literals = Vec::new();
    for relative in production_source_files(&root)
        .into_iter()
        .filter(|path| path.starts_with("crates/native-sidecar/src/"))
    {
        let source = std::fs::read_to_string(root.join(&relative))
            .unwrap_or_else(|error| panic!("read {}: {error}", relative.display()));
        let mut tracker = CfgTestTracker::new();
        for (index, raw) in source.lines().enumerate() {
            if tracker.in_test(raw) {
                continue;
            }
            let code = strip_line_comment(raw);
            if code.contains("ActiveProcess::new(") {
                constructors.push(relative.clone());
            }
            if code.contains("ActiveProcess::new_with_attached_runtime_control(") {
                preattached_constructors.push(relative.clone());
            }
            if code.contains("ActiveProcess {")
                && !code.contains("struct ActiveProcess {")
                && !code.trim_start().starts_with("impl ")
            {
                direct_literals.push(format!(
                    "{}:{}: {}",
                    relative.display(),
                    index + 1,
                    code.trim()
                ));
            }
        }
    }
    constructors.sort();
    let top_level_launch = PathBuf::from("crates/native-sidecar/src/execution/launch.rs");
    let top_level_source = std::fs::read_to_string(root.join(&top_level_launch))
        .expect("read top-level launch implementation");
    if top_level_source.contains("ActiveProcess::new_with_attached_runtime_control(") {
        preattached_constructors.push(top_level_launch);
    }
    preattached_constructors.sort();
    preattached_constructors.dedup();
    assert!(
        constructors.is_empty(),
        "production startup must never attach the endpoint after an executor may already be running: {constructors:?}"
    );
    assert_eq!(
        preattached_constructors,
        [
            PathBuf::from("crates/native-sidecar/src/execution/child_process.rs"),
            PathBuf::from("crates/native-sidecar/src/execution/launch.rs"),
        ],
        "only reviewed startup paths may transfer a receiver attached before engine start"
    );
    assert!(
        direct_literals.is_empty(),
        "production ActiveProcess literals bypass endpoint attachment:\n{}",
        direct_literals.join("\n")
    );
}

#[test]
fn neutral_host_contracts_and_shared_capabilities_have_no_engine_types() {
    let root = repo_root();
    let contract_files = production_source_files(&root)
        .into_iter()
        .filter(|path| {
            path.starts_with("crates/execution/src/backend/")
                || path.starts_with("crates/execution/src/host/")
        })
        .collect::<Vec<_>>();

    let mut violations = engine_type_identifier_matches(&root, &contract_files, &[]);

    // These lower-layer host-service models are consumed by every executor.
    // They are deliberately scanned as complete production files because no
    // engine adapter belongs in native-sidecar-core.
    let core_host_files = [
        "crates/native-sidecar-core/src/guest_fs.rs",
        "crates/native-sidecar-core/src/guest_net.rs",
        "crates/native-sidecar-core/src/guest_pty.rs",
        "crates/native-sidecar-core/src/identity.rs",
        "crates/native-sidecar-core/src/signals.rs",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect::<Vec<_>>();
    violations.extend(engine_type_identifier_matches(
        &root,
        &core_host_files,
        &["GuestRuntimeKind", "ExecutionBackendKind"],
    ));

    // host_dispatch/mod.rs starts with the compatibility-wire decoder. Scan
    // the semantic dispatcher half so the adapter may mention its source
    // engine while capability routing and kernel effects may not.
    let dispatcher_relative =
        PathBuf::from("crates/native-sidecar/src/execution/host_dispatch/mod.rs");
    let dispatcher = std::fs::read_to_string(root.join(&dispatcher_relative))
        .unwrap_or_else(|error| panic!("read {}: {error}", dispatcher_relative.display()));
    let semantic_dispatcher = dispatcher
        .find("pub(super) fn dispatch_host_operation(")
        .map(|offset| &dispatcher[offset..])
        .expect("shared host semantic dispatcher marker");
    violations.extend(engine_type_identifiers_in_source(
        &dispatcher_relative,
        semantic_dispatcher,
        &["GuestRuntimeKind", "ExecutionBackendKind"],
    ));

    // Domain files contain compatibility-wire decoders before their shared
    // capability implementation. Scan only the execution half: JavaScript
    // request types are permitted in the explicit adapter decoder, never in
    // the semantic capability that touches kernel state.
    for family in [
        "clock",
        "entropy",
        "filesystem",
        "identity",
        "network",
        "process",
        "signal",
        "terminal",
    ] {
        let relative = PathBuf::from(format!(
            "crates/native-sidecar/src/execution/host_dispatch/{family}.rs"
        ));
        let source = std::fs::read_to_string(root.join(&relative))
            .unwrap_or_else(|error| panic!("read {}: {error}", relative.display()));
        let marker = format!("pub(super) struct {}Capability", to_pascal_case(family));
        let capability = source
            .find(&marker)
            .map(|offset| &source[offset..])
            .unwrap_or_else(|| panic!("missing capability marker {marker}"));
        violations.extend(engine_type_identifiers_in_source(
            &relative,
            capability,
            &["GuestRuntimeKind", "ExecutionBackendKind"],
        ));
    }

    // These files are semantic sidecar reactor owners. Engine-specific request
    // decoding remains allowed only in the explicitly named javascript/* and
    // host_dispatch/network_compat.rs adapters, never in a shared reactor file.
    for relative in [
        "crates/native-sidecar/src/execution/network/tcp.rs",
        "crates/native-sidecar/src/execution/network/udp.rs",
        "crates/native-sidecar/src/execution/network/unix.rs",
        "crates/native-sidecar/src/execution/network/tls.rs",
        "crates/native-sidecar/src/execution/network/dns.rs",
        "crates/native-sidecar/src/execution/network/resolver.rs",
        "crates/native-sidecar/src/execution/network/managed.rs",
        "crates/native-sidecar/src/execution/network/managed_endpoint.rs",
        "crates/native-sidecar/src/execution/network/http_client.rs",
        "crates/native-sidecar/src/execution/network/http2.rs",
    ] {
        let relative = PathBuf::from(relative);
        let source = std::fs::read_to_string(root.join(&relative))
            .unwrap_or_else(|error| panic!("read {}: {error}", relative.display()));
        violations.extend(engine_type_identifiers_in_source(
            &relative,
            &source,
            &["GuestRuntimeKind", "ExecutionBackendKind"],
        ));
    }

    // state.rs also owns executor sessions, so scan the reviewed shared
    // networking model declarations rather than granting or denying the whole
    // mixed-domain file. A newly added field on one of these types is covered
    // automatically by balanced-brace extraction.
    let state_relative = PathBuf::from("crates/native-sidecar/src/state.rs");
    let state = std::fs::read_to_string(root.join(&state_relative))
        .unwrap_or_else(|error| panic!("read {}: {error}", state_relative.display()));
    for marker in [
        "pub(crate) struct SocketDescriptionLease",
        "pub(crate) struct SocketFairnessRetirement",
        "pub(crate) struct ListenerConnectionRetirement",
        "pub(crate) struct HostNetTransferDescription",
        "pub(crate) struct VmDnsConfig",
        "pub(crate) struct SocketPathContext",
        "pub(crate) struct NetworkResourceCounts",
        "pub(crate) struct GuestUnixAddress",
        "pub(crate) struct GuestUnixAddressRegistryEntry",
        "pub(crate) struct HttpLoopbackTarget",
        "pub(crate) enum SocketFamily",
        "pub(crate) struct VmListenPolicy",
        "pub(crate) struct ActiveHttpServer",
        "pub(crate) enum PendingHttpRequest",
        "pub(crate) struct ActiveHttp2State",
        "pub(crate) struct Http2SharedState",
        "pub(crate) struct ActiveHttp2Server",
        "pub(crate) struct ActiveHttp2Session",
        "pub(crate) struct ActiveHttp2Stream",
        "pub(crate) struct QueuedHttp2Event",
        "pub(crate) struct QueuedHttp2Command",
        "pub(crate) struct Http2SocketSnapshot",
        "pub(crate) struct Http2RuntimeSnapshot",
        "pub(crate) struct Http2SessionSnapshot",
        "pub(crate) struct Http2BridgeEvent",
        "pub(crate) enum Http2SessionCommand",
        "pub(crate) enum TcpListenerEvent",
        "pub(crate) struct PendingTcpSocket",
        "pub(crate) enum TcpSocketEvent",
        "pub(crate) struct SocketEventPusher",
        "struct SocketReadinessSubscriber",
        "pub(crate) struct SocketReadinessSubscribers",
        "pub(crate) struct SocketReadinessRegistration",
        "pub(crate) enum KernelSocketReadinessEvent",
        "pub(crate) struct KernelSocketReadinessTarget",
        "pub(crate) struct KernelSocketReadinessRegistryState",
        "pub(crate) struct ActiveTcpSocket",
        "pub(crate) enum NativeTlsCommand",
        "pub(crate) enum NativePlainSocketCommand",
        "pub(crate) struct PlainSocketWritePayload",
        "pub(crate) struct TlsWritePayload",
        "pub(crate) struct ReactorIoLimits",
        "pub(crate) struct LoopbackTlsTransportPair",
        "pub(crate) struct LoopbackTlsTransportPairState",
        "pub(crate) struct LoopbackTlsEndpoint",
        "pub(crate) struct TlsClientHello",
        "pub(crate) struct TlsBridgeOptions",
        "pub(crate) enum TlsMaterial",
        "pub(crate) enum TlsDataValue",
        "pub(crate) struct ActiveTlsState",
        "pub(crate) struct ResolvedTcpConnectAddr",
        "pub(crate) struct ActiveTcpListener",
        "pub(crate) enum UnixListenerEvent",
        "pub(crate) struct PendingUnixSocket",
        "pub(crate) struct GuestUnixConnectionState",
        "pub(crate) struct PendingUnixConnectionGuard",
        "pub(crate) struct ActiveUnixSocket",
        "pub(crate) struct ActiveUnixListener",
        "pub(crate) enum UdpFamily",
        "pub(crate) enum DatagramEvent",
        "pub(crate) struct NativeUdpSendPayload",
        "pub(crate) enum NativeUdpSocketOption",
        "pub(crate) enum NativeUdpCommand",
        "pub(crate) struct ActiveUdpSocket",
        "pub(crate) struct ManagedUdpPollRecheck",
        "pub(crate) enum PendingNetConnect",
        "pub(crate) struct PendingNetConnectState",
        "pub(crate) enum SocketQueryKind",
        "pub(crate) struct ProcNetEntry",
    ] {
        let declaration = rust_braced_item(&state, marker);
        violations.extend(engine_type_identifiers_in_source(
            &state_relative,
            declaration,
            &["GuestRuntimeKind", "ExecutionBackendKind"],
        ));
    }

    assert!(
        violations.is_empty(),
        "runtime-neutral host contracts and capability execution must not contain engine-specific types or executor switchboards; keep those in explicit adapter decoders:\n{}",
        violations.join("\n")
    );
}

fn rust_braced_item<'a>(source: &'a str, marker: &str) -> &'a str {
    let start = source
        .find(marker)
        .unwrap_or_else(|| panic!("missing shared model declaration {marker}"));
    let item = &source[start..];
    let open = item
        .find('{')
        .unwrap_or_else(|| panic!("shared model declaration {marker} has no body"));
    let mut depth = 0_usize;
    for (offset, byte) in item[open..].bytes().enumerate() {
        match byte {
            b'{' => depth = depth.saturating_add(1),
            b'}' => {
                depth = depth
                    .checked_sub(1)
                    .unwrap_or_else(|| panic!("unbalanced shared model declaration {marker}"));
                if depth == 0 {
                    return &item[..open + offset + 1];
                }
            }
            _ => {}
        }
    }
    panic!("unterminated shared model declaration {marker}")
}

fn to_pascal_case(value: &str) -> String {
    let mut characters = value.chars();
    characters
        .next()
        .map(|first| first.to_ascii_uppercase().to_string() + characters.as_str())
        .unwrap_or_default()
}

fn engine_type_identifier_matches(
    root: &Path,
    files: &[PathBuf],
    forbidden_exact: &[&str],
) -> Vec<String> {
    files
        .iter()
        .flat_map(|relative| {
            let source = std::fs::read_to_string(root.join(relative))
                .unwrap_or_else(|error| panic!("read {}: {error}", relative.display()));
            engine_type_identifiers_in_source(relative, &source, forbidden_exact)
        })
        .collect()
}

fn engine_type_identifiers_in_source(
    relative: &Path,
    source: &str,
    forbidden_exact: &[&str],
) -> Vec<String> {
    let mut violations = Vec::new();
    let mut tracker = CfgTestTracker::new();
    for (index, raw) in source.lines().enumerate() {
        if tracker.in_test(raw) {
            continue;
        }
        let code = strip_line_comment(raw);
        for identifier in
            code.split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        {
            let engine_type = ["Javascript", "V8", "Python", "Wasmtime"]
                .iter()
                .any(|prefix| identifier.starts_with(prefix) && identifier.len() > prefix.len());
            if engine_type || forbidden_exact.contains(&identifier) {
                violations.push(format!(
                    "{}:{}: {identifier}",
                    relative.display(),
                    index + 1
                ));
            }
        }
    }
    violations
}

/// Sanity: the scan actually sees source files and the allowlisted files exist.
/// Guards against a refactor silently making the lint scan nothing (which would
/// make it vacuously pass).
#[test]
fn lint_scans_real_sources_and_allowlist_paths_exist() {
    let root = repo_root();
    let files = production_source_files(&root);
    assert!(
        files.len() > 30,
        "expected to scan many source files, found {}",
        files.len()
    );

    let mut missing = Vec::new();
    for class in [FS_ALLOW, NET_ALLOW, PROCESS_ALLOW, ENV_ALLOW] {
        for rel in class {
            let path = root.join(rel);
            let exists = if rel.ends_with('/') {
                path.is_dir()
            } else {
                path.is_file()
            };
            if !exists {
                missing.push(rel.to_string());
            }
        }
    }
    missing.sort();
    missing.dedup();
    assert!(
        missing.is_empty(),
        "allowlist references files that no longer exist (clean them up): {missing:?}"
    );
}

// ---------------------------------------------------------------------------
// Runtime topology and lower-layer dependency guards.
// ---------------------------------------------------------------------------

fn dependency_keys(manifest: &Path) -> BTreeSet<String> {
    let text = std::fs::read_to_string(manifest)
        .unwrap_or_else(|error| panic!("read {manifest:?}: {error}"));
    let mut dependencies = BTreeSet::new();
    let mut in_dependencies = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            in_dependencies = line.contains("dependencies");
            continue;
        }
        if !in_dependencies || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let key = line
            .split(['=', ' ', '\t'])
            .next()
            .unwrap_or("")
            .trim_matches('"');
        if !key.is_empty() {
            dependencies.insert(key.to_owned());
        }
    }
    dependencies
}

#[test]
fn generic_runtime_layers_do_not_depend_on_product_or_acp_layers() {
    let root = repo_root();
    let lower_layers = [
        "resource",
        "runtime",
        "kernel",
        "vfs",
        "vfs-store",
        "v8-runtime",
        "execution",
    ];
    let forbidden = [
        "agentos-protocol",
        "agentos-sidecar-core",
        "agentos-sidecar",
        "agentos-client",
        "agentos-actor-plugin",
    ];
    let mut violations = Vec::new();
    for crate_dir in lower_layers {
        let manifest = root.join("crates").join(crate_dir).join("Cargo.toml");
        let dependencies = dependency_keys(&manifest);
        for dependency in forbidden {
            if dependencies.contains(dependency) {
                violations.push(format!("crates/{crate_dir}: {dependency}"));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "generic runtime layers depend on product/ACP layers:\n{}",
        violations.join("\n")
    );
}

#[test]
fn kernel_dns_contract_has_no_native_resolver_or_runtime_dependency() {
    let root = repo_root();
    let manifest = root.join("crates/kernel/Cargo.toml");
    let dependencies = dependency_keys(&manifest);
    for forbidden in ["hickory-resolver", "tokio"] {
        assert!(
            !dependencies.contains(forbidden),
            "kernel must not depend on native DNS transport crate {forbidden}"
        );
    }

    let kernel_dns = std::fs::read_to_string(root.join("crates/kernel/src/dns.rs"))
        .expect("read kernel DNS contract");
    for forbidden in [
        "agentos_runtime",
        "BlockingJobError",
        "RuntimeContext",
        "hickory_resolver",
        "TokioResolver",
        "tokio::",
    ] {
        assert!(
            !kernel_dns.contains(forbidden),
            "kernel DNS contract contains native resolver/runtime symbol {forbidden}"
        );
    }

    let native_resolver = std::fs::read_to_string(
        root.join("crates/native-sidecar/src/execution/network/resolver.rs"),
    )
    .expect("read native DNS resolver");
    assert!(
        native_resolver.contains("pub(crate) struct HickoryDnsResolver")
            && native_resolver.contains("runtime: RuntimeContext")
            && native_resolver.contains("TokioResolver"),
        "native sidecar must own Hickory/Tokio DNS transport with an injected RuntimeContext"
    );
    let service = std::fs::read_to_string(root.join("crates/native-sidecar/src/service.rs"))
        .expect("read native sidecar service");
    assert!(
        service.contains("HickoryDnsResolver::new(runtime_context.clone())"),
        "native sidecar must inject its one process RuntimeContext into DNS transport"
    );
}

#[test]
fn kernel_resource_accounting_has_no_runtime_or_tokio_dependency_cycle() {
    let root = repo_root();
    let kernel_dependencies = dependency_keys(&root.join("crates/kernel/Cargo.toml"));
    for forbidden in ["agentos-runtime", "tokio"] {
        assert!(
            !kernel_dependencies.contains(forbidden),
            "kernel resource authority must not depend on {forbidden}"
        );
    }

    let runtime_dependencies = dependency_keys(&root.join("crates/runtime/Cargo.toml"));
    assert!(
        !runtime_dependencies.contains("agentos-kernel"),
        "process runtime must not create a runtime -> kernel -> VFS -> runtime cycle"
    );
    assert!(
        runtime_dependencies.contains("agentos-resource")
            && kernel_dependencies.contains("agentos-resource"),
        "kernel and process runtime must share the runtime-neutral accounting layer"
    );

    let resource_dependencies = dependency_keys(&root.join("crates/resource/Cargo.toml"));
    assert_eq!(
        resource_dependencies,
        BTreeSet::from([String::from("event-listener")]),
        "resource accounting must remain independent of executors, Tokio, VFS, and product layers"
    );

    for path in [
        "crates/kernel/src/kernel.rs",
        "crates/kernel/src/socket_table.rs",
    ] {
        let source = std::fs::read_to_string(root.join(path)).expect("read kernel resource owner");
        for forbidden in ["agentos_runtime", "tokio::"] {
            assert!(
                !source.contains(forbidden),
                "{path} contains concrete runtime symbol {forbidden}"
            );
        }
    }
}

#[test]
fn shared_acp_runtime_has_no_adapter_name_policy() {
    let root = repo_root();
    let production = ["mod.rs", "runtime.rs", "restore.rs", "turn.rs"]
        .into_iter()
        .map(|file| {
            let source =
                std::fs::read_to_string(root.join("crates/agentos-sidecar/src/acp").join(file))
                    .unwrap_or_else(|error| panic!("read native ACP module {file}: {error}"));
            source
                .split("#[cfg(test)]")
                .next()
                .unwrap_or(&source)
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n");
    for adapter_name in [
        "\"claude\"",
        "\"codex\"",
        "\"opencode\"",
        "\"pi\"",
        "\"pi-cli\"",
    ] {
        assert!(
            !production.contains(adapter_name),
            "shared ACP runtime must not branch on adapter name {adapter_name}; put launch compatibility in the AgentOS-owned package launcher"
        );
    }
    assert!(
        production.contains("ACP_APPEND_SYSTEM_PROMPT_ENV"),
        "shared ACP runtime must use the adapter-neutral package-launch contract"
    );
}

#[test]
fn typescript_sdk_does_not_ship_a_competing_in_memory_vfs() {
    let root = repo_root();
    for relative_path in [
        "packages/core/src/runtime-compat.ts",
        "packages/core/src/index.ts",
        "packages/core/src/layers.ts",
        "packages/runtime-core/src/node-runtime.ts",
    ] {
        let source = std::fs::read_to_string(root.join(relative_path))
            .unwrap_or_else(|error| panic!("read {relative_path}: {error}"));
        assert!(
            !source.contains("createInMemoryFileSystem")
                && !source.contains("class InMemoryFileSystem")
                && !source.contains("createInMemoryLayerStore"),
            "production TypeScript SDK must not implement or export an in-memory VFS: {relative_path}"
        );
    }
    assert!(
        root.join("packages/runtime-core/src/test-runtime.ts")
            .is_file(),
        "the explicit test-only VFS callback fixture must remain available"
    );
    let low_level_runtime =
        std::fs::read_to_string(root.join("packages/runtime-core/src/node-runtime.ts"))
            .expect("read low-level Node runtime");
    assert!(
        low_level_runtime.contains("filesystem: VirtualFileSystem")
            && low_level_runtime.contains("const filesystem = options.filesystem"),
        "the low-level compatibility runtime must require a caller-owned filesystem instead of creating a TypeScript default"
    );
}

#[test]
fn rust_client_transport_routes_live_events_without_history() {
    let root = repo_root();
    let source = std::fs::read_to_string(root.join("crates/sidecar-client/src/transport.rs"))
        .expect("read Rust sidecar transport");
    for obsolete in [
        "WireEventLog",
        "route_sequence",
        "global_sequence",
        "provisional_process",
    ] {
        assert!(
            !source.contains(obsolete),
            "client transport must not retain replay/history state ({obsolete})"
        );
    }
    assert!(
        source.contains("broadcast::channel(EVENT_CHANNEL_CAPACITY)"),
        "client transport must retain only bounded live event fan-out"
    );
}

fn native_reactor_source_files(root: &Path) -> Vec<PathBuf> {
    production_source_files(root)
        .into_iter()
        .filter(|path| {
            let path = path.to_string_lossy();
            [
                "crates/bridge/",
                "crates/execution/",
                "crates/kernel/",
                "crates/native-sidecar/",
                "crates/native-sidecar-core/",
                "crates/runtime/",
                "crates/sidecar-protocol/",
                "crates/v8-runtime/",
                "crates/vfs/",
                "crates/vfs-store/",
                "crates/vm-config/",
            ]
            .iter()
            .any(|prefix| path.starts_with(prefix))
        })
        .collect()
}

fn native_execution_source_files(root: &Path) -> Vec<PathBuf> {
    production_source_files(root)
        .into_iter()
        .filter(|path| path.starts_with("crates/native-sidecar/src/execution"))
        .collect()
}

fn native_execution_source(root: &Path) -> String {
    native_execution_source_files(root)
        .into_iter()
        .map(|path| {
            std::fs::read_to_string(root.join(&path))
                .unwrap_or_else(|error| panic!("read {path:?}: {error}"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn native_execution_is_split_by_domain() {
    let root = repo_root();
    let expected = [
        "crates/native-sidecar/src/execution/mod.rs",
        "crates/native-sidecar/src/execution/coordinator.rs",
        "crates/native-sidecar/src/execution/launch.rs",
        "crates/native-sidecar/src/execution/process.rs",
        "crates/native-sidecar/src/execution/process_events.rs",
        "crates/native-sidecar/src/execution/child_process.rs",
        "crates/native-sidecar/src/execution/signals.rs",
        "crates/native-sidecar/src/execution/stdio.rs",
        "crates/native-sidecar/src/execution/network/mod.rs",
        "crates/native-sidecar/src/execution/network/tcp.rs",
        "crates/native-sidecar/src/execution/network/unix.rs",
        "crates/native-sidecar/src/execution/network/udp.rs",
        "crates/native-sidecar/src/execution/network/tls.rs",
        "crates/native-sidecar/src/execution/network/http2.rs",
        "crates/native-sidecar/src/execution/network/dns.rs",
        "crates/native-sidecar/src/execution/network/resolver.rs",
        "crates/native-sidecar/src/execution/javascript/mod.rs",
        "crates/native-sidecar/src/execution/javascript/rpc.rs",
        "crates/native-sidecar/src/execution/javascript/crypto.rs",
        "crates/native-sidecar/src/execution/javascript/sqlite.rs",
        "crates/native-sidecar/src/execution/javascript/http.rs",
    ];

    for path in expected {
        assert!(root.join(path).is_file(), "missing execution module {path}");
    }
    assert!(
        !root.join("crates/native-sidecar/src/execution.rs").exists(),
        "the monolithic execution.rs must not be restored"
    );
}

#[test]
fn python_filesystem_and_process_calls_use_common_host_operations() {
    let root = repo_root();
    let adapter = std::fs::read_to_string(root.join("crates/execution/src/python.rs"))
        .expect("read Python execution adapter");
    let mapper =
        std::fs::read_to_string(root.join("crates/native-sidecar/src/execution/process.rs"))
            .expect("read common execution event mapper");
    let filesystem = std::fs::read_to_string(root.join("crates/native-sidecar/src/filesystem.rs"))
        .expect("read sidecar filesystem helpers");
    let state = std::fs::read_to_string(root.join("crates/native-sidecar/src/state.rs"))
        .expect("read shared sidecar state");

    assert!(
        adapter.contains("pub fn try_host_call(")
            && adapter.contains("HostOperation::Filesystem(")
            && adapter.contains("HostOperation::Process(ProcessOperation::RunCaptured"),
        "the Python wire adapter must translate filesystem and captured-process calls into runtime-neutral host operations"
    );
    assert!(
        mapper.contains("python_responder.try_host_call(")
            && mapper.contains("host.submit(call.operation, call.reply.clone(), admission)"),
        "Python filesystem/process requests must enter the same admitted host-operation lane as other executors"
    );
    for removed in [
        "crates/native-sidecar/src/execution/python/mod.rs",
        "crates/native-sidecar/src/execution/python/rpc.rs",
        "crates/native-sidecar/src/execution/python/sockets.rs",
    ] {
        assert!(
            !root.join(removed).exists(),
            "Python semantics must not return to the deleted sidecar dispatcher ({removed})"
        );
    }
    for removed_state in [
        "PythonVfsRpcRequest(Box<",
        "PythonSocketConnectCompletion",
        "python_sockets:",
        "next_python_socket_id:",
    ] {
        assert!(
            !state.contains(removed_state),
            "shared sidecar state must not retain Python-specific semantic state ({removed_state})"
        );
    }
    assert!(
        !filesystem.contains("PythonVfsRpc")
            && !filesystem.contains("handle_python_vfs_rpc_request"),
        "the filesystem source of truth must not contain a Python-specific semantic switch"
    );
    assert!(
        !root
            .join("crates/native-sidecar/src/execution/python/subprocess.rs")
            .exists(),
        "captured subprocess execution belongs to the common process capability, not a Python implementation"
    );
}

#[test]
fn python_common_host_replies_preserve_typed_error_details() {
    let adapter = include_str!("../../execution/src/python.rs");
    let target = adapter
        .split_once("impl DirectHostReplyTarget for PythonHostReplyTarget")
        .expect("Python direct host reply target")
        .1
        .split_once("fn python_host_reply_adapter_error")
        .expect("end Python direct host reply target")
        .0;

    assert!(
        target.contains("respond_claimed_host_error(call_id, error)")
            && target.contains("respond_host_error(call_id, error)"),
        "the Python adapter must forward the complete HostServiceError, including structured details"
    );
    assert!(
        !target.contains("error.code") && !target.contains("error.message"),
        "the Python common reply target must not flatten typed host errors into code/message pairs"
    );
}

#[test]
fn javascript_timer_and_direct_reply_errors_use_typed_classification() {
    let javascript = include_str!("../../execution/src/javascript.rs");
    let timer = javascript
        .split_once("fn timer_dispatch_error(")
        .expect("JavaScript timer error adapter")
        .1
        .split_once("fn javascript_timer_error(")
        .expect("end JavaScript timer error adapter")
        .0;
    assert!(
        timer.contains("error.code")
            && timer.contains("error.message")
            && timer.contains("error.details"),
        "JavaScript timer dispatch must preserve the typed HostServiceError payload"
    );

    let direct_reply = javascript
        .split_once("pub(crate) fn map_host_reply_adapter_response(")
        .expect("JavaScript direct reply adapter")
        .1
        .split_once("fn encode_host_service_error_payload(")
        .expect("end JavaScript direct reply adapter")
        .0;
    assert!(
        direct_reply.contains("BridgeSettlementErrorKind::StaleCompletion"),
        "stale direct replies must be classified by the typed V8 settlement kind"
    );

    for (label, source) in [
        ("timer dispatch", timer),
        ("direct host reply", direct_reply),
    ] {
        for forbidden in [
            ".split(",
            ".split_once(",
            ".starts_with(",
            ".strip_prefix(",
            ".contains(",
        ] {
            assert!(
                !source.contains(forbidden),
                "{label} errors must not infer behavior from diagnostic strings ({forbidden})"
            );
        }
    }
}

#[test]
fn pending_process_event_limits_are_typed_and_actionable() {
    let service = include_str!("../src/service.rs");
    let state = include_str!("../src/state.rs");
    let check = service
        .split_once("fn check_pending_process_event_capacity(")
        .expect("pending process-event capacity check")
        .1
        .split_once("fn ")
        .expect("end pending process-event capacity check")
        .0;

    assert!(
        check.matches("SidecarError::host_resource_limit(").count() >= 2,
        "pending event count and byte limits must return typed resource-limit errors"
    );
    assert!(
        !check.contains("SidecarError::InvalidState"),
        "bounded queue admission failures are resource limits, not invalid internal state"
    );
    let typed_details = state
        .split_once("pub(crate) fn host_resource_limit(")
        .expect("typed host resource-limit helper")
        .1
        .split_once("impl fmt::Display for SidecarError")
        .expect("end typed host resource-limit helper")
        .0;
    for field in ["limitName", "observed", "limit", "configPath"] {
        assert!(
            typed_details.contains(field),
            "pending process-event resource errors must preserve {field} details"
        );
    }
}

fn production_matches(root: &Path, files: &[PathBuf], needles: &[&str]) -> Vec<String> {
    let mut matches = Vec::new();
    for rel in files {
        if is_excluded_file(rel) {
            continue;
        }
        let content = std::fs::read_to_string(root.join(rel))
            .unwrap_or_else(|error| panic!("read {rel:?}: {error}"));
        let mut tracker = CfgTestTracker::new();
        for (index, raw) in content.lines().enumerate() {
            if tracker.in_test(raw) {
                continue;
            }
            let code = strip_line_comment(raw);
            if needles.iter().any(|needle| code.contains(needle)) {
                matches.push(format!("{}:{}: {}", rel.display(), index + 1, raw.trim()));
            }
        }
    }
    matches
}

#[test]
fn native_sidecar_dependency_closure_has_one_tokio_runtime_builder() {
    let root = repo_root();
    let files = native_reactor_source_files(&root);
    let builders = production_matches(
        &root,
        &files,
        &[
            "Builder::new_multi_thread()",
            "Builder::new_current_thread()",
        ],
    );
    assert_eq!(
        builders.len(),
        1,
        "expected exactly one production Tokio runtime builder:\n{}",
        builders.join("\n")
    );
    assert!(
        builders[0].starts_with("crates/runtime/src/lib.rs:"),
        "the one runtime builder must be process-owned: {}",
        builders[0]
    );
}

#[test]
fn production_subsystems_use_injected_runtime_contexts() {
    let root = repo_root();
    let files = native_reactor_source_files(&root)
        .into_iter()
        .filter(|path| path != Path::new("crates/runtime/src/lib.rs"))
        .collect::<Vec<_>>();
    let violations = production_matches(&root, &files, &["SidecarRuntime::process_context("]);
    assert!(
        violations.is_empty(),
        "production subsystems must receive an injected VM/process RuntimeContext:\n{}",
        violations.join("\n")
    );
}

#[test]
fn native_reactor_never_uses_tokios_elastic_blocking_pool() {
    let root = repo_root();
    let files = native_reactor_source_files(&root);
    let violations = production_matches(
        &root,
        &files,
        &[
            "tokio::task::spawn_blocking",
            "spawn_blocking(",
            "block_in_place(",
        ],
    );
    assert!(
        violations.is_empty(),
        "blocking work must use the fixed, byte-admitted sidecar executor:\n{}",
        violations.join("\n")
    );
}

#[test]
fn native_execution_dispatch_never_blocks_on_completion_or_polling() {
    let root = repo_root();
    let files = native_execution_source_files(&root);
    let violations = production_matches(
        &root,
        &files,
        &[
            "recv_timeout(",
            "mpsc::sync_channel(",
            ".wait_timeout(",
            ".poll_event_blocking(",
            "thread::sleep(",
            "std::thread::sleep(",
        ],
    );
    assert!(
        violations.is_empty(),
        "native dispatch must defer async completions and wait on reactor readiness; it may not block or poll:\n{}",
        violations.join("\n")
    );
}

#[test]
fn top_level_python_start_uses_the_async_runtime_adapter() {
    let path = repo_root().join("crates/native-sidecar/src/execution/launch.rs");
    let source =
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path:?}: {error}"));
    let compact = source
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    assert!(
        compact.contains(".python_engine.start_execution_with_runtime_async("),
        "top-level Python startup must await cache materialization and prewarm instead of blocking a Tokio worker"
    );
    assert!(
        source.contains(".bundled_pyodide_dist_path_for_vm_async(&vm_id, &vm.runtime_context)"),
        "top-level Pyodide cache materialization must not run synchronously before the async Python start"
    );
}

#[test]
fn nested_child_start_never_blocks_the_shared_runtime_worker() {
    let source = native_execution_source(&repo_root());

    assert!(
        source.contains("pub(crate) async fn spawn_child_process("),
        "root child startup must be an async sidecar dispatch path"
    );
    assert!(
        source.contains("async fn spawn_descendant_process("),
        "descendant child startup must be an async sidecar dispatch path"
    );
    assert!(
        source
            .matches(".start_execution_with_runtime_async(")
            .count()
            >= 6,
        "top-level plus root/descendant Python and WASM startup must use async runtime adapters"
    );
    assert!(
        !source.contains(".start_execution_with_runtime(\n                            StartPythonExecutionRequest")
            && !source.contains(
                ".start_execution_with_runtime(\n                            StartWasmExecutionRequest",
            ),
        "Python/WASM child startup must not synchronously prewarm on a Tokio worker"
    );
}

#[test]
fn reactor_readiness_never_uses_the_ordinary_stream_event_lane() {
    let root = repo_root();
    let mut files = native_execution_source_files(&root);
    files.extend([
        PathBuf::from("crates/native-sidecar/src/vm.rs"),
        PathBuf::from("crates/execution/src/javascript.rs"),
    ]);
    let violations = production_matches(
        &root,
        &files,
        &[
            "send_stream_event(\"net_socket\"",
            "send_stream_event(\"signal\"",
            "send_javascript_stream_event(\"signal\"",
            "send_stream_event(\"timer\"",
        ],
    );
    assert!(
        violations.is_empty(),
        "socket, protocol, signal, and timer readiness must update durable broker state and publish one coalesced wake; it may not enqueue ordinary per-event messages:\n{}",
        violations.join("\n")
    );
}

#[test]
fn javascript_tcp_receive_path_is_event_driven() {
    let root = repo_root();
    for (relative_path, legacy_poll_markers) in [
        (
            "packages/build-tools/bridge-src/builtins/net.ts",
            &[
                "_netSocketPollRaw",
                "NET_BRIDGE_POLL_DELAY_MS",
                "netBridgePollDelay",
                "setPollDelayMs",
                "scheduleSocketPoll",
                "scheduleServerPoll",
                "net.poll",
                "net.server_poll",
            ][..],
        ),
        (
            "packages/build-tools/bridge-src/builtins/network.ts",
            &["NET_BRIDGE_POLL_DELAY_MS", "netBridgePollDelay"][..],
        ),
        (
            "packages/runtime-benchmarks/src/focused/net-tcp-event-floor.bench.ts",
            &["net-poll-delay-ms", "setPollDelayMs", "pollDelayMs"][..],
        ),
        (
            "crates/execution/src/node_import_cache.rs",
            &[
                "NODE_EXECUTION_RUNNER_SOURCE",
                "root_dir.join(\"runner.mjs\")",
                "createRpcBackedNetModule",
                "scheduleSocketPoll",
                "scheduleServerPoll",
                "net.poll",
                "net.server_poll",
            ][..],
        ),
    ] {
        let path = root.join(relative_path);
        let source =
            std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path:?}: {error}"));
        for legacy_poll_marker in legacy_poll_markers {
            assert!(
                !source.contains(legacy_poll_marker),
                "JavaScript TCP sockets and listeners must consume coalesced sidecar readiness, not a recurring synchronous poll bridge ({legacy_poll_marker}) in {relative_path}"
            );
        }
    }
}

#[test]
fn native_reactor_has_no_unbounded_channels_or_per_io_thread_names() {
    let root = repo_root();
    let files = native_reactor_source_files(&root);
    let violations = production_matches(
        &root,
        &files,
        &[
            "unbounded_channel",
            "crossbeam_channel::unbounded",
            "tcp-socket-reader",
            "unix-socket-reader",
            "kernel-wait-rpc",
            "signal-delivery-thread",
            "http2-runtime-thread",
            "EVENT_PUMP_INTERVAL",
            "remaining.min(Duration::from_millis(10))",
        ],
    );
    assert!(
        violations.is_empty(),
        "native reactor contains forbidden unbounded/thread-per-I/O patterns:\n{}",
        violations.join("\n")
    );
}

#[test]
fn common_network_reactor_has_no_executor_specific_control_or_encoding() {
    let root = repo_root();
    let network_files = native_execution_source_files(&root)
        .into_iter()
        .filter(|path| path.starts_with("crates/native-sidecar/src/execution/network/"))
        .collect::<Vec<_>>();
    let violations = production_matches(
        &root,
        &network_files,
        &[
            "ActiveExecution::",
            "ExecutionBackendKind::",
            "V8SessionHandle",
            "v8_session_handle(",
            "v8_runtime::",
        ],
    );
    assert!(
        violations.is_empty(),
        "shared network ownership must use runtime-neutral wake/reply capabilities; executor selection and engine wire encoding belong in adapters:\n{}",
        violations.join("\n")
    );

    let process_file = vec![PathBuf::from(
        "crates/native-sidecar/src/execution/process.rs",
    )];
    let wake_leaks = production_matches(
        &root,
        &process_file,
        &[
            "V8SessionHandle",
            "ExecutionWakeTarget",
            "v8_session_handle(",
        ],
    );
    assert!(
        wake_leaks.is_empty(),
        "common process orchestration must obtain ExecutionWakeHandle from the backend contract instead of constructing an engine session target:\n{}",
        wake_leaks.join("\n")
    );

    let lifecycle = std::fs::read_to_string(root.join("crates/execution/src/backend/lifecycle.rs"))
        .expect("read execution backend lifecycle contract");
    assert!(
        lifecycle.contains(
            "fn wake_handle(&self, _identity: ExecutionWakeIdentity) -> Option<ExecutionWakeHandle>"
        ),
        "the executor backend contract must own construction of its runtime-neutral wake capability"
    );
    assert!(
        lifecycle.contains("WebAssembly")
            && !lifecycle.contains("CompatibilityWasm")
            && !lifecycle.contains("Wasmtime"),
        "common lifecycle kinds must identify the WebAssembly language backend without predeclaring an engine"
    );

    let unix =
        std::fs::read_to_string(root.join("crates/native-sidecar/src/execution/network/unix.rs"))
            .expect("read Unix reactor source");
    assert!(
        unix.contains(
            "target.pending_connections.len() >= target.pending_connection_limit"
        ) && unix.contains("listener_accept_capacity(backlog, reactor_limits)"),
        "Unix pre-accept metadata must be admitted against the same bounded listener capacity as its completion lane"
    );
}

#[test]
fn native_reactor_tasks_enter_through_task_supervision() {
    let root = repo_root();
    let files = native_reactor_source_files(&root)
        .into_iter()
        // This is the sole implementation of the supervised spawn API. Its
        // Handle::spawn calls run only after TaskSupervisor admission.
        .filter(|path| path != Path::new("crates/runtime/src/lib.rs"))
        .collect::<Vec<_>>();
    let violations = production_matches(
        &root,
        &files,
        &[
            "tokio::spawn(",
            "tokio::task::spawn(",
            "Handle::current().spawn(",
            ".handle().spawn(",
            ".handle.spawn(",
        ],
    );
    assert!(
        violations.is_empty(),
        "native reactor tasks must enter through RuntimeContext's supervised spawn API:\n{}",
        violations.join("\n")
    );
}

#[test]
fn v8_platform_worker_pool_has_a_reviewed_fixed_bound() {
    let root = repo_root();
    let path = root.join("crates/v8-runtime/src/isolate.rs");
    let source =
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path:?}: {error}"));
    assert!(
        source.contains("const V8_PLATFORM_WORKER_THREADS: u32 = 4;")
            && source.contains("v8::new_default_platform(V8_PLATFORM_WORKER_THREADS, false)"),
        "V8's internal platform workers must use the reviewed fixed four-thread bound"
    );
}

#[test]
fn production_threads_match_the_reviewed_topology_manifest() {
    const MANIFEST: &[(&str, &str)] = &[
        ("blocking-executor-worker", "crates/runtime/src/lib.rs"),
        (
            "constant-v8-platform-owner",
            "crates/v8-runtime/src/isolate.rs",
        ),
        (
            "embedded-v8-dispatch",
            "crates/v8-runtime/src/embedded_runtime.rs",
        ),
        (
            "embedded-v8-writer",
            "crates/v8-runtime/src/embedded_runtime.rs",
        ),
        ("bounded-v8-warm-worker", "crates/v8-runtime/src/session.rs"),
        (
            "admitted-v8-session-executor",
            "crates/v8-runtime/src/session.rs",
        ),
        (
            "serialized-v8-maintenance",
            "crates/execution/src/v8_host.rs",
        ),
        (
            "constant-stdio-writer",
            "crates/native-sidecar/src/stdio.rs",
        ),
        (
            "constant-stdio-reader",
            "crates/native-sidecar/src/stdio.rs",
        ),
        ("constant-heartbeat", "crates/native-sidecar/src/stdio.rs"),
    ];

    let root = repo_root();
    let mut observed = BTreeSet::new();
    let mut unmarked = Vec::new();
    // This census covers every production crate, not only the reactor's
    // dependency closure. ACP/session or client-side support code runs in the
    // same sidecar process and may not introduce an unreviewed OS thread either.
    for rel in production_source_files(&root) {
        if is_excluded_file(&rel) {
            continue;
        }
        let content = std::fs::read_to_string(root.join(&rel))
            .unwrap_or_else(|error| panic!("read {rel:?}: {error}"));
        let lines = content.lines().collect::<Vec<_>>();
        let mut tracker = CfgTestTracker::new();
        for (index, raw) in lines.iter().enumerate() {
            if tracker.in_test(raw) {
                continue;
            }
            let code = strip_line_comment(raw);
            if ![
                "thread::spawn(",
                "std::thread::spawn(",
                "thread::Builder::new()",
                "std::thread::Builder::new()",
            ]
            .iter()
            .any(|needle| code.contains(needle))
            {
                continue;
            }
            let marker = lines[index.saturating_sub(3)..index]
                .iter()
                .rev()
                .find_map(|line| line.split("AGENTOS_THREAD_SITE: ").nth(1))
                .map(str::trim);
            match marker {
                Some(marker) => {
                    observed.insert((marker.to_owned(), rel.to_string_lossy().replace('\\', "/")));
                }
                None => unmarked.push(format!("{}:{}: {}", rel.display(), index + 1, raw.trim())),
            }
        }
    }

    assert!(
        unmarked.is_empty(),
        "production OS thread sites must carry a reviewed AGENTOS_THREAD_SITE marker:\n{}",
        unmarked.join("\n")
    );
    let expected = MANIFEST
        .iter()
        .map(|(marker, path)| ((*marker).to_owned(), (*path).to_owned()))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        observed, expected,
        "production thread topology changed without updating the reviewed manifest"
    );
}

#[test]
fn javascript_dgram_receive_path_is_event_driven() {
    let path = repo_root().join("packages/build-tools/bridge-src/builtins/dgram.ts");
    let source =
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path:?}: {error}"));
    for legacy_poll_marker in ["_receivePollTimer", "NET_BRIDGE_POLL_DELAY_MS"] {
        assert!(
            !source.contains(legacy_poll_marker),
            "JavaScript dgram receive must wait for coalesced sidecar readiness, not recurring polling ({legacy_poll_marker})"
        );
    }
}

#[test]
fn javascript_http2_receive_path_is_event_driven() {
    let path = repo_root().join("packages/build-tools/bridge-src/builtins/http2.ts");
    let source =
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path:?}: {error}"));
    for legacy_poll_marker in ["fallbackTimer", "setTimeout(tick"] {
        assert!(
            !source.contains(legacy_poll_marker),
            "JavaScript HTTP/2 receive must wait for coalesced sidecar readiness, not recurring polling ({legacy_poll_marker})"
        );
    }
}

#[test]
fn protocol_and_abort_delivery_have_no_recurring_poll_timer() {
    let root = repo_root();
    for (relative_path, forbidden) in [
        (
            "crates/agentos-sidecar/src/acp/runtime.rs",
            &["ACP_JSON_RPC_POLL_INTERVAL", "remaining.min(ACP_"][..],
        ),
        (
            "crates/native-sidecar/src/stdio.rs",
            &["write_rx.recv_timeout(Duration::from_millis(5))"][..],
        ),
        (
            "packages/build-tools/bridge-src/builtins/http.ts",
            &["_startAbortSignalPoll", "_signalPollTimer"][..],
        ),
        (
            "packages/build-tools/bridge-src/builtins/fs.ts",
            &[
                "setTimeout(attemptKernelStdinRead",
                "setTimeout(attemptRead",
                "_kernelStdinRead.apply(void 0, [length, 100]",
            ][..],
        ),
        (
            "packages/build-tools/bridge-src/builtins/stdin.ts",
            &["_kernelStdinRead.apply(void 0, [65536, 100]"][..],
        ),
    ] {
        let path = root.join(relative_path);
        let source =
            std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path:?}: {error}"));
        for marker in forbidden {
            assert!(
                !source.contains(marker),
                "protocol/abort delivery must wait on a direct event notification, not recurring polling ({marker}) in {relative_path}"
            );
        }
    }
}

#[test]
fn standalone_wasm_wait_has_no_recurring_adapter_poll() {
    let path = repo_root().join("crates/execution/src/wasm.rs");
    let source =
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path:?}: {error}"));
    for marker in [
        "self.poll_event_blocking(Duration::from_millis(50))",
        "Sample elapsed budget each poll",
    ] {
        assert!(
            !source.contains(marker),
            "standalone WASM waits must block on readiness with one deadline-aware wait, not a recurring adapter poll ({marker})"
        );
    }
    assert!(
        source.contains("fn wait_event_blocking("),
        "standalone WASM wait must retain its direct readiness/deadline wait helper"
    );
}

#[test]
fn browser_sources_are_retained_but_disabled_from_native_build_and_publish_gates() {
    let root = repo_root();
    for relative_path in [
        "crates/agentos-sidecar-core/src",
        "crates/agentos-sidecar-browser/src",
        "crates/native-sidecar-browser/src",
        "packages/browser/src",
        "packages/runtime-browser/src",
    ] {
        assert!(
            root.join(relative_path).is_dir(),
            "browser migration source must remain retained at {relative_path}"
        );
    }

    for relative_path in [
        "crates/agentos-sidecar-browser/src/lib.rs",
        "crates/native-sidecar-browser/src/lib.rs",
        "packages/browser/src/index.ts",
        "packages/runtime-browser/src/index.ts",
    ] {
        let source = std::fs::read_to_string(root.join(relative_path))
            .unwrap_or_else(|error| panic!("read {relative_path}: {error}"));
        assert!(
            source.contains("AGENTOS_BROWSER_SUPPORT_DISABLED"),
            "browser public entrypoint must remain disabled: {relative_path}"
        );
        assert!(
            source.lines().any(|line| line.trim() == "/*"),
            "browser public entrypoint source must remain commented out: {relative_path}"
        );
        if relative_path.ends_with(".rs") {
            assert!(
                source.trim_end().ends_with("*/"),
                "disabled Rust browser entrypoint must contain no active items after its retained source: {relative_path}"
            );
        } else {
            assert!(
                source.trim_end().ends_with("export {};"),
                "disabled TypeScript browser entrypoint must expose only an empty module: {relative_path}"
            );
        }
    }

    let workspace =
        std::fs::read_to_string(root.join("Cargo.toml")).expect("read workspace Cargo.toml");
    assert!(
        workspace.contains("exclude = [\"software\", \"crates/agentos-sidecar-core\"]"),
        "the obsolete browser-only ACP state machine must remain outside the native workspace"
    );
    let obsolete_core =
        std::fs::read_to_string(root.join("crates/agentos-sidecar-core/Cargo.toml"))
            .expect("read obsolete browser-only ACP core manifest");
    assert!(
        obsolete_core.contains("publish = false"),
        "the obsolete browser-only ACP state machine must not be publishable"
    );
    let default_members = workspace
        .split("default-members = [")
        .nth(1)
        .and_then(|tail| tail.split(']').next())
        .expect("workspace must declare default-members while browser is disabled");
    for browser_crate in [
        "crates/agentos-sidecar-browser",
        "crates/native-sidecar-browser",
    ] {
        assert!(
            workspace.contains(&format!("\"{browser_crate}\"")),
            "retained browser crate must remain a workspace member: {browser_crate}"
        );
        assert!(
            !default_members.contains(browser_crate),
            "disabled browser crate entered Cargo default-members: {browser_crate}"
        );
        let manifest = std::fs::read_to_string(root.join(browser_crate).join("Cargo.toml"))
            .unwrap_or_else(|error| panic!("read {browser_crate}/Cargo.toml: {error}"));
        assert!(
            manifest
                .lines()
                .any(|line| line.trim() == "publish = false"),
            "disabled browser crate must not be publishable: {browser_crate}"
        );
    }

    for browser_package in ["packages/browser", "packages/runtime-browser"] {
        let manifest = std::fs::read_to_string(root.join(browser_package).join("package.json"))
            .unwrap_or_else(|error| panic!("read {browser_package}/package.json: {error}"));
        assert!(
            manifest.contains("\"private\": true"),
            "disabled browser package must remain private: {browser_package}"
        );
    }

    let publish_discovery =
        std::fs::read_to_string(root.join("scripts/publish/src/lib/packages.ts"))
            .expect("read npm publish discovery");
    for package in [
        "@rivet-dev/agentos-browser",
        "@rivet-dev/agentos-runtime-browser",
    ] {
        assert!(
            publish_discovery.contains(&format!("\"{package}\"")),
            "disabled browser package must remain explicitly denied by publish discovery: {package}"
        );
    }

    for relative_path in [
        "package.json",
        ".github/workflows/ci.yml",
        ".github/workflows/publish.yaml",
    ] {
        let source = std::fs::read_to_string(root.join(relative_path))
            .unwrap_or_else(|error| panic!("read {relative_path}: {error}"));
        for package in [
            "!@rivet-dev/agentos-browser",
            "!@rivet-dev/agentos-runtime-browser",
        ] {
            assert!(
                source.contains(package),
                "{relative_path} must explicitly filter disabled package {package}"
            );
        }
    }

    for relative_path in [
        ".github/workflows/ci.yml",
        ".github/workflows/ci-nightly.yml",
        "scripts/ci.sh",
    ] {
        let source = std::fs::read_to_string(root.join(relative_path))
            .unwrap_or_else(|error| panic!("read {relative_path}: {error}"));
        for browser_crate in ["agentos-sidecar-browser", "agentos-native-sidecar-browser"] {
            assert!(
                source.contains(&format!("--exclude {browser_crate}")),
                "{relative_path} must exclude disabled Rust crate {browser_crate}"
            );
        }
    }

    let mirror_generator =
        std::fs::read_to_string(root.join("scripts/generate-agentos-mirror.mjs"))
            .expect("read compatibility mirror generator");
    assert!(
        mirror_generator.contains("browserShim ? { private: true } : {}")
            && mirror_generator.contains("browserShim ? \"publish = false\" : \"\""),
        "generated browser compatibility shims must remain private and unpublishable"
    );
    for relative_path in [".github/workflows/ci.yml", "scripts/ci.sh"] {
        let source = std::fs::read_to_string(root.join(relative_path))
            .unwrap_or_else(|error| panic!("read {relative_path}: {error}"));
        assert!(
            source.contains("node --test scripts/generate-agentos-mirror.test.mjs"),
            "{relative_path} must enforce compatibility-mirror reproducibility"
        );
    }
}

#[test]
fn nightly_runs_explicit_churn_and_multi_vm_soak_gates() {
    let nightly = std::fs::read_to_string(repo_root().join(".github/workflows/ci-nightly.yml"))
        .expect("read nightly workflow");
    for test_name in [
        "multi_vm_generation_soak_has_no_accounting_or_scheduler_drift",
        "multi_vm_protocol_faults_reconcile_shared_runtime_soak",
    ] {
        assert!(
            nightly.contains(test_name),
            "nightly workflow must invoke ignored closure gate {test_name}"
        );
    }
    assert!(
        nightly.matches("--ignored").count() >= 2,
        "nightly workflow must explicitly opt into both expensive closure gates"
    );
}

#[test]
fn javascript_child_process_receive_path_is_event_driven() {
    let root = repo_root();
    for (relative_path, legacy_poll_markers) in [
        (
            "packages/build-tools/bridge-src/builtins/child-process.ts",
            &[
                "_childProcessPoll",
                "scheduleChildProcessPoll",
                "pumpDetachedChildBootstrap",
            ][..],
        ),
        (
            "crates/execution/src/node_import_cache.rs",
            &["scheduleSyntheticChildPoll", "child_process.poll"][..],
        ),
    ] {
        let path = root.join(relative_path);
        let source =
            std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path:?}: {error}"));
        for legacy_poll_marker in legacy_poll_markers {
            assert!(
                !source.contains(legacy_poll_marker),
                "JavaScript child_process output and exit must arrive through bounded/coalesced sidecar events, not a recurring synchronous poll bridge ({legacy_poll_marker}) in {relative_path}"
            );
        }
    }
}

#[test]
fn reactor_completion_paths_do_not_silently_drop_settlement() {
    let root = repo_root();
    let native_execution = native_execution_source(&root);
    for marker in ["let _ = respond_to.send", "let _ = pending.respond_to.send"] {
        assert!(
            !native_execution.contains(marker),
            "reactor completion/control settlement must classify stale/coalesced delivery or log it; found {marker:?} in native execution modules"
        );
    }
    for (relative_path, forbidden) in [
        (
            "crates/v8-runtime/src/session.rs",
            &[
                "limits.javascript.sessionCommandQueue",
                "runtime.protocol.maxEgressFrames",
                "let _ = entry.shutdown_tx.try_send",
                "let _ = crate::bridge::resolve_pending_promise",
            ][..],
        ),
        (
            "crates/execution/src/javascript.rs",
            &[
                "let _ = v8_session.send_bridge_response",
                "let _ = self.v8_session.send_stream_event",
                "cbor_payload_to_json_args(&payload).unwrap_or_default",
                "json_to_cbor_payload(&response).unwrap_or_default",
                "encode_host_service_error_payload(&error).unwrap_or_else",
                "getrandom(&mut bytes).is_err",
                "timers.lock().ok()",
            ][..],
        ),
        (
            "crates/execution/src/backend/submission.rs",
            &["let _ = reply.fail"][..],
        ),
        (
            "crates/execution/src/host/mod.rs",
            &["let _ = reply.fail"][..],
        ),
        (
            "crates/native-sidecar-core/src/guest_net.rs",
            &["let _ = kernel.socket_close"][..],
        ),
        (
            "crates/native-sidecar/src/execution/javascript/sqlite.rs",
            &[
                "let _ = connection.pragma_update",
                "let _ = database\n        .connection\n        .execute_batch",
            ][..],
        ),
        (
            "crates/native-sidecar/src/execution/javascript/rpc.rs",
            &["let _ = socket.close", "let _ = listener.close"][..],
        ),
        (
            "crates/native-sidecar/src/execution/network/udp.rs",
            &["let _ = close_kernel_socket_idempotent"][..],
        ),
        (
            "crates/native-sidecar/src/execution/network/unix.rs",
            &["let _ = stream.shutdown"][..],
        ),
        (
            "crates/native-sidecar/src/execution/process_events.rs",
            &["let _ = fs::write", "let _ = vm.kernel.wait_and_reap"][..],
        ),
        (
            "crates/native-sidecar/src/filesystem.rs",
            &["let _ = fs::write"][..],
        ),
        (
            "crates/native-sidecar/src/service.rs",
            &["self.permissions.lock().ok()?"][..],
        ),
    ] {
        let path = root.join(relative_path);
        let source =
            std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path:?}: {error}"));
        for marker in forbidden {
            assert!(
                !source.contains(marker),
                "reactor completion/control settlement must classify stale/coalesced delivery or log it; found {marker:?} in {relative_path}"
            );
        }
    }
}

#[test]
fn structured_audit_delivery_failures_have_a_non_recursive_stderr_fallback() {
    let root = repo_root();
    let service_source = std::fs::read_to_string(root.join("crates/native-sidecar/src/service.rs"))
        .expect("read native-sidecar service source");
    assert!(
        !service_source.contains("let _ = emit_structured_event("),
        "structured audit failures must not be silently discarded in service.rs"
    );
    assert!(
        !native_execution_source(&root).contains("let _ = emit_structured_event("),
        "structured audit failures must not be silently discarded in native execution modules"
    );
    let service = std::fs::read_to_string(root.join("crates/native-sidecar/src/service.rs"))
        .expect("read native-sidecar service source");
    let fallback = service
        .split("fn emit_structured_event_or_stderr")
        .nth(1)
        .and_then(|tail| tail.split("pub(crate) fn structured_event_frame").next())
        .expect("locate structured-event stderr fallback");
    assert!(fallback.contains("eprintln!"));
    assert!(fallback.contains("ERR_AGENTOS_STRUCTURED_EVENT"));
    assert!(
        !fallback.contains("emit_log"),
        "telemetry failure fallback must not recurse through bridge telemetry"
    );
}

#[test]
fn python_native_tcp_connect_uses_the_common_managed_network_operation() {
    let root = repo_root();
    let source = std::fs::read_to_string(root.join("crates/execution/src/python.rs"))
        .expect("read Python execution adapter");
    let socket_connect_arm = source
        .split("PythonVfsRpcMethod::SocketConnect =>")
        .nth(1)
        .and_then(|tail| tail.split("PythonVfsRpcMethod::SocketSend =>").next())
        .expect("locate Python SocketConnect arm");
    assert!(
        socket_connect_arm.contains("HostOperation::Network(NetworkOperation::ManagedConnect"),
        "Python TCP connect must normalize to the common managed-network operation"
    );
    assert!(
        socket_connect_arm.contains("PythonHostReplyKind::SocketCreated(reservation)"),
        "the Python adapter must retain only its bounded guest-handle reservation"
    );
    assert!(
        !root
            .join("crates/native-sidecar/src/execution/python/sockets.rs")
            .exists(),
        "Python must not retain a parallel native TCP dispatcher"
    );
}

#[test]
fn native_udp_has_one_descriptor_owner_and_no_readiness_clone() {
    let execution = std::fs::read_to_string(
        repo_root().join("crates/native-sidecar/src/execution/network/udp.rs"),
    )
    .expect("read native-sidecar UDP source");
    let state = std::fs::read_to_string(repo_root().join("crates/native-sidecar/src/state.rs"))
        .expect("read native-sidecar state source");
    let owner_task = execution
        .split("struct NativeUdpOwnerTask")
        .nth(1)
        .and_then(|tail| tail.split("async fn run_native_udp_owner").next())
        .expect("locate native UDP task ownership record");
    for required in [
        "socket: tokio::net::UdpSocket",
        "commands: TokioReceiver<NativeUdpCommand>",
        "registration: NativeUdpOwnerRegistration",
    ] {
        assert!(
            owner_task.contains(required),
            "UDP task ownership record is missing {required}"
        );
    }
    let owner = execution
        .split("async fn run_native_udp_owner")
        .nth(1)
        .and_then(|tail| tail.split("fn spawn_native_udp_owner").next())
        .expect("locate native UDP owner task");

    for required in [
        "receive_queue",
        "reserve_udp_receive_buffer",
        "resources.capacity_changed()",
        "socket.try_recv_from",
        "limits.datagram_quantum.min(limits.operation_quantum)",
        "tokio::task::yield_now().await",
    ] {
        assert!(owner.contains(required), "UDP owner is missing {required}");
    }
    let spawn = execution
        .split("fn spawn_native_udp_owner")
        .nth(1)
        .and_then(|tail| tail.split("impl ActiveUdpSocket").next())
        .expect("locate native UDP owner registration");
    for required in [
        "tokio::net::UdpSocket::from_std(socket)",
        "registration.limits.max_handle_commands.max(1)",
        "tokio_channel(capacity)",
        "TaskClass::Udp",
    ] {
        assert!(
            spawn.contains(required),
            "UDP owner spawn is missing {required}"
        );
    }
    assert!(
        execution.contains("if !wake_pending.swap(true, Ordering::AcqRel)")
            && execution.contains("push_socket_event(event_pusher, event)"),
        "native UDP readiness must coalesce to one pending cross-boundary wake"
    );
    let udp_impl = execution
        .split("impl ActiveUdpSocket")
        .nth(1)
        .expect("locate ActiveUdpSocket implementation");
    assert!(
        !execution.contains("spawn_native_udp_readiness") && !udp_impl.contains("try_clone()"),
        "native UDP must not split readiness and I/O across descriptor clones"
    );
    let active_udp = state
        .split("pub(crate) struct ActiveUdpSocket")
        .nth(1)
        .and_then(|tail| {
            tail.split(
                "// ---------------------------------------------------------------------------",
            )
            .next()
        })
        .expect("locate ActiveUdpSocket");
    assert!(
        active_udp.contains("native_commands: Option<TokioSender<NativeUdpCommand>>")
            && !active_udp.contains("UdpSocket"),
        "the process registry must retain only the owner mailbox, never a native descriptor"
    );

    let connect = udp_impl
        .split("fn connect<B>")
        .nth(1)
        .and_then(|tail| tail.split("fn disconnect").next())
        .expect("locate UDP connect implementation");
    let kernel_branch = connect
        .split("if use_kernel_loopback")
        .nth(1)
        .and_then(|tail| tail.split("self.submit_native_value_command").next())
        .expect("locate VM-local UDP connect branch");
    assert!(
        kernel_branch.contains("socket_connect_udp_loopback")
            && kernel_branch.contains("kernel_connected_remote_addr")
            && kernel_branch.contains("ActiveUdpValueResult::Immediate")
            && !kernel_branch.contains("ensure_native_owner"),
        "VM-local connected UDP must remain taskless and must not activate the native owner"
    );

    let kernel = std::fs::read_to_string(repo_root().join("crates/kernel/src/kernel.rs"))
        .expect("read kernel source");
    let kernel_connect = kernel
        .split("pub fn socket_connect_udp_loopback")
        .nth(1)
        .and_then(|tail| tail.split("pub fn socket_disconnect_udp").next())
        .expect("locate kernel UDP connect implementation");
    assert!(
        kernel_connect.contains("connect_bound_udp_socket")
            && !kernel_connect.contains("tokio::")
            && !kernel_connect.contains("spawn"),
        "kernel UDP connect must be table state only, with no task or runtime"
    );
}

#[test]
fn python_bridge_error_classification_uses_typed_codes_only() {
    let runner = std::fs::read_to_string(
        repo_root().join("crates/execution/assets/runners/python-runner.mjs"),
    )
    .expect("read Python runner");
    let classifier = runner
        .split_once("def _agentos_raise_from_error(error):")
        .expect("Python bridge error classifier")
        .1
        .split_once("def _agentos_bridge_error(error):")
        .expect("end of Python bridge error classifier")
        .0;

    assert!(classifier.contains("code in (\"EACCES\", \"EPERM\")"));
    assert!(classifier.contains("code == \"ENOENT\""));
    assert!(classifier.contains("exception.code = code"));
    assert!(classifier.contains("exception.details = details"));
    assert!(
        !classifier.contains(" in message") && !classifier.contains("message.lower("),
        "Python exception classes must never be inferred from engine-specific error strings"
    );

    let bridge_normalizer = runner
        .split_once("function normalizePythonBridgeError(error) {")
        .expect("Python bridge error normalizer")
        .1
        .split_once("function createPythonBridgeRpcBridge() {")
        .expect("end of Python bridge error normalizer")
        .0;
    assert!(bridge_normalizer.contains("typeof error?.code === 'string'"));
    assert!(bridge_normalizer.contains("normalized.code = structuredCode"));
    assert!(bridge_normalizer.contains(": 'EIO'"));
    assert!(bridge_normalizer.contains("normalized.details = error.details"));
    for forbidden in [
        "separatorIndex",
        "message.indexOf(",
        "message.slice(",
        ".test(code)",
    ] {
        assert!(
            !bridge_normalizer.contains(forbidden),
            "Python bridge errno must come from error.code, not diagnostic parsing ({forbidden})"
        );
    }

    let socket_classifier = runner
        .split_once("def _agentos_socket_oserror(exc):")
        .expect("Python socket error classifier")
        .1
        .split_once("def _agentos_socket_rpc(call):")
        .expect("end of Python socket error classifier")
        .0;
    assert!(socket_classifier.contains("code_name = getattr(exc, \"code\", None)"));
    assert!(socket_classifier.contains("_agentos_errno.EIO"));
    assert!(socket_classifier.contains("mapped = OSError(errno_value, message)"));
    assert!(socket_classifier.contains("mapped.details = details"));
    for forbidden in [
        "message.split(",
        "message.lower(",
        "message.upper(",
        " in message",
        "head =",
    ] {
        assert!(
            !socket_classifier.contains(forbidden),
            "Python socket errno must come from exc.code, not diagnostic parsing ({forbidden})"
        );
    }

    let filesystem_classifier = runner
        .split_once("  function createFsError(error) {")
        .expect("Python filesystem error classifier")
        .1
        .split_once("  function withFsErrors(operation) {")
        .expect("end of Python filesystem error classifier")
        .0;
    assert!(filesystem_classifier.contains("typeof error?.code === 'string'"));
    assert!(filesystem_classifier.contains("ERRNO_CODES[code]"));
    assert!(filesystem_classifier.contains("ERRNO_CODES.EIO"));
    assert!(filesystem_classifier.contains("mapped.code = code || 'EIO'"));
    assert!(filesystem_classifier.contains("mapped.message ="));
    assert!(filesystem_classifier.contains("mapped.details = error.details"));
    for forbidden in [
        ".toLowerCase(",
        ".toUpperCase(",
        ".test(message)",
        "message.includes(",
    ] {
        assert!(
            !filesystem_classifier.contains(forbidden),
            "Python filesystem errno must come from error.code, not diagnostic parsing ({forbidden})"
        );
    }
}

#[test]
fn reactor_event_errors_preserve_typed_codes() {
    let root = repo_root();
    for relative in [
        "crates/native-sidecar/src/execution/network/managed.rs",
        "crates/native-sidecar/src/execution/javascript/rpc.rs",
        "crates/native-sidecar/src/execution/network/tcp.rs",
    ] {
        let source = std::fs::read_to_string(root.join(relative)).expect("read reactor adapter");
        for forbidden in [
            "SidecarError::Execution(format!(\"{code}: {message}\"))",
            "SidecarError::Execution(format!(\"{detail}: {message}\"))",
        ] {
            assert!(
                !source.contains(forbidden),
                "{relative} must carry reactor error codes structurally, not encode them into diagnostics"
            );
        }
    }

    let managed = std::fs::read_to_string(
        root.join("crates/native-sidecar/src/execution/network/managed.rs"),
    )
    .expect("read runtime-neutral network adapter");
    assert!(
        managed
            .matches("code.as_deref().unwrap_or(\"EIO\")")
            .count()
            >= 3,
        "runtime-neutral read and accept errors need stable typed codes"
    );
}
