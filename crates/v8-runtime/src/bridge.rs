// Host function injection via v8::FunctionTemplate

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::OnceLock;

use openssl::version as openssl_version;
use v8::MapFnTo;
use v8::ValueDeserializerHelper;
use v8::ValueSerializerHelper;

use crate::host_call::BridgeCallContext;

// CBOR codec flag: when true, use CBOR (via ciborium) instead of V8
// ValueSerializer/ValueDeserializer for IPC payloads. Activated by
// SECURE_EXEC_V8_CODEC=cbor for runtimes whose node:v8 module doesn't
// produce real V8 serialization format (e.g. Bun).
static USE_CBOR_CODEC: AtomicBool = AtomicBool::new(false);
static EMBEDDED_CBOR_USERS: AtomicUsize = AtomicUsize::new(0);

/// Initialize the codec from the SECURE_EXEC_V8_CODEC environment variable.
/// Call once at process startup before any sessions are created.
pub fn init_codec() {
    USE_CBOR_CODEC.store(configured_cbor_codec_enabled(), Ordering::Relaxed);
}

pub fn enable_cbor_codec() {
    USE_CBOR_CODEC.store(true, Ordering::Relaxed);
}

pub fn acquire_embedded_cbor_codec() {
    EMBEDDED_CBOR_USERS.fetch_add(1, Ordering::AcqRel);
    USE_CBOR_CODEC.store(true, Ordering::Relaxed);
}

pub fn release_embedded_cbor_codec() {
    let previous = EMBEDDED_CBOR_USERS.fetch_sub(1, Ordering::AcqRel);
    if previous <= 1 {
        USE_CBOR_CODEC.store(configured_cbor_codec_enabled(), Ordering::Relaxed);
    }
}

/// Returns true if the CBOR codec is active.
pub fn is_cbor_codec() -> bool {
    USE_CBOR_CODEC.load(Ordering::Relaxed)
}

fn configured_cbor_codec_enabled() -> bool {
    std::env::var("SECURE_EXEC_V8_CODEC")
        .map(|val| val == "cbor")
        .unwrap_or(false)
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
    serialize_v8_wire_value(scope, value)
}

/// Serialize a V8 value to bytes using V8's native wire format regardless of
/// the process-wide codec toggle.
pub fn serialize_v8_wire_value(
    scope: &mut v8::HandleScope,
    value: v8::Local<v8::Value>,
) -> Result<Vec<u8>, String> {
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
    deserialize_v8_wire_value(scope, data)
}

/// Deserialize bytes from V8's native wire format regardless of the
/// process-wide codec toggle.
pub fn deserialize_v8_wire_value<'s>(
    scope: &mut v8::HandleScope<'s>,
    data: &[u8],
) -> Result<v8::Local<'s, v8::Value>, String> {
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
            let val = obj
                .get(scope, key)
                .unwrap_or_else(|| v8::undefined(scope).into());
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
        ciborium::Value::Text(s) => v8::String::new(scope, s).unwrap().into(),
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
    ciborium::into_writer(&cbor_val, &mut buf).map_err(|e| format!("CBOR encode failed: {}", e))?;
    Ok(buf)
}

/// Deserialize CBOR bytes to a V8 value.
pub fn deserialize_cbor_value<'s>(
    scope: &mut v8::HandleScope<'s>,
    data: &[u8],
) -> Result<v8::Local<'s, v8::Value>, String> {
    let cbor_val: ciborium::Value =
        ciborium::from_reader(data).map_err(|e| format!("CBOR decode failed: {}", e))?;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ThreadResourceUsageSnapshot {
    user_cpu_us: u64,
    system_cpu_us: u64,
    max_rss_kib: i64,
    shared_memory_size: i64,
    unshared_data_size: i64,
    unshared_stack_size: i64,
    minor_page_faults: i64,
    major_page_faults: i64,
    swapped_out: i64,
    fs_read: i64,
    fs_write: i64,
    ipc_sent: i64,
    ipc_received: i64,
    signals_count: i64,
    voluntary_context_switches: i64,
    involuntary_context_switches: i64,
}

fn non_negative_c_long(value: libc::c_long) -> i64 {
    let normalized = i128::from(value).max(0);
    normalized.min(i128::from(i64::MAX)) as i64
}

fn timeval_to_micros(value: libc::timeval) -> u64 {
    let seconds = i128::from(value.tv_sec).max(0);
    let micros = i128::from(value.tv_usec).max(0);
    (seconds
        .saturating_mul(1_000_000)
        .saturating_add(micros)
        .min(i128::from(u64::MAX))) as u64
}

fn current_thread_resource_usage() -> Result<ThreadResourceUsageSnapshot, String> {
    let mut usage = MaybeUninit::<libc::rusage>::uninit();
    let result = unsafe { libc::getrusage(libc::RUSAGE_THREAD, usage.as_mut_ptr()) };
    if result != 0 {
        return Err(format!(
            "getrusage(RUSAGE_THREAD) failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let usage = unsafe { usage.assume_init() };
    Ok(ThreadResourceUsageSnapshot {
        user_cpu_us: timeval_to_micros(usage.ru_utime),
        system_cpu_us: timeval_to_micros(usage.ru_stime),
        max_rss_kib: non_negative_c_long(usage.ru_maxrss),
        shared_memory_size: non_negative_c_long(usage.ru_ixrss),
        unshared_data_size: non_negative_c_long(usage.ru_idrss),
        unshared_stack_size: non_negative_c_long(usage.ru_isrss),
        minor_page_faults: non_negative_c_long(usage.ru_minflt),
        major_page_faults: non_negative_c_long(usage.ru_majflt),
        swapped_out: non_negative_c_long(usage.ru_nswap),
        fs_read: non_negative_c_long(usage.ru_inblock),
        fs_write: non_negative_c_long(usage.ru_oublock),
        ipc_sent: non_negative_c_long(usage.ru_msgsnd),
        ipc_received: non_negative_c_long(usage.ru_msgrcv),
        signals_count: non_negative_c_long(usage.ru_nsignals),
        voluntary_context_switches: non_negative_c_long(usage.ru_nvcsw),
        involuntary_context_switches: non_negative_c_long(usage.ru_nivcsw),
    })
}

fn normalize_openssl_version(raw: &str) -> String {
    raw.split_whitespace().nth(1).unwrap_or(raw).to_string()
}

fn set_object_string_property<'s>(
    scope: &mut v8::HandleScope<'s>,
    object: v8::Local<'s, v8::Object>,
    key: &str,
    value: &str,
) {
    let key = v8::String::new(scope, key).expect("V8 string key");
    let value = v8::String::new(scope, value).expect("V8 string value");
    let _ = object.set(scope, key.into(), value.into());
}

fn set_object_number_property<'s>(
    scope: &mut v8::HandleScope<'s>,
    object: v8::Local<'s, v8::Object>,
    key: &str,
    value: f64,
) {
    let key = v8::String::new(scope, key).expect("V8 string key");
    let value = v8::Number::new(scope, value);
    let _ = object.set(scope, key.into(), value.into());
}

fn number_property_or_zero<'s>(
    scope: &mut v8::HandleScope<'s>,
    object: v8::Local<'s, v8::Object>,
    key: &str,
) -> u64 {
    let key = v8::String::new(scope, key).expect("V8 string key");
    object
        .get(scope, key.into())
        .and_then(|value| value.integer_value(scope))
        .and_then(|value| u64::try_from(value).ok())
        .unwrap_or_default()
}

fn process_memory_usage_value<'s>(scope: &mut v8::HandleScope<'s>) -> v8::Local<'s, v8::Value> {
    let mut stats = v8::HeapStatistics::default();
    scope.get_heap_statistics(&mut stats);

    let object = v8::Object::new(scope);
    set_object_number_property(scope, object, "rss", stats.total_physical_size() as f64);
    set_object_number_property(scope, object, "heapTotal", stats.total_heap_size() as f64);
    set_object_number_property(scope, object, "heapUsed", stats.used_heap_size() as f64);
    set_object_number_property(scope, object, "external", stats.external_memory() as f64);
    set_object_number_property(
        scope,
        object,
        "arrayBuffers",
        stats.external_memory() as f64,
    );
    object.into()
}

fn process_cpu_usage_value<'s>(
    scope: &mut v8::HandleScope<'s>,
    args: &v8::FunctionCallbackArguments,
) -> Result<v8::Local<'s, v8::Value>, String> {
    let usage = current_thread_resource_usage()?;
    let current_user = usage.user_cpu_us;
    let current_system = usage.system_cpu_us;

    let (user, system) = if args.length() > 0 {
        let prev = args.get(0);
        if prev.is_null_or_undefined() {
            (current_user, current_system)
        } else if let Some(prev) = prev.to_object(scope) {
            let previous_user = number_property_or_zero(scope, prev, "user");
            let previous_system = number_property_or_zero(scope, prev, "system");
            (
                current_user.saturating_sub(previous_user),
                current_system.saturating_sub(previous_system),
            )
        } else {
            (current_user, current_system)
        }
    } else {
        (current_user, current_system)
    };

    let object = v8::Object::new(scope);
    set_object_number_property(scope, object, "user", user as f64);
    set_object_number_property(scope, object, "system", system as f64);
    Ok(object.into())
}

fn process_resource_usage_value<'s>(
    scope: &mut v8::HandleScope<'s>,
) -> Result<v8::Local<'s, v8::Value>, String> {
    let usage = current_thread_resource_usage()?;
    let object = v8::Object::new(scope);
    set_object_number_property(scope, object, "userCPUTime", usage.user_cpu_us as f64);
    set_object_number_property(scope, object, "systemCPUTime", usage.system_cpu_us as f64);
    set_object_number_property(scope, object, "maxRSS", usage.max_rss_kib as f64);
    set_object_number_property(
        scope,
        object,
        "sharedMemorySize",
        usage.shared_memory_size as f64,
    );
    set_object_number_property(
        scope,
        object,
        "unsharedDataSize",
        usage.unshared_data_size as f64,
    );
    set_object_number_property(
        scope,
        object,
        "unsharedStackSize",
        usage.unshared_stack_size as f64,
    );
    set_object_number_property(
        scope,
        object,
        "minorPageFault",
        usage.minor_page_faults as f64,
    );
    set_object_number_property(
        scope,
        object,
        "majorPageFault",
        usage.major_page_faults as f64,
    );
    set_object_number_property(scope, object, "swappedOut", usage.swapped_out as f64);
    set_object_number_property(scope, object, "fsRead", usage.fs_read as f64);
    set_object_number_property(scope, object, "fsWrite", usage.fs_write as f64);
    set_object_number_property(scope, object, "ipcSent", usage.ipc_sent as f64);
    set_object_number_property(scope, object, "ipcReceived", usage.ipc_received as f64);
    set_object_number_property(scope, object, "signalsCount", usage.signals_count as f64);
    set_object_number_property(
        scope,
        object,
        "voluntaryContextSwitches",
        usage.voluntary_context_switches as f64,
    );
    set_object_number_property(
        scope,
        object,
        "involuntaryContextSwitches",
        usage.involuntary_context_switches as f64,
    );
    Ok(object.into())
}

fn process_versions_value<'s>(scope: &mut v8::HandleScope<'s>) -> v8::Local<'s, v8::Value> {
    let object = v8::Object::new(scope);
    set_object_string_property(scope, object, "v8", v8::V8::get_version());
    set_object_string_property(
        scope,
        object,
        "openssl",
        &normalize_openssl_version(openssl_version::version()),
    );
    object.into()
}

#[derive(Clone)]
struct VmContextState {
    context: v8::Global<v8::Context>,
    baseline_keys: HashSet<String>,
    mirrored_keys: HashSet<String>,
}

#[derive(Clone, Debug)]
struct VmRunOptions {
    filename: String,
    line_offset: i32,
    column_offset: i32,
    timeout_ms: Option<u32>,
}

impl Default for VmRunOptions {
    fn default() -> Self {
        Self {
            filename: String::from("evalmachine.<anonymous>"),
            line_offset: 0,
            column_offset: 0,
            timeout_ms: None,
        }
    }
}

thread_local! {
    static VM_CONTEXTS: RefCell<HashMap<u32, VmContextState>> = RefCell::new(HashMap::new());
    static NEXT_VM_CONTEXT_ID: Cell<u32> = const { Cell::new(1) };
}

fn next_vm_context_id() -> u32 {
    NEXT_VM_CONTEXT_ID.with(|next_id| {
        let id = next_id.get();
        let next = id.checked_add(1).unwrap_or(1);
        next_id.set(next.max(1));
        id
    })
}

fn vm_collect_object_keys<'s>(
    scope: &mut v8::HandleScope<'s>,
    object: v8::Local<'s, v8::Object>,
) -> HashSet<String> {
    let names = object
        .get_own_property_names(scope, v8::GetPropertyNamesArgs::default())
        .unwrap_or_else(|| v8::Array::new(scope, 0));
    let mut keys = HashSet::new();
    for index in 0..names.length() {
        let Some(name) = names.get_index(scope, index) else {
            continue;
        };
        if name.is_string() {
            keys.insert(name.to_rust_string_lossy(scope));
        }
    }
    keys
}

fn vm_set_property<'s>(
    scope: &mut v8::HandleScope<'s>,
    object: v8::Local<'s, v8::Object>,
    key: &str,
    value: v8::Local<'s, v8::Value>,
) {
    let Some(key_value) = v8::String::new(scope, key) else {
        return;
    };
    let _ = object.set(scope, key_value.into(), value);
}

fn vm_delete_property<'s>(
    scope: &mut v8::HandleScope<'s>,
    object: v8::Local<'s, v8::Object>,
    key: &str,
) {
    let Some(key_value) = v8::String::new(scope, key) else {
        return;
    };
    let _ = object.delete(scope, key_value.into());
}

fn vm_copy_sandbox_into_context<'s>(
    scope: &mut v8::HandleScope<'s>,
    sandbox: v8::Local<'s, v8::Object>,
    context_global: v8::Local<'s, v8::Object>,
    previous_mirrored_keys: &HashSet<String>,
) -> HashSet<String> {
    let current_keys = vm_collect_object_keys(scope, sandbox);
    for key in current_keys.iter() {
        let Some(key_value) = v8::String::new(scope, key) else {
            continue;
        };
        let value = sandbox
            .get(scope, key_value.into())
            .unwrap_or_else(|| v8::undefined(scope).into());
        vm_set_property(scope, context_global, key, value);
    }
    for key in previous_mirrored_keys {
        if !current_keys.contains(key) {
            vm_delete_property(scope, context_global, key);
        }
    }
    current_keys
}

fn vm_copy_context_into_sandbox<'s>(
    scope: &mut v8::HandleScope<'s>,
    context_global: v8::Local<'s, v8::Object>,
    sandbox: v8::Local<'s, v8::Object>,
    baseline_keys: &HashSet<String>,
    previous_mirrored_keys: &HashSet<String>,
) -> HashSet<String> {
    let current_keys = vm_collect_object_keys(scope, context_global)
        .into_iter()
        .filter(|key| !baseline_keys.contains(key))
        .collect::<HashSet<_>>();
    for key in current_keys.iter() {
        let Some(key_value) = v8::String::new(scope, key) else {
            continue;
        };
        let value = context_global
            .get(scope, key_value.into())
            .unwrap_or_else(|| v8::undefined(scope).into());
        vm_set_property(scope, sandbox, key, value);
    }
    for key in previous_mirrored_keys {
        if !current_keys.contains(key) {
            vm_delete_property(scope, sandbox, key);
        }
    }
    current_keys
}

fn vm_options_from_value<'s>(
    scope: &mut v8::HandleScope<'s>,
    value: v8::Local<'s, v8::Value>,
) -> VmRunOptions {
    if value.is_null_or_undefined() {
        return VmRunOptions::default();
    }
    if value.is_string() {
        return VmRunOptions {
            filename: value.to_rust_string_lossy(scope),
            ..VmRunOptions::default()
        };
    }
    let Some(options) = value.to_object(scope) else {
        return VmRunOptions::default();
    };
    let mut result = VmRunOptions::default();
    let read_string = |scope: &mut v8::HandleScope<'s>, key: &str| {
        let key_value = v8::String::new(scope, key).expect("V8 string key");
        options
            .get(scope, key_value.into())
            .filter(|value| value.is_string())
            .map(|value| value.to_rust_string_lossy(scope))
    };
    let read_i32 = |scope: &mut v8::HandleScope<'s>, key: &str| {
        let key_value = v8::String::new(scope, key).expect("V8 string key");
        options
            .get(scope, key_value.into())
            .and_then(|value| value.int32_value(scope))
    };
    let read_u32 = |scope: &mut v8::HandleScope<'s>, key: &str| {
        let key_value = v8::String::new(scope, key).expect("V8 string key");
        options
            .get(scope, key_value.into())
            .and_then(|value| value.integer_value(scope))
            .and_then(|value| u32::try_from(value).ok())
    };

    if let Some(filename) = read_string(scope, "filename") {
        result.filename = filename;
    }
    if let Some(line_offset) = read_i32(scope, "lineOffset") {
        result.line_offset = line_offset;
    }
    if let Some(column_offset) = read_i32(scope, "columnOffset") {
        result.column_offset = column_offset;
    }
    result.timeout_ms = read_u32(scope, "timeout").filter(|timeout_ms| *timeout_ms > 0);
    result
}

fn vm_throw_error<'s>(
    scope: &mut v8::HandleScope<'s>,
    message: &str,
    code: Option<&str>,
    type_error: bool,
) -> v8::Local<'s, v8::Value> {
    let message_value = v8::String::new(scope, message).expect("V8 error message");
    let exception = if type_error {
        v8::Exception::type_error(scope, message_value)
    } else {
        v8::Exception::error(scope, message_value)
    };
    if let Some(code) = code {
        if let Some(exception_object) = exception.to_object(scope) {
            let code_key = v8::String::new(scope, "code").expect("V8 code key");
            let code_value = v8::String::new(scope, code).expect("V8 code value");
            let _ = exception_object.set(scope, code_key.into(), code_value.into());
        }
    }
    scope.throw_exception(exception);
    exception
}

fn vm_throw_execution_error<'s>(
    scope: &mut v8::HandleScope<'s>,
    error: &crate::ipc::ExecutionError,
) -> v8::Local<'s, v8::Value> {
    let message_value = v8::String::new(scope, &error.message).expect("V8 error message");
    let exception = match error.error_type.as_str() {
        "TypeError" => v8::Exception::type_error(scope, message_value),
        _ => v8::Exception::error(scope, message_value),
    };
    if let Some(exception_object) = exception.to_object(scope) {
        if let Some(code) = error.code.as_deref() {
            let code_key = v8::String::new(scope, "code").expect("V8 code key");
            let code_value = v8::String::new(scope, code).expect("V8 code value");
            let _ = exception_object.set(scope, code_key.into(), code_value.into());
        }
        if !error.stack.is_empty() {
            let stack_key = v8::String::new(scope, "stack").expect("V8 stack key");
            let stack_value = v8::String::new(scope, &error.stack).expect("V8 stack value");
            let _ = exception_object.set(scope, stack_key.into(), stack_value.into());
        }
    }
    scope.throw_exception(exception);
    exception
}

fn vm_apply_script_origin_to_error(
    mut error: crate::ipc::ExecutionError,
    options: &VmRunOptions,
) -> crate::ipc::ExecutionError {
    let display_line = options.line_offset.saturating_add(1).max(1);
    let display_column = options.column_offset.saturating_add(1).max(1);
    let marker = format!("{}:{}", options.filename, display_line);
    if !error.stack.contains(&marker) {
        error.stack = format!(
            "{}: {}\n    at {}:{}:{}",
            error.error_type, error.message, options.filename, display_line, display_column
        );
    }
    error
}

fn vm_run_script_in_context<'s>(
    scope: &mut v8::HandleScope<'s>,
    isolate_handle: v8::IsolateHandle,
    context: v8::Local<'s, v8::Context>,
    code: &str,
    options: &VmRunOptions,
) -> Result<v8::Local<'s, v8::Value>, String> {
    let mut timeout_guard = options.timeout_ms.map(|timeout_ms| {
        let (abort_tx, _abort_rx) = crossbeam_channel::bounded::<()>(0);
        crate::timeout::TimeoutGuard::new(timeout_ms, isolate_handle.clone(), abort_tx)
    });

    let mut result = None;
    let mut exception = None;
    {
        let context_scope = &mut v8::ContextScope::new(scope, context);
        let tc = &mut v8::TryCatch::new(context_scope);
        let source = v8::String::new(tc, code)
            .ok_or_else(|| String::from("vm source string too large for V8"))?;
        let filename = v8::String::new(tc, &options.filename)
            .ok_or_else(|| String::from("vm filename too large for V8"))?;
        let origin = v8::ScriptOrigin::new(
            tc,
            filename.into(),
            options.line_offset.saturating_sub(1),
            options.column_offset,
            false,
            -1,
            None,
            false,
            false,
            false,
            None,
        );
        match v8::Script::compile(tc, source, Some(&origin)) {
            Some(script) => match script.run(tc) {
                Some(value) => {
                    tc.perform_microtask_checkpoint();
                    if let Some(thrown) = tc.exception() {
                        exception = Some(vm_apply_script_origin_to_error(
                            crate::execution::extract_error_info(tc, thrown),
                            options,
                        ));
                    } else {
                        result = Some(v8::Global::new(tc, value));
                    }
                }
                None => {
                    let failure_message = v8::String::new(tc, "vm script execution failed")
                        .expect("vm failure message");
                    let thrown = tc
                        .exception()
                        .unwrap_or_else(|| v8::Exception::error(tc, failure_message).into());
                    exception = Some(vm_apply_script_origin_to_error(
                        crate::execution::extract_error_info(tc, thrown),
                        options,
                    ));
                }
            },
            None => {
                let failure_message = v8::String::new(tc, "vm script compilation failed")
                    .expect("vm failure message");
                let thrown = tc
                    .exception()
                    .unwrap_or_else(|| v8::Exception::error(tc, failure_message).into());
                exception = Some(vm_apply_script_origin_to_error(
                    crate::execution::extract_error_info(tc, thrown),
                    options,
                ));
            }
        }
    }

    let timed_out = if let Some(ref mut guard) = timeout_guard {
        guard.cancel();
        guard.timed_out()
    } else {
        false
    };

    if timed_out {
        isolate_handle.cancel_terminate_execution();
        return Ok(vm_throw_error(
            scope,
            &format!(
                "Script execution timed out after {}ms",
                options.timeout_ms.unwrap_or_default()
            ),
            Some("ERR_SCRIPT_EXECUTION_TIMEOUT"),
            false,
        ));
    }

    if let Some(exception) = exception {
        return Ok(vm_throw_execution_error(scope, &exception));
    }

    Ok(result
        .map(|result| v8::Local::new(scope, &result))
        .unwrap_or_else(|| v8::undefined(scope).into()))
}

fn vm_create_context_value<'s>(
    scope: &mut v8::HandleScope<'s>,
    args: &mut v8::FunctionCallbackArguments<'s>,
) -> Result<v8::Local<'s, v8::Value>, String> {
    let sandbox_value = args.get(0);
    if !(sandbox_value.is_object() || sandbox_value.is_function()) {
        return Ok(vm_throw_error(
            scope,
            "The \"object\" argument must be of type object.",
            None,
            true,
        ));
    }
    let sandbox = sandbox_value
        .to_object(scope)
        .ok_or_else(|| String::from("vm.createContext expected an object sandbox"))?;
    let context = v8::Context::new(scope, Default::default());
    {
        let context_scope = &mut v8::ContextScope::new(scope, context);
        let global = context.global(context_scope);
        for key in [
            "Buffer",
            "require",
            "process",
            "module",
            "exports",
            "__dirname",
            "__filename",
        ] {
            vm_delete_property(context_scope, global, key);
            let undefined = v8::undefined(context_scope).into();
            vm_set_property(context_scope, global, key, undefined);
        }
    }
    let baseline_keys = {
        let context_scope = &mut v8::ContextScope::new(scope, context);
        let global = context.global(context_scope);
        vm_collect_object_keys(context_scope, global)
    };
    let mirrored_keys = {
        let context_scope = &mut v8::ContextScope::new(scope, context);
        let global = context.global(context_scope);
        vm_copy_sandbox_into_context(context_scope, sandbox, global, &HashSet::new())
    };

    let context_id = next_vm_context_id();
    VM_CONTEXTS.with(|contexts| {
        contexts.borrow_mut().insert(
            context_id,
            VmContextState {
                context: v8::Global::new(scope, context),
                baseline_keys,
                mirrored_keys,
            },
        );
    });
    Ok(v8::Integer::new_from_unsigned(scope, context_id).into())
}

fn vm_run_in_context_value<'s>(
    scope: &mut v8::HandleScope<'s>,
    args: &mut v8::FunctionCallbackArguments<'s>,
) -> Result<v8::Local<'s, v8::Value>, String> {
    let context_id = args
        .get(0)
        .uint32_value(scope)
        .ok_or_else(|| String::from("vm.runInContext missing context id"))?;
    let code = args.get(1).to_rust_string_lossy(scope);
    let options_value = args.get(2);
    let options = vm_options_from_value(scope, options_value);
    let sandbox = args
        .get(3)
        .to_object(scope)
        .ok_or_else(|| String::from("vm.runInContext missing sandbox object"))?;
    let isolate_handle = unsafe { args.get_isolate() }.thread_safe_handle();

    let Some((context_global, baseline_keys, mirrored_keys)) = VM_CONTEXTS.with(|contexts| {
        contexts.borrow().get(&context_id).map(|state| {
            (
                state.context.clone(),
                state.baseline_keys.clone(),
                state.mirrored_keys.clone(),
            )
        })
    }) else {
        return Ok(vm_throw_error(
            scope,
            "The \"contextifiedObject\" argument must be a vm context.",
            Some("ERR_INVALID_ARG_TYPE"),
            true,
        ));
    };

    let context = v8::Local::new(scope, &context_global);
    {
        let context_scope = &mut v8::ContextScope::new(scope, context);
        let global = context.global(context_scope);
        vm_copy_sandbox_into_context(context_scope, sandbox, global, &mirrored_keys);
    }
    let result = vm_run_script_in_context(scope, isolate_handle, context, &code, &options)?;
    let updated_keys = {
        let context_scope = &mut v8::ContextScope::new(scope, context);
        let global = context.global(context_scope);
        vm_copy_context_into_sandbox(
            context_scope,
            global,
            sandbox,
            &baseline_keys,
            &mirrored_keys,
        )
    };
    VM_CONTEXTS.with(|contexts| {
        if let Some(state) = contexts.borrow_mut().get_mut(&context_id) {
            state.mirrored_keys = updated_keys;
        }
    });
    Ok(result)
}

fn vm_run_in_this_context_value<'s>(
    scope: &mut v8::HandleScope<'s>,
    args: &mut v8::FunctionCallbackArguments<'s>,
) -> Result<v8::Local<'s, v8::Value>, String> {
    let code = args.get(0).to_rust_string_lossy(scope);
    let options_value = args.get(1);
    let options = vm_options_from_value(scope, options_value);
    let context = scope.get_current_context();
    let isolate_handle = unsafe { args.get_isolate() }.thread_safe_handle();
    vm_run_script_in_context(scope, isolate_handle, context, &code, &options)
}

fn handle_local_bridge_call<'s>(
    scope: &mut v8::HandleScope<'s>,
    method: &str,
    args: &mut v8::FunctionCallbackArguments<'s>,
) -> Result<Option<v8::Local<'s, v8::Value>>, String> {
    match method {
        "process.memoryUsage" => Ok(Some(process_memory_usage_value(scope))),
        "process.cpuUsage" => process_cpu_usage_value(scope, args).map(Some),
        "process.resourceUsage" => process_resource_usage_value(scope).map(Some),
        "process.versions" => Ok(Some(process_versions_value(scope))),
        "_vmCreateContext" => vm_create_context_value(scope, args).map(Some),
        "_vmRunInContext" => vm_run_in_context_value(scope, args).map(Some),
        "_vmRunInThisContext" => vm_run_in_this_context_value(scope, args).map(Some),
        _ => Ok(None),
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
        attach_bridge_function_aliases(scope, func, &["applySync", "applySyncPromise"]);

        let key = v8::String::new(scope, method_name).unwrap();
        global.set(scope, key.into(), func.into());
    }

    BridgeFnStore { _data: data }
}

/// V8 FunctionTemplate callback for sync-blocking bridge calls.
fn sync_bridge_callback<'s>(
    scope: &mut v8::HandleScope<'s>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue,
) {
    let mut args = args;
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

    {
        let tc = &mut v8::TryCatch::new(scope);
        match handle_local_bridge_call(tc, &data.method, &mut args) {
            Ok(Some(value)) => {
                if tc.has_caught() {
                    let _ = tc.rethrow();
                    return;
                }
                rv.set(value);
                return;
            }
            Ok(None) => {}
            Err(err) => {
                if tc.has_caught() {
                    let _ = tc.rethrow();
                    return;
                }
                let msg = v8::String::new(tc, &format!("bridge runtime error: {err}")).unwrap();
                let exc = v8::Exception::error(tc, msg);
                tc.throw_exception(exc);
                return;
            }
        }
    }

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
            if let Some(code) = bridge_error_code(&err_msg) {
                let exc_object = exc.to_object(scope).unwrap();
                let code_key = v8::String::new(scope, "code").unwrap();
                let code_value = v8::String::new(scope, code).unwrap();
                let _ = exc_object.set(scope, code_key.into(), code_value.into());
            }
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
        attach_bridge_function_aliases(scope, func, &["apply"]);

        let key = v8::String::new(scope, method_name).unwrap();
        global.set(scope, key.into(), func.into());
    }

    AsyncBridgeFnStore { _data: data }
}

fn attach_bridge_function_aliases<'s>(
    scope: &mut v8::HandleScope<'s>,
    func: v8::Local<'s, v8::Function>,
    aliases: &[&str],
) {
    let func_object = func.to_object(scope).unwrap();
    for alias in aliases {
        let key = v8::String::new(scope, alias).unwrap();
        let Some(wrapper) = build_bridge_apply_wrapper(scope, func) else {
            continue;
        };
        let _ = func_object.set(scope, key.into(), wrapper.into());
    }
}

fn build_bridge_apply_wrapper<'s>(
    scope: &mut v8::HandleScope<'s>,
    func: v8::Local<'s, v8::Function>,
) -> Option<v8::Local<'s, v8::Function>> {
    let source = v8::String::new(
        scope,
        "(function (fn) { return function (_thisArg, args) { return fn(...(Array.isArray(args) ? args : [])); }; })",
    )?;
    let script = v8::Script::compile(scope, source, None)?;
    let factory = script.run(scope)?;
    let factory = v8::Local::<v8::Function>::try_from(factory).ok()?;
    let argv = [func.into()];
    let receiver = v8::undefined(scope).into();
    factory
        .call(scope, receiver, &argv)
        .and_then(|value| v8::Local::<v8::Function>::try_from(value).ok())
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
        if let Some(code) = bridge_error_code(&err_msg) {
            let exc_object = exc.to_object(scope).unwrap();
            let code_key = v8::String::new(scope, "code").unwrap();
            let code_value = v8::String::new(scope, code).unwrap();
            let _ = exc_object.set(scope, code_key.into(), code_value.into());
        }
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

fn bridge_error_code(message: &str) -> Option<&str> {
    const TRUSTED_PREFIXES: &[&str] = &[
        "ERR_AGENT_OS_NODE_SYNC_RPC",
        "ERR_AGENT_OS_PYTHON_VFS_RPC",
        "ERR_AGENT_OS_BRIDGE",
    ];

    let mut segments = message.split(':').map(str::trim);
    let first = segments.next()?;
    if is_errno_segment(first) {
        return Some(first);
    }

    if TRUSTED_PREFIXES.contains(&first) {
        let second = segments.next()?;
        if is_errno_segment(second) {
            return Some(second);
        }
    }

    None
}

fn is_errno_segment(segment: &str) -> bool {
    segment.len() >= 2
        && segment.starts_with('E')
        && !segment.starts_with("ERR_")
        && segment[1..]
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

#[cfg(test)]
mod tests {
    use super::bridge_error_code;

    #[test]
    fn bridge_error_code_rejects_guest_controlled_errno_segments() {
        assert_eq!(bridge_error_code("user said 'EACCES: denied'"), None);
        assert_eq!(
            bridge_error_code("prefix: user said 'EPERM': more text"),
            None
        );
        assert_eq!(bridge_error_code("ERR_AGENT_OS_FAKE: EACCES: denied"), None);
    }

    #[test]
    fn bridge_error_code_accepts_trusted_agent_os_prefixes() {
        assert_eq!(
            bridge_error_code("ERR_AGENT_OS_NODE_SYNC_RPC: EACCES: permission denied on /foo"),
            Some("EACCES")
        );
        assert_eq!(
            bridge_error_code("ERR_AGENT_OS_PYTHON_VFS_RPC: ENOENT: missing file"),
            Some("ENOENT")
        );
        assert_eq!(bridge_error_code("EEXIST: already exists"), Some("EEXIST"));
    }
}
