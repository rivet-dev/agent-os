use agentos_execution::backend::{
    DirectHostReplyHandle, DirectHostReplyTarget, HostCallIdentity, HostCallReply,
    HostServiceError, NearLimitWarning, NearLimitWarningHook, PayloadLimit,
};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct RecordingTarget {
    replies: Mutex<Vec<Result<HostCallReply, HostServiceError>>>,
}

impl DirectHostReplyTarget for RecordingTarget {
    fn claim(&self, _: u64) -> Result<bool, HostServiceError> {
        Ok(true)
    }

    fn respond(
        &self,
        _: u64,
        _: bool,
        result: Result<HostCallReply, HostServiceError>,
    ) -> Result<(), HostServiceError> {
        self.replies.lock().expect("reply lock").push(result);
        Ok(())
    }
}

#[derive(Default)]
struct RecordingWarnings(Mutex<Vec<NearLimitWarning>>);

impl NearLimitWarningHook for RecordingWarnings {
    fn warn(&self, warning: NearLimitWarning) {
        self.0.lock().expect("warning lock").push(warning);
    }
}

#[test]
fn direct_reply_admission_warns_and_preserves_typed_error_details() {
    let target = Arc::new(RecordingTarget::default());
    let warnings = Arc::new(RecordingWarnings::default());
    let limit = PayloadLimit::with_warning_hook(
        "runtime.resources.maxBridgeResponseBytes",
        150,
        Some(warnings.clone()),
    )
    .expect("reply limit");
    let reply = DirectHostReplyHandle::new_with_limit(
        HostCallIdentity {
            generation: 3,
            pid: 41,
            call_id: 7,
        },
        target.clone(),
        limit,
    )
    .expect("reply handle");

    let typed = HostServiceError::new("EACCES", "d".repeat(60))
        .with_details(serde_json::json!({ "path": "/private" }));
    reply.fail(typed.clone()).expect("typed error reply");

    let replies = target.replies.lock().expect("reply lock");
    assert_eq!(replies[0].as_ref().unwrap_err(), &typed);
    assert_eq!(warnings.0.lock().expect("warning lock").len(), 1);
}

#[test]
fn oversized_json_is_settled_as_a_named_limit_error() {
    let target = Arc::new(RecordingTarget::default());
    let limit = PayloadLimit::new("limits.bridge.maxReplyBytes", 32).expect("reply limit");
    let reply = DirectHostReplyHandle::new_with_limit(
        HostCallIdentity {
            generation: 3,
            pid: 41,
            call_id: 8,
        },
        target.clone(),
        limit,
    )
    .expect("reply handle");

    reply
        .succeed_json(serde_json::json!({ "payload": "x".repeat(256) }))
        .expect("settle typed limit reply");

    let replies = target.replies.lock().expect("reply lock");
    let error = replies[0].as_ref().unwrap_err();
    assert_eq!(error.code, "E2BIG");
    assert_eq!(
        error.details.as_ref().expect("limit details")["limitName"],
        "limits.bridge.maxReplyBytes"
    );
}

#[test]
fn common_payload_constructors_require_named_limits() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let backend = manifest.join("src/backend");
    let host = fs::read_to_string(manifest.join("src/host/mod.rs")).expect("host source");
    let reply = fs::read_to_string(backend.join("reply.rs")).expect("reply source");
    let event = fs::read_to_string(backend.join("event.rs")).expect("event source");
    let v8_host = fs::read_to_string(manifest.join("src/v8_host.rs")).expect("V8 adapter source");

    assert!(
        host.matches("limit: &PayloadLimit").count() >= 4,
        "every retained byte/string/vector/count constructor must require a named PayloadLimit"
    );
    assert!(
        reply.contains("payload_limit: PayloadLimit")
            && reply.contains("PayloadLimit::with_stderr_warning(")
            && reply.contains("\"limits.reactor.maxBridgeResponseBytes\"")
            && reply.contains("pub fn succeed_raw(")
            && reply.contains("pub fn succeed_json("),
        "direct replies must retain their configured named bound and expose pre-envelope admission"
    );
    assert!(
        !reply.contains("serde_json::to_vec"),
        "direct reply admission must never allocate an unbounded encoded JSON temporary"
    );
    assert!(
        event.contains("Warning(BoundedHostServiceError)")
            && event.contains("pub fn output(")
            && event.contains("pub fn warning("),
        "common output and warning events must be admitted through bounded constructors"
    );
    let pre_admit = v8_host
        .find("encoded_limit.admit_json(payload)")
        .expect("structured adapter-event pre-admission");
    let encode = v8_host
        .find("json_to_cbor_payload(payload)")
        .expect("adapter event encoding");
    assert!(
        pre_admit < encode,
        "structured adapter events must be admitted before CBOR construction"
    );
}
