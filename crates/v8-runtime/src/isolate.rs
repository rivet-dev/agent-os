// V8 isolate lifecycle: platform init, create, configure, destroy

use std::collections::HashMap;
use std::sync::Once;

use crate::ipc::ExecutionError;

static V8_INIT: Once = Once::new();

#[repr(align(16))]
struct AlignedBytes<const N: usize>([u8; N]);

static ICU_COMMON_DATA: AlignedBytes<{ include_bytes!(concat!(env!("OUT_DIR"), "/icudtl.dat")).len() }> =
    AlignedBytes(*include_bytes!(concat!(env!("OUT_DIR"), "/icudtl.dat")));

#[derive(Default)]
pub struct PromiseRejectState {
    pub unhandled: HashMap<i32, ExecutionError>,
}

extern "C" fn promise_reject_callback(msg: v8::PromiseRejectMessage) {
    let scope = &mut unsafe { v8::CallbackScope::new(&msg) };
    let promise_id = msg.get_promise().get_identity_hash().get();
    match msg.get_event() {
        v8::PromiseRejectEvent::PromiseRejectWithNoHandler => {
            let error = {
                let scope = &mut v8::HandleScope::new(scope);
                let value = msg
                    .get_value()
                    .unwrap_or_else(|| v8::undefined(scope).into());
                crate::execution::extract_error_info(scope, value)
            };
            if let Some(state) = scope.get_slot_mut::<PromiseRejectState>() {
                state.unhandled.insert(promise_id, error);
            }
        }
        v8::PromiseRejectEvent::PromiseHandlerAddedAfterReject => {
            if let Some(state) = scope.get_slot_mut::<PromiseRejectState>() {
                state.unhandled.remove(&promise_id);
            }
        }
        _ => {}
    }
}

pub fn configure_isolate(isolate: &mut v8::OwnedIsolate) {
    isolate.set_slot(PromiseRejectState::default());
    isolate.set_promise_reject_callback(promise_reject_callback);
}

/// Initialize the V8 platform (once per process).
/// Safe to call multiple times; only the first call takes effect.
pub fn init_v8_platform() {
    V8_INIT.call_once(|| {
        v8::icu::set_common_data_74(&ICU_COMMON_DATA.0)
            .expect("failed to initialize V8 ICU common data");
        let platform = v8::new_default_platform(0, false).make_shared();
        v8::V8::initialize_platform(platform);
        v8::V8::initialize();
    });
}

/// Create a new V8 isolate with an optional heap limit in MB.
pub fn create_isolate(heap_limit_mb: Option<u32>) -> v8::OwnedIsolate {
    let mut params = v8::CreateParams::default();
    if let Some(limit) = heap_limit_mb {
        let limit_bytes = (limit as usize) * 1024 * 1024;
        params = params.heap_limits(0, limit_bytes);
    }
    let mut isolate = v8::Isolate::new(params);
    configure_isolate(&mut isolate);
    isolate
}

/// Create a new V8 context on the given isolate.
/// Returns a Global handle so the context can be reused across scopes.
pub fn create_context(isolate: &mut v8::OwnedIsolate) -> v8::Global<v8::Context> {
    let scope = &mut v8::HandleScope::new(isolate);
    let context = v8::Context::new(scope, Default::default());
    v8::Global::new(scope, context)
}

// V8 lifecycle tests are consolidated in execution::tests to avoid
// inter-test SIGSEGV from V8 global state issues.
