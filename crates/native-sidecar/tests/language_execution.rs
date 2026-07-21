mod support;

use agentos_native_sidecar::wire;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use support::{
    authenticate_wire, create_vm_wire, create_vm_wire_with_metadata,
    dispose_vm_and_close_session_wire, new_sidecar, open_session_wire, temp_dir, wire_request,
    wire_session, wire_vm,
};

fn process_options(execution_id: Option<String>) -> wire::ProcessExecutionOptions {
    let execution_id = execution_id.or_else(|| Some(String::from("test-execution")));
    wire::ProcessExecutionOptions {
        identity: wire::ExecutionIdentityOptions { context_id: None },
        output: wire::ExecutionOutputOptions {
            capture: Some(wire::ExecutionOutputCapture::All),
            retain_events: Some(true),
        },
        operation_id: execution_id,
        background: Some(true),
        cwd: None,
        env: Some(HashMap::new()),
        args: Vec::new(),
        stdin: None,
        timeout_ms: Some(30_000),
        pty: None,
    }
}

fn context_process_options(context_id: &str) -> wire::ProcessExecutionOptions {
    let mut process = process_options(None);
    process.identity.context_id = Some(context_id.to_owned());
    process.operation_id = None;
    process.background = Some(false);
    process
}

fn create_context(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<support::RecordingBridge>,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    context_id: &str,
) {
    let response = sidecar
        .dispatch_wire_blocking(wire_request(
            89,
            wire_vm(connection_id, session_id, vm_id),
            wire::RequestPayload::CreateContextRequest(wire::CreateContextRequest {
                context_id: context_id.to_owned(),
            }),
        ))
        .expect("create context");
    assert!(matches!(
        response.response.payload,
        wire::ResponsePayload::ExecutionDescriptorResponse(_)
    ));
}

fn accepted_execution_id(result: wire::WireDispatchResult) -> String {
    match result.response.payload {
        wire::ResponsePayload::ExecutionAcceptedResponse(response) => response.operation_id,
        other => panic!("unexpected language execution response: {other:?}"),
    }
}

fn wait_for_execution(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<support::RecordingBridge>,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    execution_id: &str,
) -> wire::ExecutionCompletedResponse {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let event = sidecar
            .poll_event_wire_blocking(
                &wire_session(connection_id, session_id),
                Duration::from_millis(100),
            )
            .expect("poll execution event");
        if let Some(event) = event {
            if let wire::EventPayload::ExecutionCompletedEvent(completed) = event.payload {
                if completed.execution_id == execution_id {
                    break;
                }
            }
        }
        assert!(Instant::now() < deadline, "language execution timed out");
    }

    let response = sidecar
        .dispatch_wire_blocking(wire_request(
            90,
            wire_vm(connection_id, session_id, vm_id),
            wire::RequestPayload::WaitExecutionRequest(wire::WaitExecutionRequest {
                execution_id: execution_id.to_owned(),
            }),
        ))
        .expect("wait for execution result");
    match response.response.payload {
        wire::ResponsePayload::ExecutionCompletedResponse(result) => result,
        other => panic!("unexpected wait response: {other:?}"),
    }
}

fn reset_execution(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<support::RecordingBridge>,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    execution_id: &str,
) {
    let response = sidecar
        .dispatch_wire_blocking(wire_request(
            91,
            wire_vm(connection_id, session_id, vm_id),
            wire::RequestPayload::ResetExecutionRequest(wire::ResetExecutionRequest {
                execution_id: execution_id.to_owned(),
            }),
        ))
        .expect("reset retained execution");
    match response.response.payload {
        wire::ResponsePayload::ExecutionDescriptorResponse(response) => {
            assert_eq!(response.execution.state, wire::ExecutionState::Idle);
            assert_eq!(response.execution.retained_language, None);
        }
        other => panic!("unexpected reset response: {other:?}"),
    }
}

#[test]
fn attached_execution_is_ephemeral_and_does_not_capture_by_default() {
    let mut sidecar = new_sidecar("language-execution-ephemeral");
    let connection_id = authenticate_wire(&mut sidecar, "ephemeral-connection");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let cwd = temp_dir("language-execution-ephemeral-cwd");
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        wire::GuestRuntimeKind::JavaScript,
        &cwd,
    );
    let mut process = process_options(Some(String::from("unused")));
    process.identity = wire::ExecutionIdentityOptions { context_id: None };
    process.operation_id = None;
    process.background = Some(false);
    process.output = wire::ExecutionOutputOptions {
        capture: None,
        retain_events: None,
    };
    let accepted = sidecar
        .dispatch_wire_blocking(wire_request(
            4,
            wire_vm(&connection_id, &session_id, &vm_id),
            wire::RequestPayload::JavaScriptExecutionRequest(wire::JavaScriptExecutionRequest {
                process,
                source: String::from("console.log('stdout'); console.error('stderr');"),
                format: Some(wire::JavaScriptModuleFormat::CommonJs),
                file_path: None,
                inputs: None,
            }),
        ))
        .expect("start ephemeral execution");
    let operation_id = accepted_execution_id(accepted);
    let result = wait_for_execution(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        &operation_id,
    );
    assert!(result.execution.is_none());
    assert!(result.stdout.is_none());
    assert!(result.stderr.is_none());

    let listed = sidecar
        .dispatch_wire_blocking(wire_request(
            5,
            wire_vm(&connection_id, &session_id, &vm_id),
            wire::RequestPayload::ListExecutionsRequest,
        ))
        .expect("list executions");
    match listed.response.payload {
        wire::ResponsePayload::ExecutionListResponse(response) => {
            assert!(response.executions.is_empty());
        }
        other => panic!("unexpected list response: {other:?}"),
    }
    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
}

#[test]
fn background_and_retained_events_require_valid_identity() {
    let mut sidecar = new_sidecar("language-execution-identity-validation");
    let connection_id = authenticate_wire(&mut sidecar, "identity-validation-connection");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let cwd = temp_dir("language-execution-identity-validation-cwd");
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        wire::GuestRuntimeKind::JavaScript,
        &cwd,
    );
    for (request_id, background, retain_events) in [(4, true, false), (5, false, true)] {
        let mut process = process_options(Some(String::from("unused")));
        process.identity = wire::ExecutionIdentityOptions { context_id: None };
        process.operation_id = None;
        process.background = Some(background);
        process.output.retain_events = Some(retain_events);
        let rejected = sidecar
            .dispatch_wire_blocking(wire_request(
                request_id,
                wire_vm(&connection_id, &session_id, &vm_id),
                wire::RequestPayload::ShellExecutionRequest(wire::ShellExecutionRequest {
                    process,
                    command: String::from("true"),
                }),
            ))
            .expect("validate execution identity");
        match rejected.response.payload {
            wire::ResponsePayload::RejectedResponse(response) => {
                assert_eq!(response.code, "invalid_execution_identity");
            }
            other => panic!("expected identity rejection, got {other:?}"),
        }
    }
    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
}

#[test]
fn contexts_are_explicit_unique_and_language_pinned() {
    let mut sidecar = new_sidecar("language-context-contract");
    let connection_id = authenticate_wire(&mut sidecar, "context-contract-connection");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let cwd = temp_dir("language-context-contract-cwd");
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        wire::GuestRuntimeKind::JavaScript,
        &cwd,
    );
    create_context(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "analysis",
    );

    let conflict = sidecar
        .dispatch_wire_blocking(wire_request(
            4,
            wire_vm(&connection_id, &session_id, &vm_id),
            wire::RequestPayload::CreateContextRequest(wire::CreateContextRequest {
                context_id: String::from("analysis"),
            }),
        ))
        .expect("reject duplicate context");
    assert!(matches!(
        conflict.response.payload,
        wire::ResponsePayload::RejectedResponse(ref rejected)
            if rejected.code == "context_conflict"
    ));

    let unknown = sidecar
        .dispatch_wire_blocking(wire_request(
            5,
            wire_vm(&connection_id, &session_id, &vm_id),
            wire::RequestPayload::JavaScriptExecutionRequest(wire::JavaScriptExecutionRequest {
                process: context_process_options("missing"),
                source: String::from("1"),
                format: Some(wire::JavaScriptModuleFormat::CommonJs),
                file_path: None,
                inputs: None,
            }),
        ))
        .expect("reject unknown context");
    assert!(matches!(
        unknown.response.payload,
        wire::ResponsePayload::RejectedResponse(ref rejected)
            if rejected.code == "context_not_found"
    ));

    let first = sidecar
        .dispatch_wire_blocking(wire_request(
            6,
            wire_vm(&connection_id, &session_id, &vm_id),
            wire::RequestPayload::JavaScriptExecutionRequest(wire::JavaScriptExecutionRequest {
                process: context_process_options("analysis"),
                source: String::from("globalThis.answer = 42"),
                format: Some(wire::JavaScriptModuleFormat::CommonJs),
                file_path: None,
                inputs: None,
            }),
        ))
        .expect("pin JavaScript context");
    let operation_id = accepted_execution_id(first);
    wait_for_execution(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        &operation_id,
    );

    let mismatch = sidecar
        .dispatch_wire_blocking(wire_request(
            7,
            wire_vm(&connection_id, &session_id, &vm_id),
            wire::RequestPayload::PythonEvaluationRequest(wire::PythonEvaluationRequest {
                process: context_process_options("analysis"),
                expression: String::from("42"),
                inputs: None,
            }),
        ))
        .expect("reject cross-language context reuse");
    assert!(matches!(
        mismatch.response.payload,
        wire::ResponsePayload::RejectedResponse(ref rejected)
            if rejected.code == "context_language_mismatch"
    ));

    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
}

#[test]
fn stderr_capture_is_independent_from_retained_event_history() {
    let mut sidecar = new_sidecar("language-execution-stderr-capture");
    let connection_id = authenticate_wire(&mut sidecar, "stderr-capture-connection");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let cwd = temp_dir("language-execution-stderr-capture-cwd");
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        wire::GuestRuntimeKind::JavaScript,
        &cwd,
    );
    let mut process = process_options(None);
    process.output = wire::ExecutionOutputOptions {
        capture: Some(wire::ExecutionOutputCapture::Stderr),
        retain_events: Some(false),
    };
    let accepted = sidecar
        .dispatch_wire_blocking(wire_request(
            4,
            wire_vm(&connection_id, &session_id, &vm_id),
            wire::RequestPayload::JavaScriptExecutionRequest(wire::JavaScriptExecutionRequest {
                process,
                source: String::from("console.log('not captured'); console.error('captured');"),
                format: Some(wire::JavaScriptModuleFormat::CommonJs),
                file_path: None,
                inputs: None,
            }),
        ))
        .expect("start retained capture execution");
    let execution_id = accepted_execution_id(accepted);
    let result = wait_for_execution(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        &execution_id,
    );
    assert!(result.stdout.is_none());
    assert_eq!(
        String::from_utf8(result.stderr.expect("captured stderr")).expect("UTF-8 stderr"),
        "captured\n"
    );
    assert!(result.stdout_truncated.is_none());
    assert_eq!(result.stderr_truncated, Some(false));

    let page = sidecar
        .dispatch_wire_blocking(wire_request(
            5,
            wire_vm(&connection_id, &session_id, &vm_id),
            wire::RequestPayload::ReadExecutionOutputRequest(wire::ReadExecutionOutputRequest {
                execution_id: execution_id.clone(),
                cursor: None,
                limit: None,
            }),
        ))
        .expect("read disabled retained output");
    match page.response.payload {
        wire::ResponsePayload::RejectedResponse(rejected) => {
            assert_eq!(rejected.code, "execution_output_not_retained");
        }
        other => panic!("expected output retention rejection, got {other:?}"),
    }
    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
}

#[test]
fn context_survives_completed_operation_ttl() {
    let mut sidecar = new_sidecar("language-execution-retention-ttl");
    let connection_id = authenticate_wire(&mut sidecar, "retention-ttl-connection");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let cwd = temp_dir("language-execution-retention-ttl-cwd");
    let mut metadata = HashMap::new();
    metadata.insert(
        String::from("limits.execution.completed_ttl_ms"),
        String::from("25"),
    );
    let (vm_id, _) = create_vm_wire_with_metadata(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        wire::GuestRuntimeKind::JavaScript,
        &cwd,
        metadata,
    );
    create_context(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "durable-context",
    );
    let process = context_process_options("durable-context");
    let accepted = sidecar
        .dispatch_wire_blocking(wire_request(
            4,
            wire_vm(&connection_id, &session_id, &vm_id),
            wire::RequestPayload::JavaScriptExecutionRequest(wire::JavaScriptExecutionRequest {
                process,
                source: String::from("globalThis.retained = true"),
                format: Some(wire::JavaScriptModuleFormat::CommonJs),
                file_path: None,
                inputs: None,
            }),
        ))
        .expect("start retained execution");
    let execution_id = accepted_execution_id(accepted);
    let result = wait_for_execution(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        &execution_id,
    );
    assert!(result.execution.is_some());
    std::thread::sleep(Duration::from_millis(75));

    let retained = sidecar
        .dispatch_wire_blocking(wire_request(
            5,
            wire_vm(&connection_id, &session_id, &vm_id),
            wire::RequestPayload::GetExecutionRequest(wire::GetExecutionRequest { execution_id }),
        ))
        .expect("get retained context");
    match retained.response.payload {
        wire::ResponsePayload::ExecutionDescriptorResponse(response) => {
            assert_eq!(response.execution.execution_id, "durable-context");
        }
        other => panic!("expected retained context descriptor, got {other:?}"),
    }
    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
}

#[test]
fn background_process_records_do_not_appear_as_contexts() {
    let mut sidecar = new_sidecar("language-execution-retention-count");
    let connection_id = authenticate_wire(&mut sidecar, "retention-count-connection");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let cwd = temp_dir("language-execution-retention-count-cwd");
    let mut metadata = HashMap::new();
    metadata.insert(
        String::from("limits.execution.max_completed_executions"),
        String::from("1"),
    );
    let (vm_id, _) = create_vm_wire_with_metadata(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        wire::GuestRuntimeKind::JavaScript,
        &cwd,
        metadata,
    );
    for (request_id, execution_id) in [(4, "first"), (5, "second")] {
        let mut process = process_options(Some(execution_id.to_owned()));
        process.background = Some(true);
        let accepted = sidecar
            .dispatch_wire_blocking(wire_request(
                request_id,
                wire_vm(&connection_id, &session_id, &vm_id),
                wire::RequestPayload::JavaScriptExecutionRequest(
                    wire::JavaScriptExecutionRequest {
                        process,
                        source: String::from("1 + 1"),
                        format: Some(wire::JavaScriptModuleFormat::CommonJs),
                        file_path: None,
                        inputs: None,
                    },
                ),
            ))
            .expect("start retained execution");
        let admitted_id = accepted_execution_id(accepted);
        wait_for_execution(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            &admitted_id,
        );
    }

    let listed = sidecar
        .dispatch_wire_blocking(wire_request(
            6,
            wire_vm(&connection_id, &session_id, &vm_id),
            wire::RequestPayload::ListExecutionsRequest,
        ))
        .expect("list bounded executions");
    match listed.response.payload {
        wire::ResponsePayload::ExecutionListResponse(response) => {
            assert!(response.executions.is_empty());
        }
        other => panic!("unexpected list response: {other:?}"),
    }
    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
}

#[test]
fn javascript_execution_reuses_retained_context() {
    let mut sidecar = new_sidecar("language-execution-retained-js");
    let connection_id = authenticate_wire(&mut sidecar, "language-execution-connection");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let cwd = temp_dir("language-execution-retained-js-cwd");
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        wire::GuestRuntimeKind::JavaScript,
        &cwd,
    );
    create_context(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "test-execution",
    );
    let first = sidecar
        .dispatch_wire_blocking(wire_request(
            4,
            wire_vm(&connection_id, &session_id, &vm_id),
            wire::RequestPayload::JavaScriptExecutionRequest(wire::JavaScriptExecutionRequest {
                process: context_process_options("test-execution"),
                source: String::from(
                    "import { sep } from 'node:path'; let retainedAnswer = sep === '/' ? 41 : 0;",
                ),
                format: Some(wire::JavaScriptModuleFormat::Module),
                file_path: None,
                inputs: None,
            }),
        ))
        .expect("start first JavaScript operation");
    let execution_id = accepted_execution_id(first);
    let first_result = wait_for_execution(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        &execution_id,
    );
    assert_eq!(first_result.outcome, wire::ExecutionOutcome::Succeeded);

    let mut fresh_process_options = context_process_options(&execution_id);
    fresh_process_options.args = vec![String::from("-e"), String::from("void 0")];
    let process = sidecar
        .dispatch_wire_blocking(wire_request(
            5,
            wire_vm(&connection_id, &session_id, &vm_id),
            wire::RequestPayload::ArgvExecutionRequest(wire::ArgvExecutionRequest {
                process: fresh_process_options,
                command: String::from("node"),
            }),
        ))
        .expect("start fresh process between retained operations");
    assert!(
        process.events.is_empty(),
        "interleaved process failed during admission: {:?}",
        process.events
    );
    assert_eq!(accepted_execution_id(process), execution_id);
    let process_result = wait_for_execution(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        &execution_id,
    );
    assert_eq!(process_result.outcome, wire::ExecutionOutcome::Succeeded);

    let typescript = sidecar
        .dispatch_wire_blocking(wire_request(
            6,
            wire_vm(&connection_id, &session_id, &vm_id),
            wire::RequestPayload::TypeScriptExecutionRequest(wire::TypeScriptExecutionRequest {
                process: context_process_options(&execution_id),
                source: String::from(
                    "const typedAnswer: number = sep === '/' ? retainedAnswer + 1 : 0;",
                ),
                file_path: Some(String::from("retained-cell.ts")),
                tsconfig_path: None,
                compiler_options: None,
                inputs: None,
            }),
        ))
        .expect("start retained TypeScript operation");
    assert_eq!(accepted_execution_id(typescript), execution_id);
    let typescript_result = wait_for_execution(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        &execution_id,
    );
    assert_eq!(typescript_result.outcome, wire::ExecutionOutcome::Succeeded);

    let second = sidecar
        .dispatch_wire_blocking(wire_request(
            7,
            wire_vm(&connection_id, &session_id, &vm_id),
            wire::RequestPayload::JavaScriptEvaluationRequest(wire::JavaScriptEvaluationRequest {
                process: context_process_options(&execution_id),
                expression: String::from("typedAnswer"),
                format: Some(wire::JavaScriptModuleFormat::Module),
                file_path: None,
                inputs: None,
            }),
        ))
        .expect("start retained JavaScript evaluation");
    assert_eq!(accepted_execution_id(second), execution_id);
    let second_result = wait_for_execution(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        &execution_id,
    );
    assert_eq!(second_result.outcome, wire::ExecutionOutcome::Succeeded);
    assert_eq!(second_result.evaluation_value.as_deref(), Some("42"));

    reset_execution(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        &execution_id,
    );
    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
}

#[test]
fn typescript_check_reports_semantic_diagnostics() {
    let mut sidecar = new_sidecar("language-execution-typescript-check");
    let connection_id = authenticate_wire(&mut sidecar, "typescript-check-connection");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let cwd = temp_dir("language-execution-typescript-check-cwd");
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        wire::GuestRuntimeKind::JavaScript,
        &cwd,
    );
    let check = sidecar
        .dispatch_wire_blocking(wire_request(
            4,
            wire_vm(&connection_id, &session_id, &vm_id),
            wire::RequestPayload::TypeScriptCheckRequest(wire::TypeScriptCheckRequest {
                identity: wire::ExecutionIdentityOptions { context_id: None },
                output: wire::ExecutionOutputOptions {
                    capture: Some(wire::ExecutionOutputCapture::Stderr),
                    retain_events: Some(false),
                },
                source: String::from("const answer: string = 42;"),
                cwd: None,
                file_path: Some(String::from("answer.ts")),
                tsconfig_path: None,
                compiler_options: None,
                timeout_ms: Some(30_000),
            }),
        ))
        .expect("start TypeScript check");
    let execution_id = accepted_execution_id(check);
    let result = wait_for_execution(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        &execution_id,
    );
    assert_eq!(
        result.outcome,
        wire::ExecutionOutcome::Succeeded,
        "TypeScript check failed: {}",
        String::from_utf8_lossy(result.stderr.as_deref().unwrap_or_default())
    );
    let check_result: serde_json::Value = serde_json::from_str(
        result
            .type_script_check_result
            .as_deref()
            .expect("TypeScript check result"),
    )
    .expect("decode TypeScript check result");
    assert_eq!(check_result["hasErrors"], true);
    assert!(check_result["diagnostics"]
        .as_array()
        .is_some_and(|diagnostics| diagnostics.iter().any(|item| item["code"] == 2322)));

    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
}

#[test]
fn python_execution_reuses_retained_globals() {
    let mut sidecar = new_sidecar("language-execution-retained-python");
    let connection_id = authenticate_wire(&mut sidecar, "retained-python-connection");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let cwd = temp_dir("language-execution-retained-python-cwd");
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        wire::GuestRuntimeKind::Python,
        &cwd,
    );
    create_context(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "test-execution",
    );
    let first = sidecar
        .dispatch_wire_blocking(wire_request(
            4,
            wire_vm(&connection_id, &session_id, &vm_id),
            wire::RequestPayload::PythonExecutionRequest(wire::PythonExecutionRequest {
                process: context_process_options("test-execution"),
                source: String::from(
                    "import asyncio\nawait asyncio.sleep(0)\nretained_answer = 41",
                ),
                inputs: None,
            }),
        ))
        .expect("start first Python operation");
    let execution_id = accepted_execution_id(first);
    assert_eq!(
        wait_for_execution(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            &execution_id,
        )
        .outcome,
        wire::ExecutionOutcome::Succeeded
    );

    let second = sidecar
        .dispatch_wire_blocking(wire_request(
            5,
            wire_vm(&connection_id, &session_id, &vm_id),
            wire::RequestPayload::PythonEvaluationRequest(wire::PythonEvaluationRequest {
                process: context_process_options(&execution_id),
                expression: String::from("retained_answer + 1"),
                inputs: None,
            }),
        ))
        .expect("start retained Python evaluation");
    assert_eq!(accepted_execution_id(second), execution_id);
    let result = wait_for_execution(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        &execution_id,
    );
    assert_eq!(result.outcome, wire::ExecutionOutcome::Succeeded);
    assert_eq!(result.evaluation_value.as_deref(), Some("42"));

    reset_execution(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        &execution_id,
    );
    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
}

#[test]
fn execution_timeout_is_enforced_by_the_sidecar() {
    let mut sidecar = new_sidecar("language-execution-timeout");
    let connection_id = authenticate_wire(&mut sidecar, "execution-timeout-connection");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let cwd = temp_dir("language-execution-timeout-cwd");
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        wire::GuestRuntimeKind::JavaScript,
        &cwd,
    );
    let mut options = process_options(None);
    options.timeout_ms = Some(100);
    let started_at = Instant::now();
    let started = sidecar
        .dispatch_wire_blocking(wire_request(
            4,
            wire_vm(&connection_id, &session_id, &vm_id),
            wire::RequestPayload::JavaScriptExecutionRequest(wire::JavaScriptExecutionRequest {
                process: options,
                source: String::from("while (true) {}"),
                format: Some(wire::JavaScriptModuleFormat::CommonJs),
                file_path: None,
                inputs: None,
            }),
        ))
        .expect("start timed JavaScript operation");
    let execution_id = accepted_execution_id(started);
    let result = wait_for_execution(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        &execution_id,
    );
    assert_eq!(result.outcome, wire::ExecutionOutcome::TimedOut);
    assert_eq!(
        result.error.as_ref().map(|error| error.code.as_str()),
        Some("execution_timed_out")
    );
    assert!(
        started_at.elapsed() < Duration::from_secs(5),
        "sidecar timeout did not terminate the guest promptly"
    );

    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
}

#[test]
fn evaluation_rejects_non_json_values_with_a_structured_result() {
    let mut sidecar = new_sidecar("language-execution-json-evaluation");
    let connection_id = authenticate_wire(&mut sidecar, "json-evaluation-connection");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let cwd = temp_dir("language-execution-json-evaluation-cwd");
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        wire::GuestRuntimeKind::JavaScript,
        &cwd,
    );
    let started = sidecar
        .dispatch_wire_blocking(wire_request(
            4,
            wire_vm(&connection_id, &session_id, &vm_id),
            wire::RequestPayload::JavaScriptEvaluationRequest(wire::JavaScriptEvaluationRequest {
                process: process_options(None),
                expression: String::from("undefined"),
                format: Some(wire::JavaScriptModuleFormat::CommonJs),
                file_path: None,
                inputs: None,
            }),
        ))
        .expect("start non-JSON JavaScript evaluation");
    let execution_id = accepted_execution_id(started);
    let result = wait_for_execution(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        &execution_id,
    );
    assert_eq!(result.outcome, wire::ExecutionOutcome::Failed);
    assert_eq!(
        result.error.as_ref().map(|error| error.code.as_str()),
        Some("evaluation_serialization_failed")
    );
    assert!(result
        .error
        .as_ref()
        .is_some_and(|error| error.message.contains("JSON-serializable")));
    assert_eq!(result.evaluation_value, None);
    assert_eq!(result.type_script_check_result, None);

    reset_execution(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        &execution_id,
    );

    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
}

#[test]
fn package_mutations_are_exclusive_across_executions() {
    let mut sidecar = new_sidecar("language-execution-package-mutation");
    let connection_id = authenticate_wire(&mut sidecar, "package-mutation-connection");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let cwd = temp_dir("language-execution-package-mutation-cwd");
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        wire::GuestRuntimeKind::Python,
        &cwd,
    );
    let first = sidecar
        .dispatch_wire_blocking(wire_request(
            4,
            wire_vm(&connection_id, &session_id, &vm_id),
            wire::RequestPayload::PythonInstallRequest(wire::PythonInstallRequest {
                identity: wire::ExecutionIdentityOptions { context_id: None },
                output: wire::ExecutionOutputOptions {
                    capture: Some(wire::ExecutionOutputCapture::None),
                    retain_events: Some(false),
                },
                cwd: None,
                env: None,
                timeout_ms: Some(30_000),
                packages: vec![String::from("agentos-package-mutation-test")],
                upgrade: None,
                requirements_file: None,
                index_url: Some(String::from("http://127.0.0.1:9/simple")),
                extra_index_urls: Vec::new(),
            }),
        ))
        .expect("start first package mutation");
    let first_execution_id = accepted_execution_id(first);

    let second = sidecar
        .dispatch_wire_blocking(wire_request(
            5,
            wire_vm(&connection_id, &session_id, &vm_id),
            wire::RequestPayload::NpmProjectInstallRequest(wire::NpmProjectInstallRequest {
                identity: wire::ExecutionIdentityOptions { context_id: None },
                output: wire::ExecutionOutputOptions {
                    capture: Some(wire::ExecutionOutputCapture::None),
                    retain_events: Some(false),
                },
                cwd: None,
                env: None,
                timeout_ms: Some(30_000),
                frozen: None,
            }),
        ))
        .expect("reject concurrent package mutation");
    match second.response.payload {
        wire::ResponsePayload::RejectedResponse(rejected) => {
            assert_eq!(rejected.code, "execution_busy");
            assert!(rejected.message.contains(&first_execution_id));
            assert!(rejected.message.contains("serialized at VM scope"));
        }
        other => panic!("expected package mutation rejection, got {other:?}"),
    }

    sidecar
        .dispatch_wire_blocking(wire_request(
            6,
            wire_vm(&connection_id, &session_id, &vm_id),
            wire::RequestPayload::CancelExecutionRequest(wire::CancelExecutionRequest {
                execution_id: first_execution_id.clone(),
            }),
        ))
        .expect("cancel first package mutation");
    assert_eq!(
        wait_for_execution(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            &first_execution_id,
        )
        .outcome,
        wire::ExecutionOutcome::Cancelled
    );

    let resumed = sidecar
        .dispatch_wire_blocking(wire_request(
            7,
            wire_vm(&connection_id, &session_id, &vm_id),
            wire::RequestPayload::NpmProjectInstallRequest(wire::NpmProjectInstallRequest {
                identity: wire::ExecutionIdentityOptions { context_id: None },
                output: wire::ExecutionOutputOptions {
                    capture: Some(wire::ExecutionOutputCapture::None),
                    retain_events: Some(false),
                },
                cwd: None,
                env: None,
                timeout_ms: Some(30_000),
                frozen: None,
            }),
        ))
        .expect("start package mutation after the prior one completed");
    let resumed_execution_id = accepted_execution_id(resumed);
    sidecar
        .dispatch_wire_blocking(wire_request(
            8,
            wire_vm(&connection_id, &session_id, &vm_id),
            wire::RequestPayload::CancelExecutionRequest(wire::CancelExecutionRequest {
                execution_id: resumed_execution_id.clone(),
            }),
        ))
        .expect("cancel resumed package mutation");
    assert_eq!(
        wait_for_execution(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            &resumed_execution_id,
        )
        .outcome,
        wire::ExecutionOutcome::Cancelled
    );

    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
}

#[test]
fn background_lifecycle_replays_cancels_resets_and_deletes() {
    let mut sidecar = new_sidecar("language-execution-background-lifecycle");
    let connection_id = authenticate_wire(&mut sidecar, "background-lifecycle-connection");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let cwd = temp_dir("language-execution-detached-lifecycle-cwd");
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        wire::GuestRuntimeKind::JavaScript,
        &cwd,
    );

    let started = sidecar
        .dispatch_wire_blocking(wire_request(
            4,
            wire_vm(&connection_id, &session_id, &vm_id),
            wire::RequestPayload::JavaScriptExecutionRequest(wire::JavaScriptExecutionRequest {
                process: process_options(None),
                source: String::from("console.log('replay-me')"),
                format: Some(wire::JavaScriptModuleFormat::CommonJs),
                file_path: None,
                inputs: None,
            }),
        ))
        .expect("start background JavaScript operation");
    let execution_id = accepted_execution_id(started);
    assert_eq!(
        wait_for_execution(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            &execution_id,
        )
        .outcome,
        wire::ExecutionOutcome::Succeeded
    );

    let replay = sidecar
        .dispatch_wire_blocking(wire_request(
            5,
            wire_vm(&connection_id, &session_id, &vm_id),
            wire::RequestPayload::ReadExecutionOutputRequest(wire::ReadExecutionOutputRequest {
                execution_id: execution_id.clone(),
                cursor: None,
                limit: Some(1),
            }),
        ))
        .expect("read retained execution output");
    match replay.response.payload {
        wire::ResponsePayload::ExecutionOutputPageResponse(page) => {
            assert!(!page.truncated);
            assert_eq!(page.events.len(), 1);
            assert!(String::from_utf8_lossy(&page.events[0].chunk).contains("replay-me"));
            assert!(!page.next_cursor.is_empty());
        }
        other => panic!("expected execution output page, got {other:?}"),
    }

    reset_execution(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        &execution_id,
    );

    let mut cancel_options = process_options(Some(execution_id.clone()));
    cancel_options.timeout_ms = Some(30_000);
    let cancellable = sidecar
        .dispatch_wire_blocking(wire_request(
            6,
            wire_vm(&connection_id, &session_id, &vm_id),
            wire::RequestPayload::JavaScriptExecutionRequest(wire::JavaScriptExecutionRequest {
                process: cancel_options,
                source: String::from("while (true) {}"),
                format: Some(wire::JavaScriptModuleFormat::CommonJs),
                file_path: None,
                inputs: None,
            }),
        ))
        .expect("start cancellable execution");
    assert_eq!(accepted_execution_id(cancellable), execution_id);
    sidecar
        .dispatch_wire_blocking(wire_request(
            7,
            wire_vm(&connection_id, &session_id, &vm_id),
            wire::RequestPayload::CancelExecutionRequest(wire::CancelExecutionRequest {
                execution_id: execution_id.clone(),
            }),
        ))
        .expect("cancel execution");
    assert_eq!(
        wait_for_execution(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            &execution_id,
        )
        .outcome,
        wire::ExecutionOutcome::Cancelled
    );

    let deleted = sidecar
        .dispatch_wire_blocking(wire_request(
            8,
            wire_vm(&connection_id, &session_id, &vm_id),
            wire::RequestPayload::DeleteExecutionRequest(wire::DeleteExecutionRequest {
                execution_id: execution_id.clone(),
            }),
        ))
        .expect("delete idle execution");
    match deleted.response.payload {
        wire::ResponsePayload::ExecutionDeletedResponse(response) => {
            assert_eq!(response.execution_id, execution_id);
        }
        other => panic!("expected execution deletion, got {other:?}"),
    }

    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);
}
