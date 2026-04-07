// Host function injection via v8::FunctionTemplate

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use v8::MapFnTo;
use v8::ValueDeserializerHelper;
use v8::ValueSerializerHelper;

use crate::host_call::BridgeCallContext;

// CBOR codec flag: when true, use CBOR (via ciborium) instead of V8
// ValueSerializer/ValueDeserializer for IPC payloads. Activated by
// SECURE_EXEC_V8_CODEC=cbor for runtimes whose node:v8 module doesn't
// produce real V8 serialization format (e.g. Bun).
static USE_CBOR_CODEC: AtomicBool = AtomicBool::new(false);

/// Initialize the codec from the SECURE_EXEC_V8_CODEC environment variable.
/// Call once at process startup before any sessions are created.
pub fn init_codec() {
    if let Ok(val) = std::env::var("SECURE_EXEC_V8_CODEC") {
        if val == "cbor" {
            USE_CBOR_CODEC.store(true, Ordering::Relaxed);
        }
    }
}

/// Returns true if the CBOR codec is active.
pub fn is_cbor_codec() -> bool {
    USE_CBOR_CODEC.load(Ordering::Relaxed)
}

/// External references for V8 snapshot serialization.
/// Maps function pointer indices in the snapshot to current addresses.
/// Must be identical at snapshot creation and restore time.
pub fn external_refs() -> &'static v8::ExternalReferences {
    static REFS: OnceLock<v8::ExternalReferences> = OnceLock::new();
    REFS.get_or_init(|| {
        v8::ExternalReferences::new(&[
            v8::ExternalReference {
                function: sync_bridge_callback.map_fn_to(),
            },
            v8::ExternalReference {
                function: async_bridge_callback.map_fn_to(),
            },
        ])
    })
}

// Minimal delegate for V8 ValueSerializer — throws DataCloneError as a V8 exception
struct DefaultSerializerDelegate;

impl v8::ValueSerializerImpl for DefaultSerializerDelegate {
    fn throw_data_clone_error<'s>(
        &self,
        scope: &mut v8::HandleScope<'s>,
        message: v8::Local<'s, v8::String>,
    ) {
        let exc = v8::Exception::error(scope, message);
        scope.throw_exception(exc);
    }
}

// Minimal delegate for V8 ValueDeserializer — default callbacks are sufficient
struct DefaultDeserializerDelegate;

impl v8::ValueDeserializerImpl for DefaultDeserializerDelegate {}

/// Serialize a V8 value to bytes using V8's built-in ValueSerializer.
/// Handles all V8 types natively: primitives, strings, arrays, objects,
/// Uint8Array, Date, Map, Set, RegExp, Error, and circular references.
/// When CBOR codec is active, uses ciborium instead.
pub fn serialize_v8_value(
    scope: &mut v8::HandleScope,
    value: v8::Local<v8::Value>,
) -> Result<Vec<u8>, String> {
    if is_cbor_codec() {
        return serialize_cbor_value(scope, value);
    }
    let context = scope.get_current_context();
    let serializer = v8::ValueSerializer::new(scope, Box::new(DefaultSerializerDelegate));
    serializer.write_header();
    serializer
        .write_value(context, value)
        .ok_or_else(|| "V8 ValueSerializer: failed to serialize value".to_string())?;
    Ok(serializer.release())
}

/// Serialize a V8 value into a pre-allocated buffer.
///
/// The buffer is cleared (not deallocated) before use, preserving capacity.
/// V8's serializer allocates internally; the result is copied into the buffer
/// so the buffer grows to high-water mark across calls.
pub fn serialize_v8_value_into(
    scope: &mut v8::HandleScope,
    value: v8::Local<v8::Value>,
    buf: &mut Vec<u8>,
) -> Result<(), String> {
    let released = serialize_v8_value(scope, value)?;
    buf.clear();
    buf.extend_from_slice(&released);
    Ok(())
}

/// Deserialize bytes back to a V8 value using V8's built-in ValueDeserializer.
/// The bytes must have been produced by serialize_v8_value() or node:v8.serialize().
pub fn deserialize_v8_value<'s>(
    scope: &mut v8::HandleScope<'s>,
    data: &[u8],
) -> Result<v8::Local<'s, v8::Value>, String> {
    if is_cbor_codec() {
        return deserialize_cbor_value(scope, data);
    }
    let context = scope.get_current_context();
    let deserializer =
        v8::ValueDeserializer::new(scope, Box::new(DefaultDeserializerDelegate), data);
    deserializer
        .read_header(context)
        .ok_or_else(|| "V8 ValueDeserializer: invalid header".to_string())?;
    deserializer
        .read_value(context)
        .ok_or_else(|| "V8 ValueDeserializer: failed to deserialize value".to_string())
}

// ── CBOR codec ──

/// Convert a V8 value to a ciborium::Value for CBOR serialization.
fn v8_to_cbor(scope: &mut v8::HandleScope, value: v8::Local<v8::Value>) -> ciborium::Value {
    if value.is_null_or_undefined() {
        return ciborium::Value::Null;
    }
    if value.is_boolean() {
        return ciborium::Value::Bool(value.boolean_value(scope));
    }
    if value.is_int32() {
        return ciborium::Value::Integer(value.int32_value(scope).unwrap_or(0).into());
    }
    if value.is_number() {
        return ciborium::Value::Float(value.number_value(scope).unwrap_or(0.0));
    }
    if value.is_string() {
        let s = value.to_rust_string_lossy(scope);
        return ciborium::Value::Text(s);
    }
    if value.is_array_buffer_view() {
        let view = v8::Local::<v8::ArrayBufferView>::try_from(value).unwrap();
        let len = view.byte_length();
        let mut buf = vec![0u8; len];
        view.copy_contents(&mut buf);
        return ciborium::Value::Bytes(buf);
    }
    if value.is_array() {
        let arr = v8::Local::<v8::Array>::try_from(value).unwrap();
        let len = arr.length();
        let mut items = Vec::with_capacity(len as usize);
        for i in 0..len {
            if let Some(elem) = arr.get_index(scope, i) {
                items.push(v8_to_cbor(scope, elem));
            } else {
                items.push(ciborium::Value::Null);
            }
        }
        return ciborium::Value::Array(items);
    }
    if value.is_object() {
        let obj = value.to_object(scope).unwrap();
        let names = obj
            .get_own_property_names(scope, v8::GetPropertyNamesArgs::default())
            .unwrap_or_else(|| v8::Array::new(scope, 0));
        let len = names.length();
        let mut entries = Vec::with_capacity(len as usize);
        for i in 0..len {
            let key = names.get_index(scope, i).unwrap();
            let key_str = key.to_rust_string_lossy(scope);
            let val = obj.get(scope, key).unwrap_or_else(|| v8::undefined(scope).into());
            entries.push((ciborium::Value::Text(key_str), v8_to_cbor(scope, val)));
        }
        return ciborium::Value::Map(entries);
    }
    ciborium::Value::Null
}

/// Convert a ciborium::Value to a V8 value.
fn cbor_to_v8<'s>(
    scope: &mut v8::HandleScope<'s>,
    value: &ciborium::Value,
) -> v8::Local<'s, v8::Value> {
    match value {
        ciborium::Value::Null => v8::null(scope).into(),
        ciborium::Value::Bool(b) => v8::Boolean::new(scope, *b).into(),
        ciborium::Value::Integer(n) => {
            let n: i128 = (*n).into();
            if n >= i32::MIN as i128 && n <= i32::MAX as i128 {
                v8::Integer::new(scope, n as i32).into()
            } else {
                v8::Number::new(scope, n as f64).into()
            }
        }
        ciborium::Value::Float(f) => v8::Number::new(scope, *f).into(),
        ciborium::Value::Text(s) => {
            v8::String::new(scope, s).unwrap().into()
        }
        ciborium::Value::Bytes(b) => {
            let len = b.len();
            let ab = v8::ArrayBuffer::new(scope, len);
            if len > 0 {
                let bs = ab.get_backing_store();
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        b.as_ptr(),
                        bs.data().unwrap().as_ptr() as *mut u8,
                        len,
                    );
                }
            }
            v8::Uint8Array::new(scope, ab, 0, len).unwrap().into()
        }
        ciborium::Value::Array(items) => {
            let arr = v8::Array::new(scope, items.len() as i32);
            for (i, item) in items.iter().enumerate() {
                let val = cbor_to_v8(scope, item);
                arr.set_index(scope, i as u32, val);
            }
            arr.into()
        }
        ciborium::Value::Map(entries) => {
            let obj = v8::Object::new(scope);
            for (k, v) in entries {
                let key = cbor_to_v8(scope, k);
                let val = cbor_to_v8(scope, v);
                obj.set(scope, key, val);
            }
            obj.into()
        }
        ciborium::Value::Tag(_, inner) => cbor_to_v8(scope, inner),
        _ => v8::undefined(scope).into(),
    }
}

/// Serialize a V8 value to CBOR bytes.
pub fn serialize_cbor_value(
    scope: &mut v8::HandleScope,
    value: v8::Local<v8::Value>,
) -> Result<Vec<u8>, String> {
    let cbor_val = v8_to_cbor(scope, value);
    let mut buf = Vec::new();
    ciborium::into_writer(&cbor_val, &mut buf)
        .map_err(|e| format!("CBOR encode failed: {}", e))?;
    Ok(buf)
}

/// Deserialize CBOR bytes to a V8 value.
pub fn deserialize_cbor_value<'s>(
    scope: &mut v8::HandleScope<'s>,
    data: &[u8],
) -> Result<v8::Local<'s, v8::Value>, String> {
    let cbor_val: ciborium::Value = ciborium::from_reader(data)
        .map_err(|e| format!("CBOR decode failed: {}", e))?;
    Ok(cbor_to_v8(scope, &cbor_val))
}

/// Pre-allocated serialization buffers reused across bridge calls within a session.
/// Grows to high-water mark; cleared (not deallocated) between calls via buf.clear().
pub struct SessionBuffers {
    /// Buffer for V8 ValueSerializer output (args serialization)
    pub ser_buf: Vec<u8>,
}

impl SessionBuffers {
    pub fn new() -> Self {
        SessionBuffers {
            ser_buf: Vec::with_capacity(256),
        }
    }
}

/// Data attached to each sync bridge function via v8::External.
/// BridgeFnStore keeps these heap allocations alive for the session.
struct SyncBridgeFnData {
    ctx: *const BridgeCallContext,
    buffers: *const RefCell<SessionBuffers>,
    method: String,
}

/// Opaque store that keeps bridge function data alive.
/// Must be held for the lifetime of the V8 context.
pub struct BridgeFnStore {
    // Box ensures stable pointer address for v8::External data when Vec grows
    #[allow(clippy::vec_box)]
    _data: Vec<Box<SyncBridgeFnData>>,
}

/// Data attached to each async bridge function via v8::External.
struct AsyncBridgeFnData {
    ctx: *const BridgeCallContext,
    pending: *const PendingPromises,
    buffers: *const RefCell<SessionBuffers>,
    method: String,
}

/// Opaque store that keeps async bridge function data alive.
/// Must be held for the lifetime of the V8 context.
pub struct AsyncBridgeFnStore {
    // Box ensures stable pointer address for v8::External data when Vec grows
    #[allow(clippy::vec_box)]
    _data: Vec<Box<AsyncBridgeFnData>>,
}

/// Stores pending promise resolvers keyed by call_id.
/// Single-threaded: only accessed from the session thread.
pub struct PendingPromises {
    map: RefCell<HashMap<u64, v8::Global<v8::PromiseResolver>>>,
}

impl PendingPromises {
    pub fn new() -> Self {
        PendingPromises {
            map: RefCell::new(HashMap::new()),
        }
    }

    /// Store a resolver for a given call_id.
    pub fn insert(&self, call_id: u64, resolver: v8::Global<v8::PromiseResolver>) {
        self.map.borrow_mut().insert(call_id, resolver);
    }

    /// Remove and return the resolver for a given call_id.
    pub fn remove(&self, call_id: u64) -> Option<v8::Global<v8::PromiseResolver>> {
        self.map.borrow_mut().remove(&call_id)
    }

    /// Number of pending promises.
    pub fn len(&self) -> usize {
        self.map.borrow().len()
    }
}

/// Register sync-blocking bridge functions on the V8 global object.
///
/// Each registered function, when called from V8:
/// 1. Serializes arguments as a V8 Array via ValueSerializer
/// 2. Sends a BridgeCall over IPC via BridgeCallContext
/// 3. Blocks on read() for the BridgeResponse
/// 4. Returns the V8-deserialized result or throws a V8 exception
///
/// The BridgeCallContext pointer must remain valid for the lifetime of the V8 context.
/// The returned BridgeFnStore must also be kept alive.
pub fn register_sync_bridge_fns(
    scope: &mut v8::HandleScope,
    ctx: *const BridgeCallContext,
    buffers: *const RefCell<SessionBuffers>,
    methods: &[&str],
) -> BridgeFnStore {
    let context = scope.get_current_context();
    let global = context.global(scope);
    let mut data = Vec::with_capacity(methods.len());

    for &method_name in methods {
        let boxed = Box::new(SyncBridgeFnData {
            ctx,
            buffers,
            method: method_name.to_string(),
        });
        // Pointer to heap allocation — stable while Box exists in data vec
        let ptr = &*boxed as *const SyncBridgeFnData as *mut c_void;
        data.push(boxed);

        let external = v8::External::new(scope, ptr);
        let template = v8::FunctionTemplate::builder(sync_bridge_callback)
            .data(external.into())
            .build(scope);
        let func = template.get_function(scope).unwrap();

        let key = v8::String::new(scope, method_name).unwrap();
        global.set(scope, key.into(), func.into());
    }

    BridgeFnStore { _data: data }
}

/// V8 FunctionTemplate callback for sync-blocking bridge calls.
fn sync_bridge_callback(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    // Extract SyncBridgeFnData from External
    let external = match v8::Local::<v8::External>::try_from(args.data()) {
        Ok(ext) => ext,
        Err(_) => {
            let msg =
                v8::String::new(scope, "internal error: missing bridge function data").unwrap();
            let exc = v8::Exception::error(scope, msg);
            scope.throw_exception(exc);
            return;
        }
    };
    // SAFETY: pointer is valid while BridgeFnStore is alive (same session lifetime)
    let data = unsafe { &*(external.value() as *const SyncBridgeFnData) };
    let ctx = unsafe { &*data.ctx };
    let buffers = unsafe { &*data.buffers };

    // Serialize V8 arguments into reusable buffer (avoids per-call allocation)
    let encoded_args = {
        let mut bufs = buffers.borrow_mut();
        match serialize_v8_args_into(scope, &args, &mut bufs.ser_buf) {
            Ok(()) => bufs.ser_buf.clone(),
            Err(err) => {
                let msg = v8::String::new(scope, &format!("bridge serialization error: {}", err))
                    .unwrap();
                let exc = v8::Exception::error(scope, msg);
                scope.throw_exception(exc);
                return;
            }
        }
    };

    // Perform sync-blocking bridge call
    match ctx.sync_call(&data.method, encoded_args) {
        Ok(Some(result_bytes)) => {
            // Try V8 deserialization in a TryCatch scope; if it fails,
            // treat as raw binary (Uint8Array) — covers status=2 raw binary
            // and V8 version incompatibilities for typed arrays.
            let v8_val = {
                let tc = &mut v8::TryCatch::new(scope);
                deserialize_v8_value(tc, &result_bytes).ok()
            };
            if let Some(val) = v8_val {
                rv.set(val);
            } else {
                // Fallback: raw binary data → Uint8Array
                let len = result_bytes.len();
                let ab = v8::ArrayBuffer::new(scope, len);
                if len > 0 {
                    let bs = ab.get_backing_store();
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            result_bytes.as_ptr(),
                            bs.data().unwrap().as_ptr() as *mut u8,
                            len,
                        );
                    }
                }
                let arr = v8::Uint8Array::new(scope, ab, 0, len).unwrap();
                rv.set(arr.into());
            }
        }
        Ok(None) => {
            rv.set_undefined();
        }
        Err(err_msg) => {
            let msg = v8::String::new(scope, &err_msg).unwrap();
            let exc = v8::Exception::error(scope, msg);
            scope.throw_exception(exc);
        }
    }
}

/// Register async promise-returning bridge functions on the V8 global object.
///
/// Each registered function, when called from V8:
/// 1. Creates a v8::PromiseResolver
/// 2. Stores the resolver + call_id in PendingPromises
/// 3. Sends a BridgeCall over IPC (non-blocking write)
/// 4. Returns the promise to V8
///
/// The BridgeCallContext and PendingPromises pointers must remain valid
/// for the lifetime of the V8 context.
pub fn register_async_bridge_fns(
    scope: &mut v8::HandleScope,
    ctx: *const BridgeCallContext,
    pending: *const PendingPromises,
    buffers: *const RefCell<SessionBuffers>,
    methods: &[&str],
) -> AsyncBridgeFnStore {
    let context = scope.get_current_context();
    let global = context.global(scope);
    let mut data = Vec::with_capacity(methods.len());

    for &method_name in methods {
        let boxed = Box::new(AsyncBridgeFnData {
            ctx,
            pending,
            buffers,
            method: method_name.to_string(),
        });
        // Pointer to heap allocation — stable while Box exists in data vec
        let ptr = &*boxed as *const AsyncBridgeFnData as *mut c_void;
        data.push(boxed);

        let external = v8::External::new(scope, ptr);
        let template = v8::FunctionTemplate::builder(async_bridge_callback)
            .data(external.into())
            .build(scope);
        let func = template.get_function(scope).unwrap();

        let key = v8::String::new(scope, method_name).unwrap();
        global.set(scope, key.into(), func.into());
    }

    AsyncBridgeFnStore { _data: data }
}

/// V8 FunctionTemplate callback for async promise-returning bridge calls.
fn async_bridge_callback(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    // Extract AsyncBridgeFnData from External
    let external = match v8::Local::<v8::External>::try_from(args.data()) {
        Ok(ext) => ext,
        Err(_) => {
            let msg = v8::String::new(scope, "internal error: missing async bridge function data")
                .unwrap();
            let exc = v8::Exception::error(scope, msg);
            scope.throw_exception(exc);
            return;
        }
    };
    // SAFETY: pointer is valid while AsyncBridgeFnStore is alive (same session lifetime)
    let data = unsafe { &*(external.value() as *const AsyncBridgeFnData) };
    let ctx = unsafe { &*data.ctx };
    let pending = unsafe { &*data.pending };
    let buffers = unsafe { &*data.buffers };

    // Create PromiseResolver
    let resolver = match v8::PromiseResolver::new(scope) {
        Some(r) => r,
        None => {
            let msg = v8::String::new(scope, "failed to create PromiseResolver").unwrap();
            let exc = v8::Exception::error(scope, msg);
            scope.throw_exception(exc);
            return;
        }
    };

    // Get the promise to return to V8
    let promise = resolver.get_promise(scope);

    // Serialize V8 arguments into reusable buffer (avoids per-call allocation)
    let encoded_args = {
        let mut bufs = buffers.borrow_mut();
        match serialize_v8_args_into(scope, &args, &mut bufs.ser_buf) {
            Ok(()) => bufs.ser_buf.clone(),
            Err(err) => {
                let msg = v8::String::new(scope, &format!("bridge serialization error: {}", err))
                    .unwrap();
                let exc = v8::Exception::error(scope, msg);
                scope.throw_exception(exc);
                return;
            }
        }
    };

    // Send BridgeCall (non-blocking write)
    match ctx.async_send(&data.method, encoded_args) {
        Ok(call_id) => {
            // Store resolver in pending promises map
            let global_resolver = v8::Global::new(scope, resolver);
            pending.insert(call_id, global_resolver);
        }
        Err(err_msg) => {
            // Reject the promise immediately if send fails
            let msg = v8::String::new(scope, &err_msg).unwrap();
            let exc = v8::Exception::error(scope, msg);
            resolver.reject(scope, exc);
        }
    }

    // Return the promise
    rv.set(promise.into());
}

/// Replace stub bridge functions on a snapshot-restored context with real
/// session-local bridge functions. Overwrites the 38 stub globals with
/// functions backed by session-local BridgeCallContext and SessionBuffers.
///
/// Returns (BridgeFnStore, AsyncBridgeFnStore) that must be kept alive
/// for the lifetime of the V8 context.
pub fn replace_bridge_fns(
    scope: &mut v8::HandleScope,
    ctx: *const BridgeCallContext,
    pending: *const PendingPromises,
    buffers: *const RefCell<SessionBuffers>,
    sync_fns: &[&str],
    async_fns: &[&str],
) -> (BridgeFnStore, AsyncBridgeFnStore) {
    let sync_store = register_sync_bridge_fns(scope, ctx, buffers, sync_fns);
    let async_store = register_async_bridge_fns(scope, ctx, pending, buffers, async_fns);
    (sync_store, async_store)
}

/// Register stub bridge functions on the V8 global for snapshot creation.
///
/// Uses the same sync_bridge_callback / async_bridge_callback as real
/// functions (required for ExternalReferences in snapshot serialization)
/// but WITHOUT v8::External data. If a stub is accidentally called during
/// snapshot creation, the callback gracefully throws a V8 exception
/// (args.data() is not External -> "missing bridge function data" error).
///
/// After snapshot restore, these stubs are replaced with real functions
/// that have proper External data pointing to a session-local BridgeCallContext.
pub fn register_stub_bridge_fns(
    scope: &mut v8::HandleScope,
    sync_fns: &[&str],
    async_fns: &[&str],
) {
    let context = scope.get_current_context();
    let global = context.global(scope);

    // Register sync bridge functions as stubs (no External data)
    for &method_name in sync_fns {
        let template = v8::FunctionTemplate::builder(sync_bridge_callback).build(scope);
        let func = template.get_function(scope).unwrap();
        let key = v8::String::new(scope, method_name).unwrap();
        global.set(scope, key.into(), func.into());
    }

    // Register async bridge functions as stubs (no External data)
    for &method_name in async_fns {
        let template = v8::FunctionTemplate::builder(async_bridge_callback).build(scope);
        let func = template.get_function(scope).unwrap();
        let key = v8::String::new(scope, method_name).unwrap();
        global.set(scope, key.into(), func.into());
    }
}

/// Serialize V8 function arguments into a pre-allocated buffer.
/// The buffer is cleared and reused across calls (grows to high-water mark).
fn serialize_v8_args_into(
    scope: &mut v8::HandleScope,
    args: &v8::FunctionCallbackArguments,
    buf: &mut Vec<u8>,
) -> Result<(), String> {
    let count = args.length();
    let array = v8::Array::new(scope, count);
    for i in 0..count {
        array.set_index(scope, i as u32, args.get(i));
    }
    serialize_v8_value_into(scope, array.into(), buf)
}

/// Resolve or reject a pending async bridge promise by call_id.
///
/// Called when a BridgeResponse arrives during the session event loop.
/// Flushes microtasks after resolution to process .then() handlers.
pub fn resolve_pending_promise(
    scope: &mut v8::HandleScope,
    pending: &PendingPromises,
    call_id: u64,
    result: Option<Vec<u8>>,
    error: Option<String>,
) -> Result<(), String> {
    let resolver_global = pending
        .remove(call_id)
        .ok_or_else(|| format!("no pending promise for call_id {}", call_id))?;
    let resolver = v8::Local::new(scope, &resolver_global);

    if let Some(err_msg) = error {
        let msg = v8::String::new(scope, &err_msg).unwrap();
        let exc = v8::Exception::error(scope, msg);
        resolver.reject(scope, exc);
    } else if let Some(result_bytes) = result {
        // Try V8 deserialization in a TryCatch scope; fallback to raw binary
        let v8_val = {
            let tc = &mut v8::TryCatch::new(scope);
            deserialize_v8_value(tc, &result_bytes).ok()
        };
        if let Some(val) = v8_val {
            resolver.resolve(scope, val);
        } else {
            // Fallback: raw binary data → Uint8Array
            let len = result_bytes.len();
            let ab = v8::ArrayBuffer::new(scope, len);
            if len > 0 {
                let bs = ab.get_backing_store();
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        result_bytes.as_ptr(),
                        bs.data().unwrap().as_ptr() as *mut u8,
                        len,
                    );
                }
            }
            let arr = v8::Uint8Array::new(scope, ab, 0, len).unwrap();
            resolver.resolve(scope, arr.into());
        }
    } else {
        let undef = v8::undefined(scope);
        resolver.resolve(scope, undef.into());
    }

    // Flush microtasks after resolution
    scope.perform_microtask_checkpoint();

    Ok(())
}
