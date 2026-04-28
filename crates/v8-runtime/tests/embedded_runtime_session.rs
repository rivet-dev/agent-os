use agent_os_v8_runtime::embedded_runtime::{shared_embedded_runtime, EmbeddedV8Runtime};
use agent_os_v8_runtime::runtime_protocol::{RuntimeCommand, RuntimeEvent, SessionMessage};
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

static NEXT_TEST_SESSION_ID: AtomicU64 = AtomicU64::new(1);

fn next_session_id() -> String {
    format!(
        "embedded-runtime-session-{}",
        NEXT_TEST_SESSION_ID.fetch_add(1, Ordering::Relaxed)
    )
}

fn register_and_create_session(
    runtime: &Arc<EmbeddedV8Runtime>,
    session_id: &str,
) -> io::Result<mpsc::Receiver<RuntimeEvent>> {
    let receiver = runtime.register_session(session_id)?;
    runtime.dispatch(RuntimeCommand::CreateSession {
        session_id: session_id.to_owned(),
        heap_limit_mb: None,
        cpu_time_limit_ms: None,
    })?;
    Ok(receiver)
}

fn dispatch_execute(
    runtime: &EmbeddedV8Runtime,
    session_id: &str,
    mode: u8,
    bridge_code: &str,
    user_code: &str,
) -> io::Result<()> {
    runtime.dispatch(RuntimeCommand::SendToSession {
        session_id: session_id.to_owned(),
        message: SessionMessage::Execute {
            mode,
            file_path: String::new(),
            bridge_code: bridge_code.to_owned(),
            post_restore_script: String::new(),
            user_code: user_code.to_owned(),
        },
    })
}

fn wait_for_execution_result(
    receiver: &mpsc::Receiver<RuntimeEvent>,
    session_id: &str,
) -> RuntimeEvent {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .expect("timed out waiting for execution result");
        let event = receiver
            .recv_timeout(remaining)
            .expect("runtime event should arrive before timeout");
        if matches!(
            &event,
            RuntimeEvent::ExecutionResult {
                session_id: event_session_id,
                ..
            } if event_session_id == session_id
        ) {
            return event;
        }
    }
}

fn wait_for_bridge_call(receiver: &mpsc::Receiver<RuntimeEvent>, session_id: &str) -> RuntimeEvent {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .expect("timed out waiting for bridge call");
        let event = receiver
            .recv_timeout(remaining)
            .expect("bridge call should arrive before timeout");
        if matches!(
            &event,
            RuntimeEvent::BridgeCall {
                session_id: event_session_id,
                ..
            } if event_session_id == session_id
        ) {
            return event;
        }
    }
}

fn assert_execution_ok(receiver: &mpsc::Receiver<RuntimeEvent>, session_id: &str) {
    let event = wait_for_execution_result(receiver, session_id);
    match event {
        RuntimeEvent::ExecutionResult {
            exit_code,
            error,
            exports,
            ..
        } => {
            assert_eq!(exit_code, 0, "expected successful execution result");
            assert!(error.is_none(), "unexpected execution error: {error:?}");
            assert!(
                exports.is_none(),
                "script execution should not export values"
            );
        }
        other => panic!("expected execution result, got {other:?}"),
    }
}

fn wait_until(message: &str, predicate: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if predicate() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("{message}");
}

fn assert_create_destroy_reuses_session_ids() -> io::Result<()> {
    let runtime = shared_embedded_runtime()?;
    let session_id = next_session_id();

    let _receiver = register_and_create_session(&runtime, &session_id)?;
    assert!(
        runtime.session_count() >= 1,
        "embedded runtime should track created sessions"
    );

    let duplicate_error = runtime
        .dispatch(RuntimeCommand::CreateSession {
            session_id: session_id.clone(),
            heap_limit_mb: None,
            cpu_time_limit_ms: None,
        })
        .expect_err("duplicate sessions should be rejected");
    assert_eq!(duplicate_error.kind(), io::ErrorKind::Other);

    runtime.session_handle(session_id.clone()).destroy()?;
    assert_eq!(
        runtime.session_count(),
        0,
        "destroying the only test session should return the runtime to zero sessions"
    );

    let _receiver = register_and_create_session(&runtime, &session_id)?;
    runtime.session_handle(session_id).destroy()?;
    assert_eq!(
        runtime.session_count(),
        0,
        "recreated sessions should also tear down cleanly"
    );

    Ok(())
}

fn assert_warmed_snapshot_bridge_state() -> io::Result<()> {
    let runtime = Arc::new(EmbeddedV8Runtime::new(Some(1))?);
    let session_id = next_session_id();
    let receiver = register_and_create_session(&runtime, &session_id)?;
    let bridge_code = "(function() { globalThis.__snapshotMarker = 'warm'; })();";

    runtime.dispatch(RuntimeCommand::WarmSnapshot {
        bridge_code: bridge_code.to_owned(),
    })?;
    dispatch_execute(
        runtime.as_ref(),
        &session_id,
        0,
        bridge_code,
        "if (globalThis.__snapshotMarker !== 'warm') { throw new Error(`saw ${globalThis.__snapshotMarker}`); }",
    )?;
    assert_execution_ok(&receiver, &session_id);

    runtime.dispatch(RuntimeCommand::DestroySession {
        session_id: session_id.clone(),
    })?;
    runtime.unregister_session(&session_id);
    Ok(())
}

fn assert_snapshot_rebuild_on_bridge_change() -> io::Result<()> {
    let runtime = Arc::new(EmbeddedV8Runtime::new(Some(1))?);
    let session_id = next_session_id();
    let receiver = register_and_create_session(&runtime, &session_id)?;
    let bridge_a = "(function() { globalThis.__bridgeSnapshot = 'A'; })();";
    let bridge_b = "(function() { globalThis.__bridgeSnapshot = 'B'; })();";

    dispatch_execute(
        runtime.as_ref(),
        &session_id,
        0,
        bridge_a,
        "if (globalThis.__bridgeSnapshot !== 'A') { throw new Error(`saw ${globalThis.__bridgeSnapshot}`); }",
    )?;
    assert_execution_ok(&receiver, &session_id);

    dispatch_execute(
        runtime.as_ref(),
        &session_id,
        0,
        bridge_b,
        "if (globalThis.__bridgeSnapshot !== 'B') { throw new Error(`saw ${globalThis.__bridgeSnapshot}`); }",
    )?;
    assert_execution_ok(&receiver, &session_id);

    runtime.dispatch(RuntimeCommand::DestroySession {
        session_id: session_id.clone(),
    })?;
    runtime.unregister_session(&session_id);
    Ok(())
}

fn assert_queued_work_waits_for_slot_release() -> io::Result<()> {
    let runtime = Arc::new(EmbeddedV8Runtime::new(Some(1))?);
    let session_a = next_session_id();
    let session_b = next_session_id();
    let receiver_a = register_and_create_session(&runtime, &session_a)?;

    wait_until(
        "expected the first embedded session to occupy the only slot before the second session is created",
        || runtime.active_slot_count() == 1 && runtime.session_count() == 1,
    );

    dispatch_execute(
        runtime.as_ref(),
        &session_a,
        1,
        "",
        "await new Promise(() => {});",
    )?;

    let receiver_b = register_and_create_session(&runtime, &session_b)?;
    dispatch_execute(
        runtime.as_ref(),
        &session_b,
        0,
        "(function() { globalThis.__queuedSession = 'released'; })();",
        "if (globalThis.__queuedSession !== 'released') { throw new Error(`saw ${globalThis.__queuedSession}`); }",
    )?;

    wait_until(
        "expected one active slot with the second session still queued",
        || runtime.active_slot_count() == 1 && runtime.session_count() == 2,
    );
    assert!(
        receiver_b.recv_timeout(Duration::from_millis(150)).is_err(),
        "queued session should not emit an execution result before the first slot is released"
    );

    runtime.dispatch(RuntimeCommand::DestroySession {
        session_id: session_a.clone(),
    })?;
    let terminated = wait_for_execution_result(&receiver_a, &session_a);
    assert!(
        matches!(
            terminated,
            RuntimeEvent::ExecutionResult {
                exit_code: 1,
                ref error,
                ..
            } if error.as_ref().is_some_and(|error| error.message == "Execution terminated")
        ),
        "destroying the in-flight session should terminate its pending execution"
    );

    assert_execution_ok(&receiver_b, &session_b);

    runtime.dispatch(RuntimeCommand::DestroySession {
        session_id: session_b.clone(),
    })?;
    runtime.unregister_session(&session_a);
    runtime.unregister_session(&session_b);
    wait_until(
        "expected all embedded sessions and slots to drain after teardown",
        || runtime.session_count() == 0 && runtime.active_slot_count() == 0,
    );
    Ok(())
}

fn assert_shared_runtime_handles_share_concurrency_quota() -> io::Result<()> {
    let runtime = Arc::new(EmbeddedV8Runtime::new(Some(3))?);
    let clients = (0..4)
        .map(|_| Arc::clone(&runtime))
        .collect::<Vec<Arc<EmbeddedV8Runtime>>>();
    let session_ids = (0..4).map(|_| next_session_id()).collect::<Vec<_>>();
    let mut receivers = clients
        .iter()
        .zip(session_ids.iter())
        .take(3)
        .map(|(client, session_id)| register_and_create_session(client, session_id))
        .collect::<io::Result<Vec<_>>>()?;

    wait_until(
        "expected the first three embedded sessions to occupy the shared slots before the fourth session is created",
        || runtime.active_slot_count() == 3 && runtime.session_count() == 3,
    );

    receivers.push(register_and_create_session(&clients[3], &session_ids[3])?);

    for (client, session_id) in clients.iter().zip(session_ids.iter()).take(3) {
        dispatch_execute(
            client.as_ref(),
            session_id,
            1,
            "",
            "await new Promise(() => {});",
        )?;
    }
    dispatch_execute(
        clients[3].as_ref(),
        &session_ids[3],
        0,
        "(function() { globalThis.__sharedQuota = 'released'; })();",
        "if (globalThis.__sharedQuota !== 'released') { throw new Error(`saw ${globalThis.__sharedQuota}`); }",
    )?;

    wait_until(
        "expected one runtime-wide slot budget shared across all embedded runtime handles",
        || runtime.active_slot_count() == 3 && runtime.session_count() == 4,
    );
    assert!(
        receivers[3].recv_timeout(Duration::from_millis(150)).is_err(),
        "the fourth client should stay queued while the first three handles occupy the shared slots"
    );

    runtime.dispatch(RuntimeCommand::DestroySession {
        session_id: session_ids[0].clone(),
    })?;
    let terminated = wait_for_execution_result(&receivers[0], &session_ids[0]);
    assert!(
        matches!(
            terminated,
            RuntimeEvent::ExecutionResult {
                exit_code: 1,
                ref error,
                ..
            } if error.as_ref().is_some_and(|error| error.message == "Execution terminated")
        ),
        "destroying one in-flight session should release a shared slot for queued handles"
    );

    assert_execution_ok(&receivers[3], &session_ids[3]);

    for session_id in session_ids.iter().skip(1) {
        runtime.dispatch(RuntimeCommand::DestroySession {
            session_id: session_id.clone(),
        })?;
    }
    for session_id in &session_ids {
        runtime.unregister_session(session_id);
    }
    wait_until(
        "expected all shared-runtime sessions and slots to drain after teardown",
        || runtime.session_count() == 0 && runtime.active_slot_count() == 0,
    );
    Ok(())
}

fn assert_terminate_interrupts_sync_bridge_wait() -> io::Result<()> {
    let runtime = Arc::new(EmbeddedV8Runtime::new(Some(1))?);
    let session_id = next_session_id();
    let receiver = register_and_create_session(&runtime, &session_id)?;

    dispatch_execute(
        runtime.as_ref(),
        &session_id,
        0,
        "",
        "_loadFileSync('/never-responds');",
    )?;

    let bridge_call = wait_for_bridge_call(&receiver, &session_id);
    assert!(
        matches!(
            bridge_call,
            RuntimeEvent::BridgeCall { ref method, .. } if method == "_loadFileSync"
        ),
        "expected the blocked sync bridge call to be visible before termination"
    );

    let terminate_started = Instant::now();
    runtime.session_handle(session_id.clone()).terminate()?;
    let terminated = wait_for_execution_result(&receiver, &session_id);

    assert!(
        terminate_started.elapsed() < Duration::from_secs(1),
        "terminate() should return promptly while the sync bridge call is blocked"
    );
    assert!(
        matches!(
            terminated,
            RuntimeEvent::ExecutionResult {
                exit_code: 1,
                ref error,
                ..
            } if error.as_ref().is_some_and(|error| error.message == "Execution terminated")
        ),
        "terminate() should interrupt a blocked sync bridge call instead of waiting for a host response"
    );

    runtime.dispatch(RuntimeCommand::DestroySession {
        session_id: session_id.clone(),
    })?;
    runtime.unregister_session(&session_id);
    wait_until(
        "expected the terminated sync-bridge session to drain cleanly",
        || runtime.session_count() == 0 && runtime.active_slot_count() == 0,
    );
    Ok(())
}

#[test]
fn embedded_runtime_session_consolidated_behaviors() -> io::Result<()> {
    // Keep the embedded-runtime coverage in one test process. V8 teardown across
    // multiple integration tests still trips intermittent SIGSEGVs in this crate.
    assert_create_destroy_reuses_session_ids()?;
    assert_warmed_snapshot_bridge_state()?;
    assert_snapshot_rebuild_on_bridge_change()?;
    assert_queued_work_waits_for_slot_release()?;
    assert_shared_runtime_handles_share_concurrency_quota()?;
    assert_terminate_interrupts_sync_bridge_wait()?;
    Ok(())
}
