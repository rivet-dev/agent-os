mod support;

use agentos_bridge::{LoadFilesystemStateRequest, PersistenceBridge};
use agentos_native_sidecar::wire::{
    DisposeReason, DisposeVmRequest, EventPayload, GuestRuntimeKind, KillProcessRequest,
    OpenSessionRequest, RequestPayload, ResponsePayload, SidecarPlacement, SidecarPlacementShared,
    StreamChannel,
};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use support::{
    assert_node_available, authenticate_wire, create_vm_wire, execute_wire, new_sidecar,
    open_session_wire, temp_dir, wire_connection, wire_request, wire_session, wire_vm,
    write_fixture, RecordingBridge,
};

const PROCESS_OUTPUT_BYTE_LIMIT: usize = 1024 * 1024;

fn wait_for_process_exit(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<RecordingBridge>,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
) -> i32 {
    let ownership = wire_vm(connection_id, session_id, vm_id);
    let deadline = Instant::now() + Duration::from_secs(10);

    loop {
        let event = sidecar
            .poll_event_wire_blocking(&ownership, Duration::from_millis(100))
            .expect("poll sidecar wire process exit");
        let Some(event) = event else {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for process exit"
            );
            continue;
        };

        match event.payload {
            EventPayload::ProcessExitedEvent(exited) if exited.process_id == process_id => {
                return exited.exit_code;
            }
            _ => {}
        }
    }
}

fn kill_process_terminates_running_guest_execution() {
    assert_node_available();

    let mut sidecar = new_sidecar("kill-process");
    let cwd = temp_dir("kill-process-cwd");
    let entry = cwd.join("hang.mjs");
    write_fixture(&entry, "setInterval(() => {}, 1000);\n");

    let connection_id = authenticate_wire(&mut sidecar, "conn-1");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );

    execute_wire(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-hang",
        GuestRuntimeKind::JavaScript,
        &entry,
        Vec::new(),
    );

    let kill = sidecar
        .dispatch_wire_blocking(wire_request(
            5,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::KillProcessRequest(KillProcessRequest {
                process_id: String::from("proc-hang"),
                signal: String::from("SIGTERM"),
            }),
        ))
        .expect("kill guest process");

    match kill.response.payload {
        ResponsePayload::ProcessKilledResponse(response) => {
            assert_eq!(response.process_id, "proc-hang");
        }
        other => panic!("unexpected kill response: {other:?}"),
    }

    let exit_code = wait_for_process_exit(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-hang",
    );
    assert_eq!(exit_code, 143);

    let rerun = cwd.join("rerun.mjs");
    write_fixture(&rerun, "console.log('rerun-ok');\n");
    execute_wire(
        &mut sidecar,
        6,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-rerun",
        GuestRuntimeKind::JavaScript,
        &rerun,
        Vec::new(),
    );
    let (stdout, stderr, rerun_exit) = collect_kill_cleanup_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-rerun",
    );
    assert_eq!(stdout, "rerun-ok\n");
    assert!(stderr.is_empty());
    assert_eq!(rerun_exit, 0);

    // Exercise the Execute -> immediate SIGTERM admission race repeatedly on
    // one warm VM. The signal must not overtake Execute and get discarded by
    // the session's pre-execution readiness branch.
    for attempt in 0..8 {
        let process_id = format!("proc-immediate-term-{attempt}");
        execute_wire(
            &mut sidecar,
            7 + attempt * 2,
            &connection_id,
            &session_id,
            &vm_id,
            &process_id,
            GuestRuntimeKind::JavaScript,
            &entry,
            Vec::new(),
        );

        let kill = sidecar
            .dispatch_wire_blocking(wire_request(
                8 + attempt * 2,
                wire_vm(&connection_id, &session_id, &vm_id),
                RequestPayload::KillProcessRequest(KillProcessRequest {
                    process_id: process_id.clone(),
                    signal: String::from("SIGTERM"),
                }),
            ))
            .expect("immediately kill guest process");
        assert!(matches!(
            kill.response.payload,
            ResponsePayload::ProcessKilledResponse(_)
        ));

        let exit_code = wait_for_process_exit(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            &process_id,
        );
        assert_eq!(exit_code, 143, "immediate SIGTERM attempt {attempt}");
    }
}

fn sigkill_synthesizes_exit_for_shared_v8_guest_execution() {
    assert_node_available();

    let mut sidecar = new_sidecar("kill-process-sigkill");
    let cwd = temp_dir("kill-process-sigkill-cwd");
    let entry = cwd.join("hang.mjs");
    write_fixture(&entry, "setInterval(() => {}, 1000);\n");

    let connection_id = authenticate_wire(&mut sidecar, "conn-1");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );

    execute_wire(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-sigkill",
        GuestRuntimeKind::JavaScript,
        &entry,
        Vec::new(),
    );

    let kill = sidecar
        .dispatch_wire_blocking(wire_request(
            5,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::KillProcessRequest(KillProcessRequest {
                process_id: String::from("proc-sigkill"),
                signal: String::from("SIGKILL"),
            }),
        ))
        .expect("SIGKILL guest process");

    match kill.response.payload {
        ResponsePayload::ProcessKilledResponse(response) => {
            assert_eq!(response.process_id, "proc-sigkill");
        }
        other => panic!("unexpected kill response: {other:?}"),
    }

    let exit_code = wait_for_process_exit(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-sigkill",
    );
    assert_eq!(exit_code, 128 + 9);
}

fn collect_kill_cleanup_process_output(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<RecordingBridge>,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
) -> (String, String, i32) {
    let ownership = wire_session(connection_id, session_id);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut exit = None;

    loop {
        let event = sidecar
            .poll_event_wire_blocking(&ownership, Duration::from_millis(100))
            .expect("poll kill-cleanup wire process event");
        if let Some(event) = event {
            assert_eq!(event.ownership, wire_vm(connection_id, session_id, vm_id));

            match event.payload {
                EventPayload::ProcessOutputEvent(output) if output.process_id == process_id => {
                    match output.channel {
                        StreamChannel::Stdout => append_process_output(
                            &mut stdout,
                            &output.chunk,
                            &output.process_id,
                            "stdout",
                        ),
                        StreamChannel::Stderr => append_process_output(
                            &mut stderr,
                            &output.chunk,
                            &output.process_id,
                            "stderr",
                        ),
                    }
                }
                EventPayload::ProcessExitedEvent(exited) if exited.process_id == process_id => {
                    exit = Some((exited.exit_code, Instant::now()));
                }
                EventPayload::ProcessOutputEvent(_)
                | EventPayload::ProcessExitedEvent(_)
                | EventPayload::ExecutionOutputEvent(_)
                | EventPayload::ExecutionCompletedEvent(_)
                | EventPayload::VmLifecycleEvent(_)
                | EventPayload::StructuredEvent(_)
                | EventPayload::ExtEnvelope(_) => {}
            }
        }

        if let Some((exit_code, seen_at)) = exit {
            if Instant::now().duration_since(seen_at) >= Duration::from_millis(200) {
                return (stdout, stderr, exit_code);
            }
        }

        assert!(
            Instant::now() < deadline,
            "timed out waiting for kill-cleanup process {process_id}\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}

fn append_process_output(buffer: &mut String, chunk: &[u8], process_id: &str, channel: &str) {
    let text = String::from_utf8_lossy(chunk);
    assert!(
        buffer.len().saturating_add(text.len()) <= PROCESS_OUTPUT_BYTE_LIMIT,
        "kill-cleanup process {process_id} exceeded {PROCESS_OUTPUT_BYTE_LIMIT} bytes on {channel}"
    );
    buffer.push_str(&text);
}

fn kill_process_terminates_running_wasm_execution() {
    // Timeout-dependent: the infinite-loop wasm module's prewarm runs into the
    // ~30s V8 CPU-time watchdog before the kill lands, so this case takes ~30s
    // regardless. Gate it to the nightly timing lane (AGENTOS_RUN_TIMING_TESTS=1)
    // rather than pay 30s on every PR. See CLAUDE.md > Testing.
    if std::env::var_os("AGENTOS_RUN_TIMING_TESTS").is_none() {
        return;
    }
    assert_node_available();

    let mut sidecar = new_sidecar("kill-process-wasm");
    let cwd = temp_dir("kill-process-wasm-cwd");
    let entry = cwd.join("hang.wasm");
    write_fixture(
        &entry,
        wat::parse_str(
            r#"
(module
  (func $_start (export "_start")
    (loop $loop
      br $loop
    )
  )
)
"#,
        )
        .expect("compile wasm hang fixture"),
    );

    let connection_id = authenticate_wire(&mut sidecar, "conn-1");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::WebAssembly,
        &cwd,
    );

    execute_wire(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-hang-wasm",
        GuestRuntimeKind::WebAssembly,
        &entry,
        Vec::new(),
    );

    let kill = sidecar
        .dispatch_wire_blocking(wire_request(
            5,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::KillProcessRequest(KillProcessRequest {
                process_id: String::from("proc-hang-wasm"),
                signal: String::from("SIGTERM"),
            }),
        ))
        .expect("kill guest wasm process");

    match kill.response.payload {
        ResponsePayload::ProcessKilledResponse(response) => {
            assert_eq!(response.process_id, "proc-hang-wasm");
        }
        other => panic!("unexpected kill response: {other:?}"),
    }

    let exit_code = wait_for_process_exit(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-hang-wasm",
    );
    assert_ne!(exit_code, 0);
}

fn dispose_vm_succeeds_even_when_a_guest_process_is_running() {
    assert_node_available();

    let mut sidecar = new_sidecar("dispose-vm-running-process");
    let cwd = temp_dir("dispose-vm-running-process-cwd");
    let entry = cwd.join("hang.mjs");
    write_fixture(&entry, "setInterval(() => {}, 1000);\n");

    let connection_id = authenticate_wire(&mut sidecar, "conn-1");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );

    execute_wire(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-hang",
        GuestRuntimeKind::JavaScript,
        &entry,
        Vec::new(),
    );

    let dispose = sidecar
        .dispatch_wire_blocking(wire_request(
            5,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::DisposeVmRequest(DisposeVmRequest {
                reason: DisposeReason::Requested,
            }),
        ))
        .expect("dispose vm with running process");

    match dispose.response.payload {
        ResponsePayload::VmDisposedResponse(response) => {
            assert_eq!(response.vm_id, vm_id);
        }
        other => panic!("unexpected dispose response: {other:?}"),
    }
    assert!(dispose
        .events
        .iter()
        .any(|event| matches!(event.payload, EventPayload::ProcessExitedEvent(_))));

    let (_, replacement_vm) = create_vm_wire(
        &mut sidecar,
        6,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );
    match replacement_vm.response.payload {
        ResponsePayload::VmCreatedResponse(_) => {}
        other => panic!("unexpected replacement vm response: {other:?}"),
    }

    sidecar
        .with_bridge_mut(|bridge: &mut RecordingBridge| {
            let snapshot = bridge
                .load_filesystem_state(LoadFilesystemStateRequest {
                    vm_id: vm_id.clone(),
                })
                .expect("load persisted snapshot");
            assert!(
                snapshot.is_some(),
                "disposed vm should flush a filesystem snapshot"
            );
        })
        .expect("inspect persistence bridge");
}

fn close_session_removes_the_session_and_disposes_owned_vms() {
    let mut sidecar = new_sidecar("close-session");
    let cwd = temp_dir("close-session-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-1");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );

    let events = sidecar
        .close_session_blocking(&connection_id, &session_id)
        .expect("close owned session");
    assert!(events
        .iter()
        .any(|event| { format!("{:?}", event.payload).contains("Disposed") }));

    let create_after_close = sidecar
        .dispatch_wire_blocking(wire_request(
            4,
            wire_session(&connection_id, &session_id),
            RequestPayload::CreateVmRequest(
                agentos_native_sidecar::wire::CreateVmRequest::legacy_test_config(
                    GuestRuntimeKind::JavaScript,
                    HashMap::from([(String::from("cwd"), cwd.to_string_lossy().into_owned())]),
                    agentos_native_sidecar::wire::RootFilesystemDescriptor {
                        mode: agentos_native_sidecar::wire::RootFilesystemMode::Ephemeral,
                        disable_default_base_layer: false,
                        lowers: Vec::new(),
                        bootstrap_entries: Vec::new(),
                    },
                    None,
                ),
            ),
        ))
        .expect("dispatch closed-session create_vm");
    match create_after_close.response.payload {
        ResponsePayload::RejectedResponse(rejected) => {
            assert_eq!(rejected.code, "invalid_state");
            assert!(rejected.message.contains("unknown sidecar session"));
        }
        other => panic!("unexpected closed-session create_vm response: {other:?}"),
    }

    let reopened = sidecar
        .dispatch_wire_blocking(wire_request(
            5,
            wire_connection(&connection_id),
            RequestPayload::OpenSessionRequest(OpenSessionRequest {
                placement: SidecarPlacement::SidecarPlacementShared(SidecarPlacementShared {
                    pool: None,
                }),
                metadata: HashMap::new(),
            }),
        ))
        .expect("open replacement session");
    match reopened.response.payload {
        ResponsePayload::SessionOpenedResponse(_) => {}
        other => panic!("unexpected session reopen response: {other:?}"),
    }

    sidecar
        .with_bridge_mut(|bridge: &mut RecordingBridge| {
            let snapshot = bridge
                .load_filesystem_state(LoadFilesystemStateRequest {
                    vm_id: vm_id.clone(),
                })
                .expect("load persisted snapshot");
            assert!(
                snapshot.is_some(),
                "closing a session should dispose its VMs"
            );
        })
        .expect("inspect persistence bridge");
}

fn remove_connection_disposes_owned_sessions_and_vms() {
    let mut sidecar = new_sidecar("remove-connection");
    let cwd = temp_dir("remove-connection-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-1");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );

    let events = sidecar
        .remove_connection_blocking(&connection_id)
        .expect("remove authenticated connection");
    assert!(events
        .iter()
        .any(|event| { format!("{:?}", event.payload).contains("Disposed") }));

    let reopened = sidecar
        .dispatch_wire_blocking(wire_request(
            5,
            wire_connection(&connection_id),
            RequestPayload::OpenSessionRequest(OpenSessionRequest {
                placement: SidecarPlacement::SidecarPlacementShared(SidecarPlacementShared {
                    pool: None,
                }),
                metadata: HashMap::new(),
            }),
        ))
        .expect("attempt open session after connection removal");
    match reopened.response.payload {
        ResponsePayload::RejectedResponse(rejected) => {
            assert_eq!(rejected.code, "invalid_state");
            assert!(rejected.message.contains("has not authenticated"));
        }
        other => panic!("unexpected post-removal open-session response: {other:?}"),
    }

    sidecar
        .with_bridge_mut(|bridge: &mut RecordingBridge| {
            let snapshot = bridge
                .load_filesystem_state(LoadFilesystemStateRequest {
                    vm_id: vm_id.clone(),
                })
                .expect("load persisted snapshot");
            assert!(
                snapshot.is_some(),
                "removing a connection should dispose its VMs"
            );
        })
        .expect("inspect persistence bridge");
}

#[test]
fn kill_cleanup_suite() {
    // Multiple libtest cases in this V8-backed integration binary still trip
    // teardown/init crashes, so keep the coverage in one top-level suite.
    //
    // cargo-nextest runs every `#[test]` in its OWN process, so that crash cannot
    // occur there — `mod kill_cleanup_split` below exposes each case as a separate
    // test that nextest runs in parallel across cores. Skip the collapsed run under
    // nextest so the work isn't done twice; the split cases skip themselves under
    // `cargo test`. nextest sets `NEXTEST=1` in each test process; libtest/`cargo
    // test` does not.
    if std::env::var_os("NEXTEST").is_some() {
        return;
    }
    close_session_removes_the_session_and_disposes_owned_vms();
    dispose_vm_succeeds_even_when_a_guest_process_is_running();
    kill_process_terminates_running_guest_execution();
    sigkill_synthesizes_exit_for_shared_v8_guest_execution();
    kill_process_terminates_running_wasm_execution();
    remove_connection_disposes_owned_sessions_and_vms();
}

/// Per-case split of `kill_cleanup_suite` for cargo-nextest (process-per-test).
///
/// Each `#[test]` here runs exactly one case in its own process, so the
/// shared-process V8 teardown/init crash that forces the collapsed
/// `kill_cleanup_suite` above does not apply, and the cases run in parallel across
/// cores instead of serially. Each case skips itself under plain `cargo test`
/// (where `NEXTEST` is unset) so the collapsed suite owns the run there; the
/// collapsed suite likewise skips under nextest so the work isn't duplicated.
mod kill_cleanup_split {
    macro_rules! nextest_cases {
        ($($case:ident),+ $(,)?) => {
            $(
                #[test]
                fn $case() {
                    // Covered by the collapsed `super::kill_cleanup_suite` under `cargo test`.
                    if std::env::var_os("NEXTEST").is_none() {
                        return;
                    }
                    super::$case();
                }
            )+
        };
    }

    nextest_cases!(
        close_session_removes_the_session_and_disposes_owned_vms,
        dispose_vm_succeeds_even_when_a_guest_process_is_running,
        kill_process_terminates_running_guest_execution,
        sigkill_synthesizes_exit_for_shared_v8_guest_execution,
        kill_process_terminates_running_wasm_execution,
        remove_connection_disposes_owned_sessions_and_vms,
    );
}
