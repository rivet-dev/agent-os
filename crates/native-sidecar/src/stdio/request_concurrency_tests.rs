//! Authoritative request-concurrency coverage for the production protocol
//! engine. These tests enter through the same decoded-frame routing queues as
//! real stdio; they intentionally do not call `route_protocol_frame` directly.

use super::*;
use crate::wire::ExtEnvelope;
use std::collections::{BTreeMap, BTreeSet};
use std::future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{Notify, Semaphore};

const NAMESPACE: &str = "dev.rivet.agentos.test.protocol-loop";
const TEST_TIMEOUT: Duration = Duration::from_secs(2);

struct Gate {
    started: AtomicBool,
    started_notify: Notify,
    release: Semaphore,
    cancel: crate::request_operations::OperationCancellation,
}

impl Default for Gate {
    fn default() -> Self {
        Self {
            started: AtomicBool::new(false),
            started_notify: Notify::new(),
            release: Semaphore::new(0),
            cancel: crate::request_operations::OperationCancellation::new(),
        }
    }
}

impl Gate {
    async fn wait_started(&self) {
        tokio::time::timeout(TEST_TIMEOUT, async {
            loop {
                let notified = self.started_notify.notified();
                if self.started.load(Ordering::Acquire) {
                    return;
                }
                notified.await;
            }
        })
        .await
        .expect("operation reached its deterministic start gate");
    }

    fn mark_started(&self) {
        self.started.store(true, Ordering::Release);
        self.started_notify.notify_waiters();
    }

    fn release(&self) {
        self.release.add_permits(1);
    }
}

#[derive(Default)]
struct GatedExtensionState {
    gates: Mutex<BTreeMap<String, Arc<Gate>>>,
}

impl GatedExtensionState {
    fn gate(&self, name: &str) -> Arc<Gate> {
        let mut gates = self.gates.lock().expect("gated extension state");
        Arc::clone(
            gates
                .entry(name.to_owned())
                .or_insert_with(|| Arc::new(Gate::default())),
        )
    }
}

struct GatedExtension {
    state: Arc<GatedExtensionState>,
}

impl Extension for GatedExtension {
    fn namespace(&self) -> &str {
        NAMESPACE
    }

    fn request_class(&self, payload: &[u8]) -> ExtensionRequestClass {
        if payload.starts_with(b"cancel:") || payload.starts_with(b"progress-events:") {
            ExtensionRequestClass::Progress
        } else {
            ExtensionRequestClass::Ordinary
        }
    }

    fn handle_request<'a>(
        &'a self,
        _ctx: crate::ExtensionContext,
        payload: Vec<u8>,
    ) -> crate::ExtensionFuture<'a, crate::ExtensionResponse> {
        Box::pin(async move {
            let mut command = String::from_utf8(payload).map_err(|error| {
                SidecarError::InvalidState(format!("invalid test extension command: {error}"))
            })?;
            if let Some(keyed) = command.strip_prefix("key:") {
                let (_, inner) = keyed.split_once(':').ok_or_else(|| {
                    SidecarError::InvalidState(String::from("invalid keyed test command"))
                })?;
                command = inner.to_owned();
            }
            if command == "panic" {
                panic!("deterministic protocol-loop extension panic");
            }
            if let Some(batch) = command
                .strip_prefix("events:")
                .or_else(|| command.strip_prefix("progress-events:"))
            {
                let events = (0..3)
                    .map(|index| {
                        let mut detail = std::collections::HashMap::new();
                        detail.insert(String::from("batch"), batch.to_owned());
                        detail.insert(String::from("index"), index.to_string());
                        crate::service::structured_event_frame("conn", "request-batch", detail)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                return crate::ExtensionResponse::with_wire_events(
                    format!("events:{batch}").into_bytes(),
                    events,
                );
            }
            if let Some(name) = command.strip_prefix("cancel:") {
                self.state
                    .gate(name)
                    .cancel
                    .signal(OperationCancellationReason::Explicit);
                return Ok(crate::ExtensionResponse::new(
                    format!("cancel-ack:{name}").into_bytes(),
                ));
            }
            if let Some(name) = command.strip_prefix("block:") {
                let gate = self.state.gate(name);
                gate.mark_started();
                let outcome = tokio::select! {
                    permit = gate.release.acquire() => {
                        permit.expect("test gate remains open").forget();
                        format!("released:{name}")
                    }
                    _ = gate.cancel.cancelled() => format!("cancelled:{name}"),
                };
                return Ok(crate::ExtensionResponse::new(outcome.into_bytes()));
            }
            if let Some(name) = command.strip_prefix("hang:") {
                self.state.gate(name).mark_started();
                future::pending::<()>().await;
                unreachable!("hanging operation is aborted by protocol drain");
            }
            Ok(crate::ExtensionResponse::new(command.into_bytes()))
        })
    }
}

struct ProtocolLoopHarness {
    ordinary_tx: Sender<Result<Option<AccountedProtocolFrame>, String>>,
    control_tx: Sender<AccountedProtocolFrame>,
    shutdown_tx: Sender<wire::ControlFrame>,
    write_error_tx: Sender<String>,
    callback_transport: Arc<FrameSidecarRequestTransport>,
    writer: ProtocolFrameWriter,
    output: Arc<ProtocolOutputQueue>,
    ingress_budget: ProtocolBudget,
    control_budget: ProtocolBudget,
    extension_routes: Arc<BTreeMap<String, Arc<dyn Extension>>>,
    extension_services: Arc<RoutedExtensionServices>,
    ordinary_service_capacity: usize,
    ownership_coordinator: OwnershipCoordinator,
    operations: OperationTable,
    progress_requests: ProgressOperationView,
}

impl ProtocolLoopHarness {
    fn build(
        state: Arc<GatedExtensionState>,
        connections: &[(&str, &[&str])],
        max_requests: usize,
        max_request_bytes: usize,
    ) -> (Self, ProtocolEngine) {
        let config = NativeSidecarConfig::default();
        let runtime = agentos_runtime::SidecarRuntime::process(&config.runtime)
            .expect("process runtime for protocol-loop harness");
        let runtime_context = runtime.context();
        let mut protocol = config.runtime.protocol.clone();
        protocol.shutdown_grace_ms = 25;

        let ingress_budget = ProtocolBudget::new(
            ProtocolBudgetConfig {
                max_frames: protocol.max_ingress_frames,
                max_bytes: protocol.max_ingress_bytes,
                frame_path: "runtime.protocol.maxIngressFrames",
                byte_path: "runtime.protocol.maxIngressBytes",
                label: "test ordinary ingress",
                metric: agentos_runtime::metrics::ChannelMetricClass::StdioIngress,
            },
            runtime_context.metrics().clone(),
        );
        let control_budget = ProtocolBudget::new(
            ProtocolBudgetConfig {
                max_frames: protocol.max_control_frames,
                max_bytes: protocol.max_control_bytes,
                frame_path: "runtime.protocol.maxControlFrames",
                byte_path: "runtime.protocol.maxControlBytes",
                label: "test progress/control ingress",
                metric: agentos_runtime::metrics::ChannelMetricClass::StdioIngress,
            },
            runtime_context.metrics().clone(),
        );
        let mut sidecar = NativeSidecar::with_config_extensions_and_runtime(
            LocalBridge::default(),
            config.clone(),
            vec![Box::new(GatedExtension { state })],
            runtime_context.clone(),
        )
        .expect("native sidecar for protocol-loop harness");
        let extension_routes = Arc::new(sidecar.extensions.clone());
        let ownership_coordinator = OwnershipCoordinator::from_runtime_config(&config.runtime);
        for (connection_id, sessions) in connections {
            let connection = ownership_coordinator
                .register_connection(*connection_id)
                .expect("register test connection coordinator");
            for session_id in *sessions {
                connection
                    .open_session(*session_id)
                    .expect("register test session coordinator");
            }
        }

        let operations = OperationTable::new(crate::request_operations::RequestOperationLimits {
            max_requests,
            max_request_bytes,
        });
        let progress_requests = operations.progress_requests();
        let output = Arc::new(ProtocolOutputQueue::new(
            protocol.max_egress_frames,
            protocol.max_control_frames,
        ));
        let writer = ProtocolFrameWriter::new(
            Arc::clone(&output),
            WireFrameCodec::new(config.max_frame_bytes),
            &protocol,
            runtime_context.metrics().clone(),
        )
        .expect("protocol output broker for harness");
        let callback_transport = Arc::new(FrameSidecarRequestTransport::new(
            writer.clone(),
            FrameSidecarRequestLimits::from_config(&config),
        ));
        sidecar.set_sidecar_request_transport(callback_transport.clone());

        let (ordinary_tx, ordinary_rx) = channel(protocol.max_ingress_frames);
        let (control_tx, control_rx) = channel(protocol.max_control_frames);
        let (shutdown_tx, shutdown_rx) = channel(MAX_SHUTDOWN_QUEUE);
        let (write_error_tx, write_error_rx) = channel(MAX_TRANSPORT_ERROR_QUEUE);
        let (_limit_warning_tx, limit_warning_rx) = channel(MAX_LIMIT_WARNING_QUEUE);
        let (event_ready_tx, event_ready_rx) = channel(MAX_EVENT_READY_QUEUE);
        let process_event_notify = Arc::clone(&sidecar.process_event_notify);
        let routed_process_event_notify = Arc::new(Notify::new());
        let service_capacity = max_requests
            .saturating_add(protocol.max_control_frames)
            .max(1);
        let (service_tx, service_rx) = channel(service_capacity);
        let (progress_service_tx, progress_service_rx) =
            channel(protocol.max_progress_frames.max(1));
        let routed_extension_services = Arc::new(
            RoutedExtensionServices::new_with_process_event_broker_and_progress(
                service_tx,
                progress_service_tx,
                Arc::clone(&routed_process_event_notify),
                sidecar.process_event_broker(),
            ),
        );
        let extension_services: Arc<dyn ExtensionServices> = routed_extension_services.clone();
        let (extension_completion_tx, extension_completion_rx) = channel(service_capacity);
        let (extension_service_completion_tx, extension_service_completion_rx) =
            channel(service_capacity);
        let (request_completion_tx, request_completion_rx) = channel(max_requests.max(1));

        let harness = Self {
            ordinary_tx,
            control_tx,
            shutdown_tx,
            write_error_tx,
            callback_transport: Arc::clone(&callback_transport),
            writer: writer.clone(),
            output: Arc::clone(&output),
            ingress_budget,
            control_budget,
            extension_routes,
            extension_services: routed_extension_services,
            ordinary_service_capacity: service_capacity,
            ownership_coordinator: ownership_coordinator.clone(),
            operations: operations.clone(),
            progress_requests: progress_requests.clone(),
        };
        let engine = ProtocolEngine {
            protocol,
            sidecar,
            extension_services,
            ownership_coordinator,
            request_operations: operations,
            progress_requests,
            callback_transport,
            frame_writer: writer,
            stdin_rx: ordinary_rx,
            stdin_control_rx: control_rx,
            shutdown_rx,
            extension_service_rx: service_rx,
            progress_service_rx,
            extension_service_completion_tx,
            extension_service_completion_rx,
            extension_completion_tx,
            extension_completion_rx,
            request_completion_tx,
            request_completion_rx,
            limit_warning_rx,
            event_ready_tx,
            event_ready_rx,
            process_event_notify,
            routed_process_event_notify,
            write_error_rx,
        };
        (harness, engine)
    }

    fn send_request(&self, frame: RequestFrame, encoded_bytes: usize) {
        assert_eq!(
            route_decoded_combined_frame(
                DecodedProtocolFrame {
                    frame: ProtocolFrame::RequestFrame(frame),
                    encoded_bytes,
                },
                &self.ordinary_tx,
                &self.callback_transport,
                &self.control_tx,
                &self.shutdown_tx,
                &self.writer,
                &self.ingress_budget,
                &self.control_budget,
                &self.extension_routes,
            ),
            StdinReaderFlow::Continue,
        );
    }

    fn shutdown(&self, reason: &str) {
        assert_eq!(
            route_decoded_combined_frame(
                DecodedProtocolFrame {
                    frame: ProtocolFrame::ControlFrame(wire::ControlFrame {
                        schema: wire::protocol_schema(),
                        payload: wire::ControlPayload::ShutdownControl(wire::ShutdownControl {
                            reason: reason.to_owned(),
                        }),
                    }),
                    encoded_bytes: 1,
                },
                &self.ordinary_tx,
                &self.callback_transport,
                &self.control_tx,
                &self.shutdown_tx,
                &self.writer,
                &self.ingress_budget,
                &self.control_budget,
                &self.extension_routes,
            ),
            StdinReaderFlow::Continue,
        );
    }

    async fn response(&self) -> ResponseFrame {
        let encoded = tokio::time::timeout(TEST_TIMEOUT, self.output.recv_control())
            .await
            .expect("protocol engine produced a response before deadline")
            .expect("protocol output remains open");
        let frame = self
            .writer
            .codec
            .decode(&encoded.bytes)
            .expect("decode protocol-loop response");
        let ProtocolFrame::ResponseFrame(response) = frame else {
            panic!("expected response frame, got {frame:?}");
        };
        response
    }

    fn fill_ordinary_output(&self) -> usize {
        let mut admitted = 0usize;
        loop {
            let event = crate::service::structured_event_frame(
                "conn",
                "filler",
                std::collections::HashMap::new(),
            )
            .expect("build filler event");
            match self.writer.try_send(ProtocolFrame::EventFrame(event)) {
                Ok(()) => admitted = admitted.saturating_add(1),
                Err(ProtocolTrySendError::Full(_)) => return admitted,
                Err(error) => panic!("unexpected filler output failure: {error}"),
            }
        }
    }

    fn ordinary_frame(&self) -> ProtocolFrame {
        let encoded = self
            .output
            .recv_ordinary()
            .expect("ordinary protocol output remains open");
        self.writer
            .codec
            .decode(&encoded.bytes)
            .expect("decode ordinary protocol-loop output")
    }
}

fn extension_request(
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    command: &str,
) -> RequestFrame {
    request_frame(
        request_id,
        session_ownership(connection_id, session_id),
        RequestPayload::ExtEnvelope(ExtEnvelope {
            namespace: NAMESPACE.to_owned(),
            payload: command.as_bytes().to_vec(),
        }),
    )
}

fn guest_filesystem_request(
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    operation: wire::GuestFilesystemOperation,
    path: &str,
    content: Option<&str>,
) -> RequestFrame {
    request_frame(
        request_id,
        vm_ownership(connection_id, session_id, vm_id),
        RequestPayload::GuestFilesystemCallRequest(wire::GuestFilesystemCallRequest {
            operation,
            path: path.to_owned(),
            destination_path: None,
            target: None,
            content: content.map(str::to_owned),
            encoding: None,
            recursive: false,
            max_depth: None,
            mode: None,
            uid: None,
            gid: None,
            atime_ms: None,
            mtime_ms: None,
            len: None,
            offset: None,
        }),
    )
}

async fn start_protocol_loop_with_vm(
    state: Arc<GatedExtensionState>,
) -> (
    ProtocolLoopHarness,
    tokio::task::JoinHandle<Result<(), Box<dyn Error>>>,
    String,
    String,
    String,
) {
    let (harness, engine) = ProtocolLoopHarness::build(state, &[], 8, 16 * 1024);
    let engine_task = tokio::task::spawn_local(run_protocol_engine(engine));
    harness.send_request(
        request_frame(
            1,
            connection_ownership("client-hint"),
            RequestPayload::AuthenticateRequest(wire::AuthenticateRequest {
                client_name: String::from("VM concurrency regression"),
                auth_token: String::new(),
                protocol_version: wire::PROTOCOL_VERSION,
                bridge_version: agentos_bridge::bridge_contract().version,
            }),
        ),
        1,
    );
    let authenticated = harness.response().await;
    let ResponsePayload::AuthenticatedResponse(authenticated) = authenticated.payload else {
        panic!("expected authenticated response");
    };
    let connection_id = authenticated.connection_id;
    harness.send_request(
        request_frame(
            2,
            connection_ownership(&connection_id),
            RequestPayload::OpenSessionRequest(wire::OpenSessionRequest {
                placement: wire::SidecarPlacement::SidecarPlacementShared(
                    wire::SidecarPlacementShared { pool: None },
                ),
                metadata: Default::default(),
            }),
        ),
        1,
    );
    let opened = harness.response().await;
    let ResponsePayload::SessionOpenedResponse(opened) = opened.payload else {
        panic!("expected session-opened response");
    };
    let session_id = opened.session_id;
    let vm_config: agentos_vm_config::CreateVmConfig =
        serde_json::from_value(serde_json::json!({ "permissions": { "fs": "allow" } }))
            .expect("build filesystem-enabled VM config");
    harness.send_request(
        request_frame(
            3,
            session_ownership(&connection_id, &session_id),
            RequestPayload::CreateVmRequest(wire::CreateVmRequest {
                runtime: wire::GuestRuntimeKind::JavaScript,
                config: serde_json::to_string(&vm_config).expect("serialize test VM config"),
            }),
        ),
        1,
    );
    let created = harness.response().await;
    let ResponsePayload::VmCreatedResponse(created) = created.payload else {
        panic!("expected VM-created response");
    };
    (
        harness,
        engine_task,
        connection_id,
        session_id,
        created.vm_id,
    )
}

fn protocol_loop_binding_process(
    vm: &mut crate::state::VmState,
    label: &str,
    parent_pid: Option<u32>,
    event_notify: Arc<Notify>,
) -> (crate::state::ActiveProcess, crate::state::BindingExecution) {
    let kernel_handle = vm
        .kernel
        .create_virtual_process(
            crate::state::EXECUTION_DRIVER_NAME,
            crate::state::EXECUTION_DRIVER_NAME,
            crate::state::JAVASCRIPT_COMMAND,
            vec![String::from(crate::state::JAVASCRIPT_COMMAND)],
            agentos_kernel::kernel::VirtualProcessOptions {
                parent_pid,
                env: vm.guest_env.clone(),
                ..Default::default()
            },
        )
        .unwrap_or_else(|error| panic!("create {label} kernel process: {error}"));
    let execution = crate::state::BindingExecution::with_event_notify(
        event_notify,
        agentos_runtime::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS,
    );
    let producer = execution.clone();
    let process = crate::state::ActiveProcess::new(
        kernel_handle.pid(),
        kernel_handle,
        vm.runtime_context.clone(),
        vm.limits.clone(),
        agentos_runtime::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS,
        wire::GuestRuntimeKind::JavaScript,
        crate::state::ActiveExecution::Binding(execution),
    );
    (process, producer)
}

fn queue_protocol_loop_binding_event(
    execution: &crate::state::BindingExecution,
    event: crate::state::ActiveExecutionEvent,
) {
    assert!(crate::execution::send_binding_process_event(
        &execution.cancelled,
        &execution.pending_events,
        &execution.event_overflow_reason,
        &execution.pending_event_bytes,
        &execution.pending_event_count_limit,
        &execution.pending_event_bytes_limit,
        &execution.vm_pending_event_bytes_budget,
        event,
    ));
    execution.event_notify.notify_one();
}

fn response_payload(response: &ResponseFrame) -> &[u8] {
    let ResponsePayload::ExtEnvelope(envelope) = &response.payload else {
        panic!("expected extension response, got {:?}", response.payload);
    };
    &envelope.payload
}

async fn finish_cleanly(
    harness: &ProtocolLoopHarness,
    engine_task: tokio::task::JoinHandle<Result<(), Box<dyn Error>>>,
) {
    harness.shutdown("test complete");
    tokio::time::timeout(TEST_TIMEOUT, engine_task)
        .await
        .expect("protocol engine shutdown deadline")
        .expect("protocol engine task joined")
        .expect("protocol engine shut down cleanly");
    assert_eq!(harness.operations.snapshot().in_flight_requests, 0);
    assert_eq!(harness.progress_requests.snapshot().in_flight_requests, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn request_concurrency_real_loop_completes_independent_work_out_of_order() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = Arc::new(GatedExtensionState::default());
            let (harness, engine) = ProtocolLoopHarness::build(
                Arc::clone(&state),
                &[("conn", &["session-a", "session-b"])],
                4,
                4096,
            );
            let engine_task = tokio::task::spawn_local(run_protocol_engine(engine));

            harness.send_request(extension_request(10, "conn", "session-a", "block:first"), 1);
            state.gate("first").wait_started().await;
            harness.send_request(extension_request(11, "conn", "session-b", "echo:second"), 1);

            let second = harness.response().await;
            assert_eq!(second.request_id, 11);
            assert_eq!(ownership_connection_id(&second.ownership), "conn");
            assert_eq!(response_payload(&second), b"echo:second");
            state.gate("first").release();
            let first = harness.response().await;
            assert_eq!(first.request_id, 10);
            assert_eq!(response_payload(&first), b"released:first");

            finish_cleanly(&harness, engine_task).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn request_concurrency_real_loop_starts_two_blocking_sessions_together() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = Arc::new(GatedExtensionState::default());
            let (harness, engine) = ProtocolLoopHarness::build(
                Arc::clone(&state),
                &[("conn", &["session-a", "session-b"])],
                4,
                4096,
            );
            let engine_task = tokio::task::spawn_local(run_protocol_engine(engine));
            harness.send_request(extension_request(10, "conn", "session-a", "block:a"), 1);
            harness.send_request(extension_request(11, "conn", "session-b", "block:b"), 1);
            let gate_a = state.gate("a");
            let gate_b = state.gate("b");
            tokio::join!(gate_a.wait_started(), gate_b.wait_started());

            state.gate("a").release();
            state.gate("b").release();
            let responses = [harness.response().await, harness.response().await];
            assert_eq!(
                responses
                    .iter()
                    .map(|response| response.request_id)
                    .collect::<BTreeSet<_>>(),
                BTreeSet::from([10, 11]),
            );
            finish_cleanly(&harness, engine_task).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn request_concurrency_real_loop_same_vm_read_and_write_share_the_gate() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = Arc::new(GatedExtensionState::default());
            let (harness, engine_task, connection_id, session_id, vm_id) =
                start_protocol_loop_with_vm(state).await;

            harness.send_request(
                guest_filesystem_request(
                    4,
                    &connection_id,
                    &session_id,
                    &vm_id,
                    wire::GuestFilesystemOperation::WriteFile,
                    "/seed.txt",
                    Some("seed"),
                ),
                1,
            );
            assert!(matches!(
                harness.response().await.payload,
                ResponsePayload::GuestFilesystemResultResponse(_)
            ));

            // Retain one real ordinary permit while both protocol requests
            // run. If the gate serialized ordinary work, neither could finish
            // until this permit was dropped.
            let held = harness
                .ownership_coordinator
                .admit(
                    &RequestOperationMetadata::new(
                        vm_ownership(&connection_id, &session_id, &vm_id),
                        "held same-VM ordinary operation",
                        VmConcurrencyClass::SharedVm,
                    ),
                    crate::request_operations::OperationCancellation::new(),
                )
                .await
                .expect("hold one ordinary VM permit");
            harness.send_request(
                guest_filesystem_request(
                    5,
                    &connection_id,
                    &session_id,
                    &vm_id,
                    wire::GuestFilesystemOperation::ReadFile,
                    "/seed.txt",
                    None,
                ),
                1,
            );
            harness.send_request(
                guest_filesystem_request(
                    6,
                    &connection_id,
                    &session_id,
                    &vm_id,
                    wire::GuestFilesystemOperation::WriteFile,
                    "/parallel.txt",
                    Some("parallel"),
                ),
                1,
            );
            let responses = [harness.response().await, harness.response().await];
            assert_eq!(
                responses
                    .iter()
                    .map(|response| response.request_id)
                    .collect::<BTreeSet<_>>(),
                BTreeSet::from([5, 6]),
            );
            assert!(responses.iter().all(|response| matches!(
                response.payload,
                ResponsePayload::GuestFilesystemResultResponse(_)
            )));
            drop(held);
            finish_cleanly(&harness, engine_task).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn request_concurrency_real_loop_sleeping_prompt_does_not_hold_same_vm_gate() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = Arc::new(GatedExtensionState::default());
            let (harness, engine_task, connection_id, session_id, vm_id) =
                start_protocol_loop_with_vm(Arc::clone(&state)).await;
            harness.send_request(
                guest_filesystem_request(
                    4,
                    &connection_id,
                    &session_id,
                    &vm_id,
                    wire::GuestFilesystemOperation::WriteFile,
                    "/during-prompt.txt",
                    Some("readable"),
                ),
                1,
            );
            assert_eq!(harness.response().await.request_id, 4);

            harness.send_request(
                extension_request(5, &connection_id, &session_id, "block:prompt-vm"),
                1,
            );
            state.gate("prompt-vm").wait_started().await;
            harness.send_request(
                guest_filesystem_request(
                    6,
                    &connection_id,
                    &session_id,
                    &vm_id,
                    wire::GuestFilesystemOperation::ReadFile,
                    "/during-prompt.txt",
                    None,
                ),
                1,
            );
            harness.send_request(
                guest_filesystem_request(
                    7,
                    &connection_id,
                    &session_id,
                    &vm_id,
                    wire::GuestFilesystemOperation::WriteFile,
                    "/also-during-prompt.txt",
                    Some("writable"),
                ),
                1,
            );
            let filesystem = [harness.response().await, harness.response().await];
            assert_eq!(
                filesystem
                    .iter()
                    .map(|response| response.request_id)
                    .collect::<BTreeSet<_>>(),
                BTreeSet::from([6, 7]),
                "same-VM filesystem work must finish before the sleeping prompt",
            );
            state.gate("prompt-vm").release();
            assert_eq!(harness.response().await.request_id, 5);
            finish_cleanly(&harness, engine_task).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn request_concurrency_real_loop_configure_waits_and_rejects_new_same_vm_work() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = Arc::new(GatedExtensionState::default());
            let (harness, engine_task, connection_id, session_id, vm_id) =
                start_protocol_loop_with_vm(state).await;
            let held = harness
                .ownership_coordinator
                .admit(
                    &RequestOperationMetadata::new(
                        vm_ownership(&connection_id, &session_id, &vm_id),
                        "held pre-configure VM operation",
                        VmConcurrencyClass::SharedVm,
                    ),
                    crate::request_operations::OperationCancellation::new(),
                )
                .await
                .expect("hold an earlier ordinary operation");
            harness.send_request(
                request_frame(
                    4,
                    vm_ownership(&connection_id, &session_id, &vm_id),
                    RequestPayload::ConfigureVmRequest(wire::ConfigureVmRequest {
                        mounts: Vec::new(),
                        software: Vec::new(),
                        permissions: None,
                        module_access_cwd: None,
                        instructions: Vec::new(),
                        projected_modules: Vec::new(),
                        command_permissions: Default::default(),
                        loopback_exempt_ports: Vec::new(),
                        packages: Vec::new(),
                        packages_mount_at: String::new(),
                        bootstrap_commands: Vec::new(),
                        binding_shim_commands: Vec::new(),
                    }),
                ),
                1,
            );
            let vm = harness
                .ownership_coordinator
                .connection(&connection_id)
                .expect("connection coordinator")
                .session(&session_id)
                .expect("session coordinator")
                .vm(&vm_id)
                .expect("VM coordinator");
            tokio::time::timeout(TEST_TIMEOUT, async {
                while vm.snapshot().lifecycle
                    != crate::ownership_coordinator::VmLifecyclePhase::Pending
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("configure enters pending lifecycle state");

            harness.send_request(
                guest_filesystem_request(
                    5,
                    &connection_id,
                    &session_id,
                    &vm_id,
                    wire::GuestFilesystemOperation::Stat,
                    "/",
                    None,
                ),
                1,
            );
            let rejected = harness.response().await;
            assert_eq!(rejected.request_id, 5);
            let ResponsePayload::RejectedResponse(rejection) = rejected.payload else {
                panic!("same-VM work must be rejected while configure is pending");
            };
            assert_eq!(rejection.code, "ERR_AGENTOS_VM_LIFECYCLE_CONFLICT");
            assert_eq!(rejection.retryable, Some(true));

            drop(held);
            let configured = harness.response().await;
            assert_eq!(configured.request_id, 4);
            assert!(matches!(
                configured.payload,
                ResponsePayload::VmConfiguredResponse(_)
            ));
            finish_cleanly(&harness, engine_task).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn request_concurrency_real_loop_pipelined_membership_commits_before_dependent_routing() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = Arc::new(GatedExtensionState::default());
            let (harness, engine) = ProtocolLoopHarness::build(Arc::clone(&state), &[], 3, 4096);

            harness.send_request(
                request_frame(
                    20,
                    connection_ownership("client-hint"),
                    RequestPayload::AuthenticateRequest(wire::AuthenticateRequest {
                        client_name: String::from("pipelined-membership"),
                        auth_token: String::new(),
                        protocol_version: wire::PROTOCOL_VERSION,
                        bridge_version: agentos_bridge::bridge_contract().version,
                    }),
                ),
                1,
            );
            harness.send_request(
                request_frame(
                    21,
                    connection_ownership("conn-1"),
                    RequestPayload::OpenSessionRequest(wire::OpenSessionRequest {
                        placement: wire::SidecarPlacement::SidecarPlacementShared(
                            wire::SidecarPlacementShared { pool: None },
                        ),
                        metadata: std::collections::HashMap::new(),
                    }),
                ),
                1,
            );
            harness.send_request(
                request_frame(
                    22,
                    session_ownership("conn-1", "session-1"),
                    RequestPayload::CreateVmRequest(wire::CreateVmRequest {
                        runtime: wire::GuestRuntimeKind::JavaScript,
                        config:
                            serde_json::to_string(&agentos_vm_config::CreateVmConfig::default())
                                .expect("serialize default VM config"),
                    }),
                ),
                1,
            );

            // Start only after all dependent frames are queued. The biased
            // production router therefore sees each next frame before any
            // detached ready-completion task can update membership later.
            let engine_task = tokio::task::spawn_local(run_protocol_engine(engine));
            let responses = [
                harness.response().await,
                harness.response().await,
                harness.response().await,
            ];
            assert!(responses.iter().any(|response| matches!(
                &response.payload,
                ResponsePayload::AuthenticatedResponse(authenticated)
                    if response.request_id == 20 && authenticated.connection_id == "conn-1"
            )));
            assert!(responses.iter().any(|response| matches!(
                &response.payload,
                ResponsePayload::SessionOpenedResponse(opened)
                    if response.request_id == 21 && opened.session_id == "session-1"
            )));
            assert!(responses.iter().any(|response| matches!(
                &response.payload,
                ResponsePayload::VmCreatedResponse(_)
                    if response.request_id == 22
            )));
            assert!(responses.iter().all(|response| !matches!(
                &response.payload,
                ResponsePayload::RejectedResponse(_)
            )));

            finish_cleanly(&harness, engine_task).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn request_concurrency_real_loop_auth_commit_is_cleaned_if_output_fails_before_terminal() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = Arc::new(GatedExtensionState::default());
            let (harness, engine) = ProtocolLoopHarness::build(Arc::clone(&state), &[], 1, 4096);
            harness.send_request(
                request_frame(
                    23,
                    connection_ownership("client-hint"),
                    RequestPayload::AuthenticateRequest(wire::AuthenticateRequest {
                        client_name: String::from("failed-auth-output"),
                        auth_token: String::new(),
                        protocol_version: wire::PROTOCOL_VERSION,
                        bridge_version: agentos_bridge::bridge_contract().version,
                    }),
                ),
                1,
            );
            harness
                .output
                .close_with_error("deterministic auth output failure");

            let error = tokio::time::timeout(
                TEST_TIMEOUT,
                tokio::task::spawn_local(run_protocol_engine(engine)),
            )
            .await
            .expect("bounded failed-auth cleanup")
            .expect("protocol engine joined")
            .expect_err("closed output remains visible");
            assert!(error
                .to_string()
                .contains("deterministic auth output failure"));
            assert_eq!(harness.operations.snapshot().in_flight_requests, 0);
            assert!(
                harness.ownership_coordinator.connection("conn-1").is_err(),
                "membership committed before output must still be removed by fatal cleanup",
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn request_concurrency_real_loop_vm_a_disposal_does_not_delay_vm_b_query() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = Arc::new(GatedExtensionState::default());
            let (harness, engine) =
                ProtocolLoopHarness::build(Arc::clone(&state), &[], 6, 16 * 1024);

            harness.send_request(
                request_frame(
                    30,
                    connection_ownership("client-hint"),
                    RequestPayload::AuthenticateRequest(wire::AuthenticateRequest {
                        client_name: String::from("generic-request-concurrency"),
                        auth_token: String::new(),
                        protocol_version: wire::PROTOCOL_VERSION,
                        bridge_version: agentos_bridge::bridge_contract().version,
                    }),
                ),
                1,
            );
            harness.send_request(
                request_frame(
                    31,
                    connection_ownership("conn-1"),
                    RequestPayload::OpenSessionRequest(wire::OpenSessionRequest {
                        placement: wire::SidecarPlacement::SidecarPlacementShared(
                            wire::SidecarPlacementShared { pool: None },
                        ),
                        metadata: std::collections::HashMap::new(),
                    }),
                ),
                1,
            );
            let engine_task = tokio::task::spawn_local(run_protocol_engine(engine));
            let membership = [harness.response().await, harness.response().await];
            assert_eq!(
                membership
                    .iter()
                    .map(|response| response.request_id)
                    .collect::<BTreeSet<_>>(),
                BTreeSet::from([30, 31]),
            );
            assert!(membership.iter().all(|response| !matches!(
                &response.payload,
                ResponsePayload::RejectedResponse(_)
            )));

            let mut vm_ids = Vec::new();
            let vm_config: agentos_vm_config::CreateVmConfig = serde_json::from_value(
                serde_json::json!({ "permissions": { "process": "allow" } }),
            )
            .expect("build VM config with process inspection permission");
            for request_id in [32, 33] {
                harness.send_request(
                    request_frame(
                        request_id,
                        session_ownership("conn-1", "session-1"),
                        RequestPayload::CreateVmRequest(wire::CreateVmRequest {
                            runtime: wire::GuestRuntimeKind::JavaScript,
                            config: serde_json::to_string(&vm_config)
                                .expect("serialize test VM config"),
                        }),
                    ),
                    1,
                );
                let response = harness.response().await;
                assert_eq!(response.request_id, request_id);
                let ResponsePayload::VmCreatedResponse(created) = response.payload else {
                    panic!("expected VM creation response");
                };
                vm_ids.push(created.vm_id);
            }
            let vm_a = vm_ids.remove(0);
            let vm_b = vm_ids.remove(0);

            // Model an already-running VM-A operation by retaining its real
            // coordinator permit. DisposeVm must wait for that narrow owner,
            // but the central protocol loop must continue routing generic
            // non-extension work owned by VM B.
            let held_cancellation = crate::request_operations::OperationCancellation::new();
            let held_vm_a = harness
                .ownership_coordinator
                .admit(
                    &RequestOperationMetadata::new(
                        vm_ownership("conn-1", "session-1", &vm_a),
                        "held VM-A operation",
                        VmConcurrencyClass::SharedVm,
                    ),
                    held_cancellation.clone(),
                )
                .await
                .expect("admit held VM-A operation");

            harness.send_request(
                request_frame(
                    34,
                    vm_ownership("conn-1", "session-1", &vm_a),
                    RequestPayload::DisposeVmRequest(wire::DisposeVmRequest {
                        reason: wire::DisposeReason::Requested,
                    }),
                ),
                1,
            );
            let vm_a_coordinator = harness
                .ownership_coordinator
                .connection("conn-1")
                .expect("connection coordinator")
                .session("session-1")
                .expect("session coordinator")
                .vm(&vm_a)
                .expect("VM-A coordinator");
            tokio::time::timeout(TEST_TIMEOUT, async {
                while vm_a_coordinator.snapshot().phase
                    != crate::ownership_coordinator::CoordinatorPhase::Closing
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("VM-A disposal reached its deterministic drain gate");
            assert_eq!(
                held_cancellation.reason(),
                Some(OperationCancellationReason::Explicit),
                "disposal must signal the operation it is waiting to drain",
            );

            harness.send_request(
                request_frame(
                    36,
                    vm_ownership("conn-1", "session-1", &vm_a),
                    RequestPayload::GetProcessSnapshotRequest,
                ),
                1,
            );
            harness.send_request(
                request_frame(
                    35,
                    vm_ownership("conn-1", "session-1", &vm_b),
                    RequestPayload::GetProcessSnapshotRequest,
                ),
                1,
            );
            let pending_responses = [harness.response().await, harness.response().await];
            let independent = pending_responses
                .iter()
                .find(|response| response.request_id == 35)
                .expect("VM-B response completes during VM-A drain");
            assert!(
                matches!(
                    &independent.payload,
                    ResponsePayload::ProcessSnapshotResponse(_)
                ),
                "unexpected VM-B response: {:?}",
                independent.payload
            );
            let same_vm = pending_responses
                .iter()
                .find(|response| response.request_id == 36)
                .expect("same-VM request receives a typed conflict");
            let ResponsePayload::RejectedResponse(rejection) = &same_vm.payload else {
                panic!("same-VM request must be rejected while disposal is pending");
            };
            assert!(
                matches!(
                    rejection.code.as_str(),
                    "ERR_AGENTOS_REQUEST_ADMISSION_CLOSED"
                        | "ERR_AGENTOS_COORDINATOR_CLOSING"
                        | "ERR_AGENTOS_VM_LIFECYCLE_CONFLICT"
                ),
                "unexpected same-VM lifecycle conflict: {}",
                rejection.code,
            );
            drop(held_vm_a);
            let disposed = harness.response().await;
            assert_eq!(disposed.request_id, 34);
            assert!(matches!(
                disposed.payload,
                ResponsePayload::VmDisposedResponse(_)
            ));

            finish_cleanly(&harness, engine_task).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn request_concurrency_real_loop_attached_and_detached_children_advance_while_ordinary_is_gated(
) {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = Arc::new(GatedExtensionState::default());
            let sessions: &[&str] = &["session-child-progress"];
            let (harness, mut engine) = ProtocolLoopHarness::build(
                Arc::clone(&state),
                &[("conn-child-progress", sessions)],
                4,
                16 * 1024,
            );
            let connection_id = "conn-child-progress";
            let session_id = "session-child-progress";
            engine.sidecar.connections.insert(
                connection_id.to_owned(),
                crate::state::ConnectionState {
                    auth_token: String::new(),
                    sessions: BTreeSet::from([session_id.to_owned()]),
                },
            );
            engine.sidecar.sessions.insert(
                session_id.to_owned(),
                crate::state::SessionState {
                    connection_id: connection_id.to_owned(),
                    placement: wire::SidecarPlacement::SidecarPlacementShared(
                        wire::SidecarPlacementShared { pool: None },
                    ),
                    metadata: BTreeMap::new(),
                    vm_ids: BTreeSet::new(),
                },
            );
            let create = crate::protocol::CreateVmRequest::legacy_test_config(
                crate::protocol::GuestRuntimeKind::JavaScript,
                Default::default(),
                Default::default(),
                Some(crate::protocol::PermissionsPolicy::allow_all()),
            );
            let create_request = crate::protocol::RequestFrame::new(
                700,
                crate::protocol::OwnershipScope::session(connection_id, session_id),
                crate::protocol::RequestPayload::CreateVm(create.clone()),
            );
            let dispatch = engine
                .sidecar
                .create_vm(&create_request, create)
                .await
                .expect("create child-progress VM");
            let crate::protocol::ResponsePayload::VmCreated(created) = dispatch.response.payload
            else {
                panic!("expected child-progress VM creation response");
            };
            let vm_id = created.vm_id;
            harness
                .ownership_coordinator
                .connection(connection_id)
                .and_then(|connection| connection.session(session_id))
                .and_then(|session| session.open_vm(vm_id.clone()))
                .expect("register child-progress VM ownership");

            let process_event_notify = Arc::clone(&engine.sidecar.process_event_notify);
            let (attached_completion_tx, attached_completion_rx) = tokio::sync::oneshot::channel();
            let (attached_producer, detached_producer) = {
                let mut vm = engine
                    .sidecar
                    .vms
                    .get_mut(&vm_id)
                    .expect("child-progress VM");
                let (mut root, _) = protocol_loop_binding_process(
                    &mut vm,
                    "child-progress root",
                    None,
                    Arc::clone(&process_event_notify),
                );
                let root_pid = root.kernel_pid;
                let (attached, attached_producer) = protocol_loop_binding_process(
                    &mut vm,
                    "attached child",
                    Some(root_pid),
                    Arc::clone(&process_event_notify),
                );
                let attached_id = String::from("attached-child");
                root.pending_child_process_sync.insert(
                    attached_id.clone(),
                    crate::state::PendingChildProcessSync {
                        pid: attached.kernel_pid,
                        stdout: Vec::new(),
                        stderr: Vec::new(),
                        max_buffer: 1024,
                        deadline: None,
                        timeout_signal: String::from("SIGTERM"),
                        kill_sent: false,
                        timed_out: false,
                        max_buffer_exceeded: false,
                        completion: crate::state::PendingChildProcessSyncCompletion::Javascript(
                            attached_completion_tx,
                        ),
                    },
                );
                root.child_processes.insert(attached_id, attached);
                vm.active_processes
                    .insert(String::from("child-progress-root"), root);

                let (detached, detached_producer) = protocol_loop_binding_process(
                    &mut vm,
                    "detached child",
                    None,
                    Arc::clone(&process_event_notify),
                );
                let detached_id = String::from("detached-child");
                vm.detached_child_processes.insert(detached_id.clone());
                vm.active_processes.insert(detached_id, detached);
                (attached_producer, detached_producer)
            };

            let engine_task = tokio::task::spawn_local(run_protocol_engine(engine));
            harness.send_request(
                extension_request(701, connection_id, session_id, "block:child-progress-gate"),
                1,
            );
            state.gate("child-progress-gate").wait_started().await;

            queue_protocol_loop_binding_event(
                &attached_producer,
                crate::state::ActiveExecutionEvent::Stdout(b"attached-progress".to_vec()),
            );
            queue_protocol_loop_binding_event(
                &attached_producer,
                crate::state::ActiveExecutionEvent::Exited(0),
            );
            queue_protocol_loop_binding_event(
                &detached_producer,
                crate::state::ActiveExecutionEvent::Stdout(b"detached-progress".to_vec()),
            );
            process_event_notify.notify_one();

            let attached = tokio::time::timeout(TEST_TIMEOUT, attached_completion_rx)
                .await
                .expect("attached child completes while ordinary request is gated")
                .expect("attached completion sender remains live")
                .expect("attached child completion succeeds");
            assert_eq!(
                attached.get("stdout").and_then(serde_json::Value::as_str),
                Some("attached-progress")
            );
            assert_eq!(
                attached.get("code").and_then(serde_json::Value::as_i64),
                Some(0)
            );

            let detached = tokio::time::timeout(TEST_TIMEOUT, async {
                loop {
                    let frame = harness.ordinary_frame();
                    if let ProtocolFrame::EventFrame(event) = frame {
                        if format!("{event:?}").contains("detached-child") {
                            break event;
                        }
                    }
                }
            })
            .await
            .expect("detached child emits while ordinary request is gated");
            assert!(format!("{detached:?}").contains("detached-child"));
            assert_eq!(harness.operations.snapshot().in_flight_requests, 1);

            state.gate("child-progress-gate").release();
            assert_eq!(harness.response().await.request_id, 701);
            finish_cleanly(&harness, engine_task).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn request_concurrency_real_loop_child_service_saturation_rearms_exactly_once() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = Arc::new(GatedExtensionState::default());
            let connection_id = "conn-child-saturation";
            let session_id = "session-child-saturation";
            let sessions: &[&str] = &[session_id];
            let (harness, mut engine) =
                ProtocolLoopHarness::build(state, &[(connection_id, sessions)], 1, 16 * 1024);
            // The production loop derives owned-service task and claim capacity
            // from these two limits. A capacity of one makes the second event
            // depend on the pump's coalesced continuation wake.
            engine.protocol.max_in_flight_requests = 1;
            engine.protocol.max_control_frames = 0;
            engine.sidecar.connections.insert(
                connection_id.to_owned(),
                crate::state::ConnectionState {
                    auth_token: String::new(),
                    sessions: BTreeSet::from([session_id.to_owned()]),
                },
            );
            engine.sidecar.sessions.insert(
                session_id.to_owned(),
                crate::state::SessionState {
                    connection_id: connection_id.to_owned(),
                    placement: wire::SidecarPlacement::SidecarPlacementShared(
                        wire::SidecarPlacementShared { pool: None },
                    ),
                    metadata: BTreeMap::new(),
                    vm_ids: BTreeSet::new(),
                },
            );
            let create = crate::protocol::CreateVmRequest::legacy_test_config(
                crate::protocol::GuestRuntimeKind::JavaScript,
                Default::default(),
                Default::default(),
                Some(crate::protocol::PermissionsPolicy::allow_all()),
            );
            let create_request = crate::protocol::RequestFrame::new(
                710,
                crate::protocol::OwnershipScope::session(connection_id, session_id),
                crate::protocol::RequestPayload::CreateVm(create.clone()),
            );
            let dispatch = engine
                .sidecar
                .create_vm(&create_request, create)
                .await
                .expect("create service-saturation VM");
            let crate::protocol::ResponsePayload::VmCreated(created) = dispatch.response.payload
            else {
                panic!("expected service-saturation VM creation response");
            };
            let vm_id = created.vm_id;
            harness
                .ownership_coordinator
                .connection(connection_id)
                .and_then(|connection| connection.session(session_id))
                .and_then(|session| session.open_vm(vm_id.clone()))
                .expect("register service-saturation VM ownership");

            let process_event_notify = Arc::clone(&engine.sidecar.process_event_notify);
            let producer = {
                let mut vm = engine
                    .sidecar
                    .vms
                    .get_mut(&vm_id)
                    .expect("service-saturation VM");
                let (process, producer) = protocol_loop_binding_process(
                    &mut vm,
                    "service-saturation root",
                    None,
                    process_event_notify,
                );
                vm.active_processes
                    .insert(String::from("service-saturation-root"), process);
                producer
            };
            for request_id in [711, 712] {
                queue_protocol_loop_binding_event(
                    &producer,
                    crate::state::ActiveExecutionEvent::JavascriptSyncRpcRequest(
                        agentos_execution::JavascriptSyncRpcRequest {
                            id: request_id,
                            method: String::from("fs.readFile"),
                            args: vec![serde_json::Value::String(String::from("/missing"))],
                            raw_bytes_args: Default::default(),
                        },
                    ),
                );
            }

            let engine_task = tokio::task::spawn_local(run_protocol_engine(engine));
            harness.send_request(
                extension_request(713, connection_id, session_id, "after-service-failure"),
                1,
            );
            assert_eq!(harness.response().await.request_id, 713);

            tokio::time::timeout(TEST_TIMEOUT, async {
                loop {
                    if producer
                        .pending_events
                        .lock()
                        .expect("binding event queue")
                        .is_empty()
                    {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("both coalesced child services are claimed");

            // A second ordinary request proves that failure released the sole
            // service slot and that the empty continuation turn did not spin
            // or starve protocol ingress.
            harness.send_request(
                extension_request(714, connection_id, session_id, "after-empty-rearm"),
                1,
            );
            assert_eq!(harness.response().await.request_id, 714);
            assert!(producer
                .pending_events
                .lock()
                .expect("binding event queue")
                .is_empty());

            finish_cleanly(&harness, engine_task).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn targeted_public_waiter_cannot_consume_internal_process_pump_wake() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = Arc::new(GatedExtensionState::default());
            let connection_id = "conn-process-wake-isolation";
            let session_id = "session-process-wake-isolation";
            let sessions: &[&str] = &[session_id];
            let (harness, mut engine) =
                ProtocolLoopHarness::build(state, &[(connection_id, sessions)], 4, 16 * 1024);
            engine.sidecar.connections.insert(
                connection_id.to_owned(),
                crate::state::ConnectionState {
                    auth_token: String::new(),
                    sessions: BTreeSet::from([session_id.to_owned()]),
                },
            );
            engine.sidecar.sessions.insert(
                session_id.to_owned(),
                crate::state::SessionState {
                    connection_id: connection_id.to_owned(),
                    placement: wire::SidecarPlacement::SidecarPlacementShared(
                        wire::SidecarPlacementShared { pool: None },
                    ),
                    metadata: BTreeMap::new(),
                    vm_ids: BTreeSet::new(),
                },
            );
            let create = crate::protocol::CreateVmRequest::legacy_test_config(
                crate::protocol::GuestRuntimeKind::JavaScript,
                Default::default(),
                Default::default(),
                Some(crate::protocol::PermissionsPolicy::allow_all()),
            );
            let create_request = crate::protocol::RequestFrame::new(
                720,
                crate::protocol::OwnershipScope::session(connection_id, session_id),
                crate::protocol::RequestPayload::CreateVm(create.clone()),
            );
            let dispatch = engine
                .sidecar
                .create_vm(&create_request, create)
                .await
                .expect("create process-wake-isolation VM");
            let crate::protocol::ResponsePayload::VmCreated(created) = dispatch.response.payload
            else {
                panic!("expected process-wake-isolation VM creation response");
            };
            let vm_id = created.vm_id;
            harness
                .ownership_coordinator
                .connection(connection_id)
                .and_then(|connection| connection.session(session_id))
                .and_then(|session| session.open_vm(vm_id.clone()))
                .expect("register process-wake-isolation VM ownership");

            let process_event_notify = Arc::clone(&engine.sidecar.process_event_notify);
            let process_id = String::from("process-wake-isolation-root");
            let producer = {
                let mut vm = engine
                    .sidecar
                    .vms
                    .get_mut(&vm_id)
                    .expect("process-wake-isolation VM");
                let (process, producer) =
                    protocol_loop_binding_process(&mut vm, &process_id, None, process_event_notify);
                vm.active_processes.insert(process_id.clone(), process);
                producer
            };

            // Start an ACP-style targeted public-event waiter while the
            // protocol engine is not running, then manually complete its first
            // durable probe. In the old topology this waiter next registered
            // on the runtime producer Notify and was guaranteed to steal the
            // only wake queued below.
            let services = Arc::clone(&engine.extension_services);
            let ownership =
                crate::protocol::OwnershipScope::vm(connection_id, session_id, vm_id.clone());
            let waiter_task = tokio::task::spawn_local(async move {
                services
                    .poll_process_event(ownership, process_id, Duration::from_secs(60))
                    .await
            });
            let command = tokio::time::timeout(TEST_TIMEOUT, engine.extension_service_rx.recv())
                .await
                .expect("targeted waiter submitted its initial durable probe")
                .expect("extension service command channel remains open");
            let prepared = prepare_extension_service_command(
                &mut engine.sidecar,
                &engine.ownership_coordinator,
                command,
            );
            let completion = prepared
                .execute_supervised()
                .await
                .expect("initial targeted probe completes");
            assert!(completion.complete(&mut engine.sidecar).is_none());
            for _ in 0..3 {
                tokio::task::yield_now().await;
            }

            queue_protocol_loop_binding_event(
                &producer,
                crate::state::ActiveExecutionEvent::JavascriptSyncRpcRequest(
                    agentos_execution::JavascriptSyncRpcRequest {
                        id: 721,
                        method: String::from("fs.readFile"),
                        args: vec![serde_json::Value::String(String::from("/missing"))],
                        raw_bytes_args: Default::default(),
                    },
                ),
            );
            // Give the already-registered public waiter every opportunity to
            // consume the producer edge before the central pump is started.
            for _ in 0..3 {
                tokio::task::yield_now().await;
            }

            let engine_task = tokio::task::spawn_local(run_protocol_engine(engine));
            tokio::time::timeout(TEST_TIMEOUT, async {
                loop {
                    if producer
                        .pending_events
                        .lock()
                        .expect("binding event queue")
                        .is_empty()
                    {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("central pump retains its wake and claims the internal RPC");

            waiter_task.abort();
            let _ = waiter_task.await;
            finish_cleanly(&harness, engine_task).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn request_concurrency_real_loop_leaves_opaque_route_exclusion_to_extension() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = Arc::new(GatedExtensionState::default());
            let (harness, engine) = ProtocolLoopHarness::build(
                Arc::clone(&state),
                &[("conn", &["session-a", "session-b"])],
                4,
                4096,
            );
            let engine_task = tokio::task::spawn_local(run_protocol_engine(engine));

            harness.send_request(
                extension_request(15, "conn", "session-a", "key:same:block:first"),
                1,
            );
            state.gate("first").wait_started().await;
            harness.send_request(
                extension_request(16, "conn", "session-b", "key:same:conflict"),
                1,
            );
            harness.send_request(
                extension_request(17, "conn", "session-b", "key:different:independent"),
                1,
            );

            let responses = [harness.response().await, harness.response().await];
            let same_key = responses
                .iter()
                .find(|response| response.request_id == 16)
                .expect("same opaque key progresses independently");
            assert_eq!(response_payload(same_key), b"conflict");
            let independent = responses
                .iter()
                .find(|response| response.request_id == 17)
                .expect("different key progresses before gate release");
            assert_eq!(response_payload(independent), b"independent");

            state.gate("first").release();
            assert_eq!(harness.response().await.request_id, 15);
            harness.send_request(
                extension_request(18, "conn", "session-b", "key:same:after-release"),
                1,
            );
            assert_eq!(
                response_payload(&harness.response().await),
                b"after-release"
            );

            finish_cleanly(&harness, engine_task).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn request_concurrency_real_loop_rejects_duplicate_but_scopes_ids_to_connection() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = Arc::new(GatedExtensionState::default());
            let (harness, engine) = ProtocolLoopHarness::build(
                Arc::clone(&state),
                &[("conn-a", &["session-a"]), ("conn-b", &["session-b"])],
                4,
                4096,
            );
            let engine_task = tokio::task::spawn_local(run_protocol_engine(engine));
            harness.send_request(extension_request(20, "conn-a", "session-a", "block:a"), 1);
            state.gate("a").wait_started().await;
            harness.send_request(extension_request(20, "conn-a", "session-a", "echo:dupe"), 1);
            harness.send_request(extension_request(20, "conn-b", "session-b", "block:b"), 1);

            let duplicate = harness.response().await;
            assert_eq!(duplicate.request_id, 20);
            assert_eq!(ownership_connection_id(&duplicate.ownership), "conn-a");
            let ResponsePayload::RejectedResponse(rejection) = duplicate.payload else {
                panic!("duplicate must receive a typed rejection");
            };
            assert_eq!(rejection.code, "ERR_AGENTOS_DUPLICATE_REQUEST_ID");
            state.gate("b").wait_started().await;
            assert_eq!(harness.operations.snapshot().in_flight_requests, 2);

            state.gate("a").release();
            state.gate("b").release();
            let responses = [harness.response().await, harness.response().await];
            let identities = responses
                .iter()
                .map(|response| {
                    (
                        ownership_connection_id(&response.ownership).to_owned(),
                        response.request_id,
                    )
                })
                .collect::<BTreeSet<_>>();
            assert_eq!(
                identities,
                BTreeSet::from([(String::from("conn-a"), 20), (String::from("conn-b"), 20),]),
            );
            finish_cleanly(&harness, engine_task).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn request_concurrency_real_loop_panic_has_one_terminal_and_frees_admission() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = Arc::new(GatedExtensionState::default());
            let (harness, engine) =
                ProtocolLoopHarness::build(state, &[("conn", &["session"])], 1, 4096);
            let engine_task = tokio::task::spawn_local(run_protocol_engine(engine));
            harness.send_request(extension_request(30, "conn", "session", "panic"), 1);
            let panic_response = harness.response().await;
            assert_eq!(panic_response.request_id, 30);
            let ResponsePayload::RejectedResponse(rejection) = panic_response.payload else {
                panic!("panicking operation must receive a typed terminal rejection");
            };
            assert!(rejection.message.contains("ERR_AGENTOS_REQUEST_TASK_PANIC"));
            assert_eq!(harness.operations.snapshot().in_flight_requests, 0);

            harness.send_request(extension_request(31, "conn", "session", "after-panic"), 1);
            let after = harness.response().await;
            assert_eq!(after.request_id, 31);
            assert_eq!(response_payload(&after), b"after-panic");
            assert_eq!(
                harness
                    .output
                    .state
                    .lock()
                    .expect("protocol output state")
                    .control_len(),
                0,
                "panic produced exactly one terminal response",
            );
            finish_cleanly(&harness, engine_task).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn request_concurrency_real_loop_reports_count_saturation_path() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = Arc::new(GatedExtensionState::default());
            let (harness, engine) =
                ProtocolLoopHarness::build(Arc::clone(&state), &[("conn", &["session"])], 1, 4096);
            let engine_task = tokio::task::spawn_local(run_protocol_engine(engine));
            harness.send_request(extension_request(40, "conn", "session", "block:count"), 1);
            state.gate("count").wait_started().await;
            harness.send_request(extension_request(41, "conn", "session", "overflow"), 1);
            let rejected = harness.response().await;
            let ResponsePayload::RejectedResponse(rejection) = rejected.payload else {
                panic!("count saturation must return typed rejection");
            };
            assert_eq!(rejection.code, "ERR_AGENTOS_IN_FLIGHT_REQUEST_LIMIT");
            assert_eq!(
                rejection.configuration_path.as_deref(),
                Some("runtime.protocol.maxInFlightRequests"),
            );
            state.gate("count").release();
            assert_eq!(harness.response().await.request_id, 40);
            finish_cleanly(&harness, engine_task).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn request_concurrency_real_loop_reports_byte_saturation_path() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = Arc::new(GatedExtensionState::default());
            let (harness, engine) =
                ProtocolLoopHarness::build(Arc::clone(&state), &[("conn", &["session"])], 2, 1);
            let engine_task = tokio::task::spawn_local(run_protocol_engine(engine));
            harness.send_request(extension_request(50, "conn", "session", "block:bytes"), 1);
            state.gate("bytes").wait_started().await;
            harness.send_request(extension_request(51, "conn", "session", "overflow"), 1);
            let rejected = harness.response().await;
            let ResponsePayload::RejectedResponse(rejection) = rejected.payload else {
                panic!("byte saturation must return typed rejection");
            };
            assert_eq!(rejection.code, "ERR_AGENTOS_IN_FLIGHT_REQUEST_BYTE_LIMIT");
            assert_eq!(
                rejection.configuration_path.as_deref(),
                Some("runtime.protocol.maxInFlightRequestBytes"),
            );
            state.gate("bytes").release();
            assert_eq!(harness.response().await.request_id, 50);
            finish_cleanly(&harness, engine_task).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn request_concurrency_real_loop_routes_unrelated_work_and_cancel_during_prompt() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = Arc::new(GatedExtensionState::default());
            let (harness, engine) = ProtocolLoopHarness::build(
                Arc::clone(&state),
                &[("conn", &["prompt", "other"])],
                2,
                4096,
            );
            let engine_task = tokio::task::spawn_local(run_protocol_engine(engine));
            harness.send_request(extension_request(60, "conn", "prompt", "block:prompt"), 1);
            state.gate("prompt").wait_started().await;
            harness.send_request(extension_request(61, "conn", "other", "unrelated"), 1);
            harness.send_request(extension_request(62, "conn", "prompt", "cancel:prompt"), 1);

            let responses = [
                harness.response().await,
                harness.response().await,
                harness.response().await,
            ];
            let by_id = responses
                .iter()
                .map(|response| (response.request_id, response_payload(response).to_vec()))
                .collect::<BTreeMap<_, _>>();
            assert_eq!(
                by_id.get(&60).map(Vec::as_slice),
                Some(&b"cancelled:prompt"[..])
            );
            assert_eq!(by_id.get(&61).map(Vec::as_slice), Some(&b"unrelated"[..]));
            assert_eq!(
                by_id.get(&62).map(Vec::as_slice),
                Some(&b"cancel-ack:prompt"[..])
            );
            finish_cleanly(&harness, engine_task).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn request_concurrency_real_loop_cancel_bypasses_saturated_ordinary_admission() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = Arc::new(GatedExtensionState::default());
            let (harness, engine) =
                ProtocolLoopHarness::build(Arc::clone(&state), &[("conn", &["prompt"])], 1, 4096);
            let engine_task = tokio::task::spawn_local(run_protocol_engine(engine));
            harness.send_request(extension_request(70, "conn", "prompt", "block:prompt"), 1);
            state.gate("prompt").wait_started().await;
            assert_eq!(harness.operations.snapshot().in_flight_requests, 1);
            harness.send_request(extension_request(71, "conn", "prompt", "cancel:prompt"), 1);

            let responses = [harness.response().await, harness.response().await];
            assert_eq!(
                responses
                    .iter()
                    .map(|response| response.request_id)
                    .collect::<BTreeSet<_>>(),
                BTreeSet::from([70, 71]),
            );
            finish_cleanly(&harness, engine_task).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn request_concurrency_real_loop_progress_service_bypasses_full_ordinary_service_queue() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = Arc::new(GatedExtensionState::default());
            let (harness, engine) = ProtocolLoopHarness::build(state, &[], 1, 4096);
            let mut ordinary = Vec::with_capacity(harness.ordinary_service_capacity);
            let waker = std::task::Waker::noop();
            let mut context = std::task::Context::from_waker(waker);
            for _ in 0..harness.ordinary_service_capacity {
                let mut request = harness.extension_services.acp_termination_grace();
                assert!(matches!(
                    request.as_mut().poll(&mut context),
                    std::task::Poll::Pending
                ));
                ordinary.push(request);
            }

            // The ordinary service receiver has not started and its physical
            // queue is full. WriteStdin is the adapter-cancellation path; it
            // must still acquire the independently bounded progress lane.
            let mut progress = harness.extension_services.write_stdin(
                vm_ownership("missing", "missing", "missing"),
                wire::WriteStdinRequest {
                    process_id: String::from("missing"),
                    chunk: b"cancel\n".to_vec(),
                },
            );
            assert!(matches!(
                progress.as_mut().poll(&mut context),
                std::task::Poll::Pending
            ));

            let engine_task = tokio::task::spawn_local(run_protocol_engine(engine));
            let progress_error = tokio::time::timeout(TEST_TIMEOUT, progress)
                .await
                .expect("progress service reaches the running protocol loop")
                .expect_err("missing VM returns a normal routed service error");
            assert!(
                !progress_error
                    .to_string()
                    .contains("ERR_AGENTOS_PROGRESS_SERVICE_LIMIT"),
                "progress was admitted through its reserved service lane: {progress_error}",
            );
            for request in ordinary {
                tokio::time::timeout(TEST_TIMEOUT, request)
                    .await
                    .expect("ordinary service request drains")
                    .expect("termination-grace service succeeds");
            }
            finish_cleanly(&harness, engine_task).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn request_concurrency_bounded_multi_vm_lifecycle_progress_load_finishes_exactly_once() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = Arc::new(GatedExtensionState::default());
            let sessions: &[&str] = &["route-a", "route-b", "route-c", "route-d"];
            let (harness, engine) = ProtocolLoopHarness::build(
                Arc::clone(&state),
                &[("conn", sessions)],
                64,
                64 * 1024,
            );
            let connection = harness
                .ownership_coordinator
                .connection("conn")
                .expect("load-test connection coordinator");
            let vm_a = connection
                .session("route-a")
                .expect("load-test route A")
                .open_vm("vm-a")
                .expect("open load-test VM A");
            let vm_b = connection
                .session("route-b")
                .expect("load-test route B")
                .open_vm("vm-b")
                .expect("open load-test VM B");
            let held_a = harness
                .ownership_coordinator
                .admit(
                    &RequestOperationMetadata::new(
                        vm_ownership("conn", "route-a", "vm-a"),
                        "delayed load operation A",
                        VmConcurrencyClass::SharedVm,
                    ),
                    crate::request_operations::OperationCancellation::new(),
                )
                .await
                .expect("delay VM A ordinary completion");
            let held_b = harness
                .ownership_coordinator
                .admit(
                    &RequestOperationMetadata::new(
                        vm_ownership("conn", "route-b", "vm-b"),
                        "delayed load operation B",
                        VmConcurrencyClass::SharedVm,
                    ),
                    crate::request_operations::OperationCancellation::new(),
                )
                .await
                .expect("delay VM B ordinary completion");
            let lifecycle_a_coordinator = harness.ownership_coordinator.clone();
            let lifecycle_a = tokio::task::spawn_local(async move {
                lifecycle_a_coordinator
                    .admit(
                        &RequestOperationMetadata::new(
                            vm_ownership("conn", "route-a", "vm-a"),
                            "periodic lifecycle A",
                            VmConcurrencyClass::ExclusiveVmLifecycle,
                        ),
                        crate::request_operations::OperationCancellation::new(),
                    )
                    .await
            });
            let lifecycle_b_coordinator = harness.ownership_coordinator.clone();
            let lifecycle_b = tokio::task::spawn_local(async move {
                lifecycle_b_coordinator
                    .admit(
                        &RequestOperationMetadata::new(
                            vm_ownership("conn", "route-b", "vm-b"),
                            "periodic lifecycle B",
                            VmConcurrencyClass::ExclusiveVmLifecycle,
                        ),
                        crate::request_operations::OperationCancellation::new(),
                    )
                    .await
            });
            tokio::time::timeout(TEST_TIMEOUT, async {
                while vm_a.snapshot().lifecycle
                    != crate::ownership_coordinator::VmLifecyclePhase::Pending
                    || vm_b.snapshot().lifecycle
                        != crate::ownership_coordinator::VmLifecyclePhase::Pending
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("both independent VM lifecycle requests become pending");
            let engine_task = tokio::task::spawn_local(run_protocol_engine(engine));

            for index in 0..8_i64 {
                let session = sessions[index as usize % sessions.len()];
                harness.send_request(
                    extension_request(
                        1_000 + index,
                        "conn",
                        session,
                        &format!("block:load-{index}"),
                    ),
                    1,
                );
            }
            for index in 0..8_i64 {
                state.gate(&format!("load-{index}")).wait_started().await;
            }
            // Deterministic frame-fuzz case: the first ordinary request is
            // still live when a progress-class frame reuses its connection ID.
            // The shared table rejects the duplicate without delivering the
            // cancel payload to the original operation.
            harness.send_request(
                extension_request(1_000, "conn", "route-a", "cancel:load-0"),
                1,
            );
            let duplicate = harness.response().await;
            assert_eq!(duplicate.request_id, 1_000);
            let ResponsePayload::RejectedResponse(duplicate) = duplicate.payload else {
                panic!("cross-class duplicate frame must be rejected");
            };
            assert_eq!(duplicate.code, "ERR_AGENTOS_DUPLICATE_PROGRESS_REQUEST_ID");
            for index in 0..16_i64 {
                let session = sessions[index as usize % sessions.len()];
                harness.send_request(
                    extension_request(1_100 + index, "conn", session, &format!("echo-{index}")),
                    1,
                );
            }
            for index in 0..8_i64 {
                let session = sessions[index as usize % sessions.len()];
                harness.send_request(
                    extension_request(
                        1_200 + index,
                        "conn",
                        session,
                        &format!("cancel:load-{index}"),
                    ),
                    1,
                );
            }

            drop(held_a);
            let lifecycle_a = tokio::time::timeout(TEST_TIMEOUT, lifecycle_a)
                .await
                .expect("VM A lifecycle completion deadline")
                .expect("VM A lifecycle task joined")
                .expect("VM A lifecycle activates independently");
            assert_eq!(
                vm_b.snapshot().lifecycle,
                crate::ownership_coordinator::VmLifecyclePhase::Pending,
                "VM A lifecycle completion must not alter VM B",
            );
            drop(lifecycle_a);
            drop(held_b);
            let lifecycle_b = tokio::time::timeout(TEST_TIMEOUT, lifecycle_b)
                .await
                .expect("VM B lifecycle completion deadline")
                .expect("VM B lifecycle task joined")
                .expect("VM B lifecycle activates independently");
            drop(lifecycle_b);

            let responses = tokio::time::timeout(TEST_TIMEOUT, async {
                let mut ids = BTreeSet::new();
                for _ in 0..32 {
                    let response = harness.response().await;
                    assert!(
                        ids.insert(response.request_id),
                        "duplicate terminal response"
                    );
                }
                ids
            })
            .await
            .expect("bounded mixed ordinary/progress load completes");
            let expected = (1_000..1_008)
                .chain(1_100..1_116)
                .chain(1_200..1_208)
                .collect::<BTreeSet<_>>();
            assert_eq!(responses, expected);
            assert_eq!(harness.operations.snapshot().in_flight_requests, 0);
            assert_eq!(harness.operations.snapshot().in_flight_request_bytes, 0);
            assert_eq!(harness.progress_requests.snapshot().in_flight_requests, 0);
            assert_eq!(
                harness.progress_requests.snapshot().in_flight_request_bytes,
                0
            );
            assert_eq!(
                vm_a.snapshot().lifecycle,
                crate::ownership_coordinator::VmLifecyclePhase::Idle,
            );
            assert_eq!(
                vm_b.snapshot().lifecycle,
                crate::ownership_coordinator::VmLifecyclePhase::Idle,
            );
            finish_cleanly(&harness, engine_task).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn request_concurrency_real_loop_retains_admission_through_backpressured_event_batches() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = Arc::new(GatedExtensionState::default());
            let (harness, engine) =
                ProtocolLoopHarness::build(Arc::clone(&state), &[("conn", &["session"])], 2, 4096);
            let filler_count = harness.fill_ordinary_output();
            assert!(filler_count > 0, "test must saturate ordinary output");
            let engine_task = tokio::task::spawn_local(run_protocol_engine(engine));

            harness.send_request(extension_request(72, "conn", "session", "events:a"), 1);
            harness.send_request(extension_request(73, "conn", "session", "events:b"), 1);

            let first = harness.response().await;
            let second = harness.response().await;
            assert_eq!(
                BTreeSet::from([first.request_id, second.request_id]),
                BTreeSet::from([72, 73]),
            );
            assert_eq!(
                harness.operations.snapshot().in_flight_requests,
                2,
                "terminal publication alone must not release event-batch admission",
            );

            harness.send_request(extension_request(74, "conn", "session", "echo"), 1);
            let rejection = harness.response().await;
            assert_eq!(rejection.request_id, 74);
            let ResponsePayload::RejectedResponse(rejection) = rejection.payload else {
                panic!("third request must receive bounded admission rejection");
            };
            assert_eq!(rejection.code, "ERR_AGENTOS_IN_FLIGHT_REQUEST_LIMIT");
            assert_eq!(
                rejection.configuration_path.as_deref(),
                Some("runtime.protocol.maxInFlightRequests"),
            );

            let mut delivered = BTreeSet::new();
            for _ in 0..filler_count.saturating_add(6) {
                tokio::task::yield_now().await;
                let ProtocolFrame::EventFrame(frame) = harness.ordinary_frame() else {
                    panic!("ordinary lane must contain only events");
                };
                if let wire::EventPayload::StructuredEvent(event) = frame.payload {
                    if event.name == "request-batch" {
                        delivered.insert((
                            event.detail.get("batch").cloned().expect("batch detail"),
                            event.detail.get("index").cloned().expect("index detail"),
                        ));
                    }
                }
            }
            assert_eq!(
                delivered,
                BTreeSet::from([
                    (String::from("a"), String::from("0")),
                    (String::from("a"), String::from("1")),
                    (String::from("a"), String::from("2")),
                    (String::from("b"), String::from("0")),
                    (String::from("b"), String::from("1")),
                    (String::from("b"), String::from("2")),
                ]),
                "each finite batch event must drain exactly once",
            );
            tokio::time::timeout(TEST_TIMEOUT, async {
                while harness.operations.snapshot().in_flight_requests != 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("batch publishers release admission after their final event");

            harness.send_request(extension_request(75, "conn", "session", "reused"), 1);
            assert_eq!(harness.response().await.request_id, 75);
            finish_cleanly(&harness, engine_task).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn request_concurrency_real_loop_shutdown_does_not_duplicate_claimed_batch_terminals() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = Arc::new(GatedExtensionState::default());
            let (harness, engine) =
                ProtocolLoopHarness::build(Arc::clone(&state), &[("conn", &["session"])], 1, 4096);
            assert!(harness.fill_ordinary_output() > 0);
            let engine_task = tokio::task::spawn_local(run_protocol_engine(engine));

            harness.send_request(
                extension_request(76, "conn", "session", "events:ordinary"),
                1,
            );
            harness.send_request(
                extension_request(77, "conn", "session", "progress-events:progress"),
                1,
            );
            let responses = [harness.response().await, harness.response().await];
            assert_eq!(
                responses
                    .iter()
                    .map(|response| response.request_id)
                    .collect::<BTreeSet<_>>(),
                BTreeSet::from([76, 77]),
            );
            assert_eq!(harness.operations.snapshot().in_flight_requests, 1);
            assert_eq!(harness.progress_requests.snapshot().in_flight_requests, 1);

            harness.shutdown("claimed terminals await saturated batch output");
            tokio::time::timeout(TEST_TIMEOUT, engine_task)
                .await
                .expect("bounded shutdown")
                .expect("protocol engine joined")
                .expect("explicit shutdown completes cleanly");

            assert_eq!(harness.operations.snapshot().in_flight_requests, 0);
            assert_eq!(harness.progress_requests.snapshot().in_flight_requests, 0);
            assert!(
                harness.output.recv_control().await.is_none(),
                "shutdown must not synthesize a second terminal or progress acknowledgement",
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn request_concurrency_real_loop_shutdown_forces_terminal_and_releases_everything() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = Arc::new(GatedExtensionState::default());
            let (harness, engine) =
                ProtocolLoopHarness::build(Arc::clone(&state), &[("conn", &["session"])], 1, 4096);
            let engine_task = tokio::task::spawn_local(run_protocol_engine(engine));
            harness.send_request(extension_request(80, "conn", "session", "hang:shutdown"), 1);
            state.gate("shutdown").wait_started().await;
            harness.shutdown("bounded test shutdown");

            let terminal = harness.response().await;
            assert_eq!(terminal.request_id, 80);
            let ResponsePayload::RejectedResponse(rejection) = terminal.payload else {
                panic!("forced shutdown must synthesize a typed terminal");
            };
            assert_eq!(rejection.code, "ERR_AGENTOS_REQUEST_SHUTDOWN");
            tokio::time::timeout(TEST_TIMEOUT, engine_task)
                .await
                .expect("bounded protocol shutdown")
                .expect("protocol engine joined")
                .expect("shutdown is not a transport error");
            assert_eq!(harness.operations.snapshot().in_flight_requests, 0);
            assert_eq!(harness.progress_requests.snapshot().in_flight_requests, 0);
            assert_eq!(harness.callback_transport.pending_usage(), (0, 0));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn request_concurrency_real_loop_disconnect_cleans_up_without_leaks() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = Arc::new(GatedExtensionState::default());
            let (harness, engine) =
                ProtocolLoopHarness::build(Arc::clone(&state), &[("conn", &["session"])], 2, 4096);
            assert!(harness.fill_ordinary_output() > 0);
            let engine_task = tokio::task::spawn_local(run_protocol_engine(engine));
            harness.send_request(
                extension_request(90, "conn", "session", "hang:disconnect"),
                1,
            );
            state.gate("disconnect").wait_started().await;
            harness.send_request(
                extension_request(91, "conn", "session", "events:writer-failure"),
                1,
            );
            assert_eq!(harness.response().await.request_id, 91);
            assert_eq!(harness.operations.snapshot().in_flight_requests, 2);
            let callback_waiter = harness
                .callback_transport
                .register_async_waiter(-90)
                .expect("register callback waiter before writer failure");

            harness
                .output
                .close_with_error("deterministic writer failure");
            harness
                .write_error_tx
                .try_send(String::from("deterministic writer failure"))
                .expect("inject transport failure");

            let error = tokio::time::timeout(TEST_TIMEOUT, engine_task)
                .await
                .expect("bounded disconnect cleanup")
                .expect("protocol engine joined")
                .expect_err("writer failure remains visible");
            assert!(error.to_string().contains("deterministic writer failure"));
            assert_eq!(harness.operations.snapshot().in_flight_requests, 0);
            assert_eq!(harness.progress_requests.snapshot().in_flight_requests, 0);
            assert_eq!(harness.callback_transport.pending_usage(), (0, 0));
            let callback_error = callback_waiter
                .await
                .expect("callback waiter settled")
                .expect_err("callback waiter receives writer failure");
            assert!(callback_error
                .to_string()
                .contains("deterministic writer failure"));
            assert_eq!(harness.writer.ordinary_budget.usage(), (0, 0));
            assert_eq!(harness.writer.terminal_budget.usage(), (0, 0));
            assert_eq!(harness.writer.progress_budget.usage(), (0, 0));
            assert_eq!(harness.writer.rejection_budget.usage(), (0, 0));
        })
        .await;
}
