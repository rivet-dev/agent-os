// Host function injection via v8::FunctionTemplate

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::OnceLock;

use agentos_bridge::bridge_contract;
use serde::de;
use v8::MapFnTo;
use v8::ValueDeserializerHelper;
use v8::ValueSerializerHelper;

use crate::host_call::BridgeCallContext;

// CBOR codec flag: when true, use CBOR (via ciborium) instead of V8
// ValueSerializer/ValueDeserializer for IPC payloads. Activated by
// AGENTOS_V8_CODEC=cbor for runtimes whose node:v8 module doesn't
// produce real V8 serialization format (e.g. Bun).
static USE_CBOR_CODEC: AtomicBool = AtomicBool::new(false);
static EMBEDDED_CBOR_USERS: AtomicUsize = AtomicUsize::new(0);
const MAX_CBOR_BRIDGE_DEPTH: usize = 64;
const MAX_CBOR_BRIDGE_CONTAINER_ITEMS: usize = 100_000;
const MAX_VM_CONTEXTS: usize = 1024;
pub(crate) const MAX_PENDING_PROMISES: usize = 1024;
const DEFAULT_BRIDGE_RESPONSE_MAX_BYTES: usize = 256 * 1024;
const READ_RESPONSE_ENVELOPE_BYTES: usize = 4 * 1024;

/// Initialize the codec from the AGENTOS_V8_CODEC environment variable.
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
    std::env::var("AGENTOS_V8_CODEC")
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
fn v8_to_cbor(
    scope: &mut v8::HandleScope,
    value: v8::Local<v8::Value>,
) -> Result<ciborium::Value, String> {
    let mut object_stack = Vec::new();
    v8_to_cbor_inner(scope, value, 0, &mut object_stack)
}

fn v8_to_cbor_inner(
    scope: &mut v8::HandleScope,
    value: v8::Local<v8::Value>,
    depth: usize,
    object_stack: &mut Vec<v8::Global<v8::Object>>,
) -> Result<ciborium::Value, String> {
    if depth > MAX_CBOR_BRIDGE_DEPTH {
        return Err(format!(
            "CBOR encode depth exceeds limit of {MAX_CBOR_BRIDGE_DEPTH}"
        ));
    }

    if value.is_null_or_undefined() {
        return Ok(ciborium::Value::Null);
    }
    if value.is_boolean() {
        return Ok(ciborium::Value::Bool(value.boolean_value(scope)));
    }
    if value.is_int32() {
        return Ok(ciborium::Value::Integer(
            value.int32_value(scope).unwrap_or(0).into(),
        ));
    }
    if value.is_number() {
        return Ok(ciborium::Value::Float(
            value.number_value(scope).unwrap_or(0.0),
        ));
    }
    if value.is_string() {
        let s = value.to_rust_string_lossy(scope);
        return Ok(ciborium::Value::Text(s));
    }
    if value.is_array_buffer_view() {
        let view = v8::Local::<v8::ArrayBufferView>::try_from(value).unwrap();
        let len = view.byte_length();
        let mut buf = vec![0u8; len];
        view.copy_contents(&mut buf);
        return Ok(ciborium::Value::Bytes(buf));
    }
    if value.is_array() {
        let obj = value
            .to_object(scope)
            .ok_or_else(|| "CBOR encode failed to convert array to object".to_string())?;
        enter_cbor_object(scope, object_stack, obj)?;
        let arr = v8::Local::<v8::Array>::try_from(value).unwrap();
        let len = arr.length();
        let item_count = cbor_container_item_count("array", len as usize)?;
        let mut items = Vec::with_capacity(item_count);
        let result = (|| {
            for i in 0..len {
                if let Some(elem) = arr.get_index(scope, i) {
                    items.push(v8_to_cbor_inner(scope, elem, depth + 1, object_stack)?);
                } else {
                    items.push(ciborium::Value::Null);
                }
            }
            Ok(ciborium::Value::Array(items))
        })();
        object_stack.pop();
        return result;
    }
    if value.is_object() {
        let obj = value.to_object(scope).unwrap();
        enter_cbor_object(scope, object_stack, obj)?;
        let names = obj
            .get_own_property_names(scope, v8::GetPropertyNamesArgs::default())
            .unwrap_or_else(|| v8::Array::new(scope, 0));
        let len = names.length();
        let item_count = cbor_container_item_count("object", len as usize)?;
        let mut entries = Vec::with_capacity(item_count);
        let result = (|| {
            for i in 0..len {
                let key = names.get_index(scope, i).unwrap();
                let key_str = key.to_rust_string_lossy(scope);
                let val = obj
                    .get(scope, key)
                    .unwrap_or_else(|| v8::undefined(scope).into());
                entries.push((
                    ciborium::Value::Text(key_str),
                    v8_to_cbor_inner(scope, val, depth + 1, object_stack)?,
                ));
            }
            Ok(ciborium::Value::Map(entries))
        })();
        object_stack.pop();
        return result;
    }
    Ok(ciborium::Value::Null)
}

fn enter_cbor_object(
    scope: &mut v8::HandleScope,
    object_stack: &mut Vec<v8::Global<v8::Object>>,
    object: v8::Local<v8::Object>,
) -> Result<(), String> {
    for previous in object_stack.iter() {
        let previous = v8::Local::new(scope, previous);
        if previous.strict_equals(object.into()) {
            return Err("CBOR encode rejected circular object graph".to_string());
        }
    }
    object_stack.push(v8::Global::new(scope, object));
    Ok(())
}

fn cbor_container_item_count(kind: &str, item_count: usize) -> Result<usize, String> {
    if item_count > MAX_CBOR_BRIDGE_CONTAINER_ITEMS {
        return Err(format!(
            "CBOR {kind} item count {item_count} exceeds limit of {MAX_CBOR_BRIDGE_CONTAINER_ITEMS}"
        ));
    }
    Ok(item_count)
}

struct LimitedCborValue(ciborium::Value);

impl<'de> de::Deserialize<'de> for LimitedCborValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        deserializer.deserialize_any(LimitedCborVisitor).map(Self)
    }
}

struct LimitedCborSeed;

impl<'de> de::DeserializeSeed<'de> for LimitedCborSeed {
    type Value = ciborium::Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        deserializer.deserialize_any(LimitedCborVisitor)
    }
}

struct LimitedCborVisitor;

impl<'de> de::Visitor<'de> for LimitedCborVisitor {
    type Value = ciborium::Value;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a bounded CBOR bridge value")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(ciborium::Value::Bool(value))
    }

    fn visit_f32<E>(self, value: f32) -> Result<Self::Value, E> {
        Ok(ciborium::Value::Float(value.into()))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E> {
        Ok(ciborium::Value::Float(value))
    }

    fn visit_i8<E>(self, value: i8) -> Result<Self::Value, E> {
        Ok(value.into())
    }

    fn visit_i16<E>(self, value: i16) -> Result<Self::Value, E> {
        Ok(value.into())
    }

    fn visit_i32<E>(self, value: i32) -> Result<Self::Value, E> {
        Ok(value.into())
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(value.into())
    }

    fn visit_i128<E>(self, value: i128) -> Result<Self::Value, E> {
        Ok(value.into())
    }

    fn visit_u8<E>(self, value: u8) -> Result<Self::Value, E> {
        Ok(value.into())
    }

    fn visit_u16<E>(self, value: u16) -> Result<Self::Value, E> {
        Ok(value.into())
    }

    fn visit_u32<E>(self, value: u32) -> Result<Self::Value, E> {
        Ok(value.into())
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(value.into())
    }

    fn visit_u128<E>(self, value: u128) -> Result<Self::Value, E> {
        Ok(value.into())
    }

    fn visit_char<E>(self, value: char) -> Result<Self::Value, E> {
        Ok(value.into())
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(value.into())
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(value.into())
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(value.into())
    }

    fn visit_bytes<E>(self, value: &[u8]) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(value.into())
    }

    fn visit_borrowed_bytes<E>(self, value: &'de [u8]) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(value.into())
    }

    fn visit_byte_buf<E>(self, value: Vec<u8>) -> Result<Self::Value, E> {
        Ok(value.into())
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(ciborium::Value::Null)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        deserializer.deserialize_any(self)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(ciborium::Value::Null)
    }

    fn visit_newtype_struct<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        deserializer.deserialize_any(self)
    }

    fn visit_seq<A>(self, mut access: A) -> Result<Self::Value, A::Error>
    where
        A: de::SeqAccess<'de>,
    {
        if let Some(item_count) = access.size_hint() {
            limited_cbor_item_count("array", item_count)?;
        }

        let mut items = Vec::new();
        while let Some(item) = access.next_element_seed(LimitedCborSeed)? {
            limited_cbor_item_count("array", items.len() + 1)?;
            items.push(item);
        }
        Ok(ciborium::Value::Array(items))
    }

    fn visit_map<A>(self, mut access: A) -> Result<Self::Value, A::Error>
    where
        A: de::MapAccess<'de>,
    {
        if let Some(item_count) = access.size_hint() {
            limited_cbor_item_count("map", item_count)?;
        }

        let mut entries = Vec::new();
        while let Some(key) = access.next_key_seed(LimitedCborSeed)? {
            limited_cbor_item_count("map", entries.len() + 1)?;
            let value = access.next_value_seed(LimitedCborSeed)?;
            entries.push((key, value));
        }
        Ok(ciborium::Value::Map(entries))
    }

    fn visit_enum<A>(self, access: A) -> Result<Self::Value, A::Error>
    where
        A: de::EnumAccess<'de>,
    {
        use serde::de::VariantAccess;

        struct TaggedValueVisitor;

        impl<'de> de::Visitor<'de> for TaggedValueVisitor {
            type Value = ciborium::Value;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a tagged CBOR bridge value")
            }

            fn visit_seq<A>(self, mut access: A) -> Result<Self::Value, A::Error>
            where
                A: de::SeqAccess<'de>,
            {
                let tag = access
                    .next_element()?
                    .ok_or_else(|| de::Error::custom("expected tag"))?;
                let value = access
                    .next_element_seed(LimitedCborSeed)?
                    .ok_or_else(|| de::Error::custom("expected tagged value"))?;
                Ok(ciborium::Value::Tag(tag, Box::new(value)))
            }
        }

        let (name, data): (String, _) = access.variant()?;
        if name != "@@TAGGED@@" {
            return Err(de::Error::custom("expected CBOR tag"));
        }
        data.tuple_variant(2, TaggedValueVisitor)
    }
}

fn limited_cbor_item_count<E: de::Error>(kind: &str, item_count: usize) -> Result<usize, E> {
    cbor_container_item_count(kind, item_count).map_err(de::Error::custom)
}

/// Convert a ciborium::Value to a V8 value.
fn cbor_to_v8<'s>(
    scope: &mut v8::HandleScope<'s>,
    value: &ciborium::Value,
) -> Result<v8::Local<'s, v8::Value>, String> {
    cbor_to_v8_inner(scope, value, 0)
}

fn cbor_to_v8_inner<'s>(
    scope: &mut v8::HandleScope<'s>,
    value: &ciborium::Value,
    depth: usize,
) -> Result<v8::Local<'s, v8::Value>, String> {
    if depth > MAX_CBOR_BRIDGE_DEPTH {
        return Err(format!(
            "CBOR decode depth exceeds limit of {MAX_CBOR_BRIDGE_DEPTH}"
        ));
    }

    match value {
        ciborium::Value::Null => Ok(v8::null(scope).into()),
        ciborium::Value::Bool(b) => Ok(v8::Boolean::new(scope, *b).into()),
        ciborium::Value::Integer(n) => {
            let n: i128 = (*n).into();
            if n >= i32::MIN as i128 && n <= i32::MAX as i128 {
                Ok(v8::Integer::new(scope, n as i32).into())
            } else {
                Ok(v8::Number::new(scope, n as f64).into())
            }
        }
        ciborium::Value::Float(f) => Ok(v8::Number::new(scope, *f).into()),
        ciborium::Value::Text(s) => Ok(v8::String::new(scope, s)
            .ok_or_else(|| "CBOR decode failed to allocate string".to_string())?
            .into()),
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
            Ok(v8::Uint8Array::new(scope, ab, 0, len)
                .ok_or_else(|| "CBOR decode failed to allocate byte array".to_string())?
                .into())
        }
        ciborium::Value::Array(items) => {
            cbor_container_item_count("array", items.len())?;
            let arr = v8::Array::new(scope, items.len() as i32);
            for (i, item) in items.iter().enumerate() {
                let val = cbor_to_v8_inner(scope, item, depth + 1)?;
                arr.set_index(scope, i as u32, val);
            }
            Ok(arr.into())
        }
        ciborium::Value::Map(entries) => {
            cbor_container_item_count("map", entries.len())?;
            let obj = v8::Object::new(scope);
            for (k, v) in entries {
                let key = cbor_to_v8_inner(scope, k, depth + 1)?;
                let val = cbor_to_v8_inner(scope, v, depth + 1)?;
                obj.set(scope, key, val);
            }
            Ok(obj.into())
        }
        ciborium::Value::Tag(_, inner) => cbor_to_v8_inner(scope, inner, depth + 1),
        _ => Ok(v8::undefined(scope).into()),
    }
}

fn raw_bytes_to_uint8array<'s>(
    scope: &mut v8::HandleScope<'s>,
    bytes: &[u8],
) -> Option<v8::Local<'s, v8::Value>> {
    let len = bytes.len();
    let ab = v8::ArrayBuffer::new(scope, len);
    if len > 0 {
        let bs = ab.get_backing_store();
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), bs.data()?.as_ptr() as *mut u8, len);
        }
    }
    v8::Uint8Array::new(scope, ab, 0, len).map(Into::into)
}

fn bridge_response_payload_to_v8<'s>(
    scope: &mut v8::HandleScope<'s>,
    status: u8,
    payload: &[u8],
) -> Option<v8::Local<'s, v8::Value>> {
    if status == 2 {
        return raw_bytes_to_uint8array(scope, payload);
    }

    let v8_val = {
        let tc = &mut v8::TryCatch::new(scope);
        deserialize_v8_value(tc, payload).ok()
    };
    if v8_val.is_some() {
        return v8_val;
    }

    // Preserve the historical compatibility fallback for malformed/non-native
    // status=0 responses, but never let status=2 raw bytes take this path.
    raw_bytes_to_uint8array(scope, payload)
}

/// Serialize a V8 value to CBOR bytes.
pub fn serialize_cbor_value(
    scope: &mut v8::HandleScope,
    value: v8::Local<v8::Value>,
) -> Result<Vec<u8>, String> {
    let cbor_val = v8_to_cbor(scope, value)?;
    let mut buf = Vec::new();
    ciborium::into_writer(&cbor_val, &mut buf).map_err(|e| format!("CBOR encode failed: {}", e))?;
    Ok(buf)
}

/// Deserialize CBOR bytes to a V8 value.
pub fn deserialize_cbor_value<'s>(
    scope: &mut v8::HandleScope<'s>,
    data: &[u8],
) -> Result<v8::Local<'s, v8::Value>, String> {
    let LimitedCborValue(cbor_val) =
        ciborium::de::from_reader_with_recursion_limit(data, MAX_CBOR_BRIDGE_DEPTH)
            .map_err(|e| format!("CBOR decode failed: {}", e))?;
    cbor_to_v8(scope, &cbor_val)
}

/// Data attached to each sync bridge function via v8::External.
/// BridgeFnStore keeps these heap allocations alive for the session.
struct SyncBridgeFnData {
    ctx: *const BridgeCallContext,
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
    reserved: Cell<usize>,
}

impl PendingPromises {
    pub fn new() -> Self {
        PendingPromises {
            map: RefCell::new(HashMap::new()),
            reserved: Cell::new(0),
        }
    }

    fn capacity_error(&self) -> Option<String> {
        let len = self.map.borrow().len().saturating_add(self.reserved.get());
        if len >= MAX_PENDING_PROMISES {
            return Some(format!(
                "async bridge pending promise registry exceeded limit of {MAX_PENDING_PROMISES} promises"
            ));
        }
        None
    }

    fn reserve(&self) -> Result<PendingPromiseReservation<'_>, String> {
        if let Some(error) = self.capacity_error() {
            return Err(error);
        }
        self.reserved.set(self.reserved.get().saturating_add(1));
        Ok(PendingPromiseReservation {
            pending: self,
            active: true,
        })
    }

    fn release_reservation(&self) {
        self.reserved.set(self.reserved.get().saturating_sub(1));
    }

    fn insert_reserved(
        &self,
        call_id: u64,
        resolver: v8::Global<v8::PromiseResolver>,
        mut reservation: PendingPromiseReservation<'_>,
    ) {
        self.map.borrow_mut().insert(call_id, resolver);
        reservation.active = false;
        self.release_reservation();
    }

    /// Remove and return the resolver for a given call_id.
    pub fn remove(&self, call_id: u64) -> Option<v8::Global<v8::PromiseResolver>> {
        self.map.borrow_mut().remove(&call_id)
    }

    /// Number of pending promises.
    pub fn len(&self) -> usize {
        self.map.borrow().len()
    }

    /// Whether there are no pending promises.
    pub fn is_empty(&self) -> bool {
        self.map.borrow().is_empty()
    }
}

impl Default for PendingPromises {
    fn default() -> Self {
        Self::new()
    }
}

struct PendingPromiseReservation<'a> {
    pending: &'a PendingPromises,
    active: bool,
}

impl Drop for PendingPromiseReservation<'_> {
    fn drop(&mut self) {
        if self.active {
            self.pending.release_reservation();
        }
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

// Used only by the non-macOS `getrusage(RUSAGE_THREAD)` path; macOS reads CPU
// time from Mach `time_value_t` instead.
#[cfg(not(target_os = "macos"))]
fn timeval_to_micros(value: libc::timeval) -> u64 {
    let seconds = i128::from(value.tv_sec).max(0);
    let micros = i128::from(value.tv_usec).max(0);
    (seconds
        .saturating_mul(1_000_000)
        .saturating_add(micros)
        .min(i128::from(u64::MAX))) as u64
}

#[cfg(not(target_os = "macos"))]
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

// macOS has no `RUSAGE_THREAD`, so per-thread CPU time comes from the Mach
// `thread_info(THREAD_BASIC_INFO)` call. The remaining rusage fields have no
// per-thread source on Apple platforms, so they are filled best-effort from the
// process-wide `getrusage(RUSAGE_SELF)` (mirroring libuv's macOS behaviour).
#[cfg(target_os = "macos")]
fn macos_thread_cpu_micros() -> Result<(u64, u64), String> {
    // SAFETY: `pthread_mach_thread_np` yields the calling thread's Mach port;
    // `thread_info` fully initialises `info` on KERN_SUCCESS.
    unsafe {
        let port = libc::pthread_mach_thread_np(libc::pthread_self());
        if port == 0 {
            return Err("pthread_mach_thread_np returned MACH_PORT_NULL".to_string());
        }
        let mut info = MaybeUninit::<libc::thread_basic_info>::zeroed();
        let mut count = (std::mem::size_of::<libc::thread_basic_info>()
            / std::mem::size_of::<libc::integer_t>())
            as libc::mach_msg_type_number_t;
        let rc = libc::thread_info(
            port,
            libc::THREAD_BASIC_INFO as libc::thread_flavor_t,
            info.as_mut_ptr() as libc::thread_info_t,
            &mut count,
        );
        if rc != libc::KERN_SUCCESS {
            return Err(format!("thread_info(THREAD_BASIC_INFO) failed: {rc}"));
        }
        let info = info.assume_init();
        let to_micros = |t: libc::time_value_t| -> u64 {
            let secs = i128::from(t.seconds).max(0);
            let micros = i128::from(t.microseconds).max(0);
            (secs
                .saturating_mul(1_000_000)
                .saturating_add(micros)
                .min(i128::from(u64::MAX))) as u64
        };
        Ok((to_micros(info.user_time), to_micros(info.system_time)))
    }
}

#[cfg(target_os = "macos")]
fn current_thread_resource_usage() -> Result<ThreadResourceUsageSnapshot, String> {
    let (user_cpu_us, system_cpu_us) = macos_thread_cpu_micros()?;

    let mut usage = MaybeUninit::<libc::rusage>::uninit();
    let result = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    if result != 0 {
        return Err(format!(
            "getrusage(RUSAGE_SELF) failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let usage = unsafe { usage.assume_init() };
    Ok(ThreadResourceUsageSnapshot {
        // Per-thread CPU time (accurate, from Mach thread_info).
        user_cpu_us,
        system_cpu_us,
        // macOS reports ru_maxrss in bytes; normalise to KiB to match Linux.
        max_rss_kib: non_negative_c_long(usage.ru_maxrss) / 1024,
        // Process-wide best-effort: no per-thread source on macOS.
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

// Guest crypto is served by pure-Rust crates (RustCrypto), not OpenSSL, so there
// is no live OpenSSL library to query. We still surface a `process.versions.openssl`
// string for guest compatibility, pinned to the OpenSSL release vendored by the
// sidecar (openssl-sys 300.6.0+3.6.2). The browser executor reports the same
// constant so both runtimes present an identical identity.
pub const EMULATED_OPENSSL_VERSION: &str = "3.6.3";

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
    set_object_string_property(scope, object, "openssl", EMULATED_OPENSSL_VERSION);
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

fn vm_context_capacity_error(current_contexts: usize) -> Option<String> {
    if current_contexts >= MAX_VM_CONTEXTS {
        return Some(format!(
            "node:vm context registry exceeded limit of {MAX_VM_CONTEXTS} contexts"
        ));
    }
    None
}

fn reserve_vm_context_slot<'s>(
    scope: &mut v8::HandleScope<'s>,
    context: v8::Local<'s, v8::Context>,
) -> Result<u32, String> {
    VM_CONTEXTS.with(|contexts| {
        let mut contexts = contexts.borrow_mut();
        if let Some(error) = vm_context_capacity_error(contexts.len()) {
            return Err(error);
        }

        let context_id = next_vm_context_id();
        contexts.insert(
            context_id,
            VmContextState {
                context: v8::Global::new(scope, context),
                baseline_keys: HashSet::new(),
                mirrored_keys: HashSet::new(),
            },
        );
        Ok(context_id)
    })
}

fn update_vm_context_slot(
    context_id: u32,
    baseline_keys: HashSet<String>,
    mirrored_keys: HashSet<String>,
) {
    VM_CONTEXTS.with(|contexts| {
        if let Some(state) = contexts.borrow_mut().get_mut(&context_id) {
            state.baseline_keys = baseline_keys;
            state.mirrored_keys = mirrored_keys;
        }
    });
}

fn remove_vm_context_slot(context_id: u32) {
    VM_CONTEXTS.with(|contexts| {
        contexts.borrow_mut().remove(&context_id);
    });
}

/// Evict every `node:vm` context slot held by the current isolate thread and
/// reset the id counter.
///
/// `VM_CONTEXTS` is a thread-local that lives for the lifetime of the (reused)
/// isolate thread, not a single execution. `reserve_vm_context_slot` adds a slot
/// for every `vm.createContext()`, but the success path
/// (`update_vm_context_slot`) never removes it — only the sandbox-mirroring error
/// path does. Without a teardown sweep, slots reserved by one execution survive
/// into every later execution on the same isolate, so the registry grows
/// monotonically until it hits the hard cap `MAX_VM_CONTEXTS` and all subsequent
/// `createContext()` calls fail (freed only on thread exit). This sweep releases
/// those slots at a session boundary so the registry returns to empty between
/// executions. See `VmContextRegistryGuard`.
fn reset_vm_context_registry() {
    VM_CONTEXTS.with(|contexts| contexts.borrow_mut().clear());
    NEXT_VM_CONTEXT_ID.with(|next_id| next_id.set(1));
}

/// RAII owner of the thread-local `node:vm` context registry for one session.
///
/// Mirrors the per-session ownership of [`PendingPromises`] (created fresh per
/// session, dropped at teardown): a session holds one guard, and dropping it
/// evicts every context slot the session reserved. Because `Drop` runs on *every*
/// termination path of the frame that holds the guard — normal return, `?` error,
/// early return, and panic unwinding — the slots are reclaimed unconditionally,
/// not only on the happy path. This prevents a reused isolate thread from
/// accumulating `vm.createContext()` slots across executions toward
/// `MAX_VM_CONTEXTS`.
#[must_use = "hold the guard for the session lifetime; dropping it evicts the vm context registry"]
pub struct VmContextRegistryGuard {
    _private: (),
}

impl VmContextRegistryGuard {
    /// Begin a session's ownership of the vm context registry, sweeping any slots
    /// a prior session left on this (reused) isolate thread so the new session
    /// starts from an empty registry.
    pub fn new() -> Self {
        reset_vm_context_registry();
        VmContextRegistryGuard { _private: () }
    }
}

impl Default for VmContextRegistryGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for VmContextRegistryGuard {
    fn drop(&mut self) {
        reset_vm_context_registry();
    }
}

#[cfg(test)]
fn clear_vm_context_registry_for_test() {
    reset_vm_context_registry();
}

#[cfg(test)]
fn fill_vm_context_registry_for_test<'s>(
    scope: &mut v8::HandleScope<'s>,
    context: v8::Local<'s, v8::Context>,
    count: usize,
) {
    clear_vm_context_registry_for_test();
    for _ in 0..count {
        reserve_vm_context_slot(scope, context).expect("fill vm context test registry");
    }
}

#[cfg(test)]
fn vm_context_registry_len_for_test() -> usize {
    VM_CONTEXTS.with(|contexts| contexts.borrow().len())
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
    runtime: Option<&agentos_runtime::RuntimeContext>,
    task_owner: Option<agentos_runtime::TaskOwner>,
) -> Result<v8::Local<'s, v8::Value>, String> {
    let mut timeout_guard = match options.timeout_ms {
        Some(timeout_ms) => {
            let runtime = runtime.ok_or_else(|| {
                format!(
                    "{}: node:vm timeout requires the session runtime context",
                    crate::timeout::TIMEOUT_GUARD_START_ERROR_CODE
                )
            })?;
            let (abort_tx, _abort_rx) = crossbeam_channel::bounded::<()>(0);
            Some(crate::timeout::TimeoutGuard::new(
                runtime,
                task_owner,
                timeout_ms,
                isolate_handle.clone(),
                abort_tx,
            )?)
        }
        None => None,
    };

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
                        .unwrap_or_else(|| v8::Exception::error(tc, failure_message));
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
                    .unwrap_or_else(|| v8::Exception::error(tc, failure_message));
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
    let context_id = match reserve_vm_context_slot(scope, context) {
        Ok(context_id) => context_id,
        Err(message) => {
            return Ok(vm_throw_error(
                scope,
                &message,
                Some("ERR_AGENTOS_VM_CONTEXT_LIMIT"),
                false,
            ));
        }
    };
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
    let mirrored_keys_result = {
        let tc = &mut v8::TryCatch::new(scope);
        let mirrored_keys = {
            let context_scope = &mut v8::ContextScope::new(tc, context);
            let global = context.global(context_scope);
            vm_copy_sandbox_into_context(context_scope, sandbox, global, &HashSet::new())
        };
        if tc.has_caught() {
            Err(tc
                .exception()
                .map(|exception| v8::Global::new(tc, exception)))
        } else {
            Ok(mirrored_keys)
        }
    };
    let mirrored_keys = match mirrored_keys_result {
        Ok(mirrored_keys) => mirrored_keys,
        Err(exception) => {
            remove_vm_context_slot(context_id);
            if let Some(exception) = exception {
                let exception = v8::Local::new(scope, &exception);
                scope.throw_exception(exception);
                return Ok(exception);
            }
            return Ok(vm_throw_error(
                scope,
                "vm.createContext failed while mirroring sandbox properties",
                None,
                false,
            ));
        }
    };

    update_vm_context_slot(context_id, baseline_keys, mirrored_keys);
    Ok(v8::Integer::new_from_unsigned(scope, context_id).into())
}

fn vm_run_in_context_value<'s>(
    scope: &mut v8::HandleScope<'s>,
    args: &mut v8::FunctionCallbackArguments<'s>,
    bridge_ctx: &BridgeCallContext,
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
    let result = vm_run_script_in_context(
        scope,
        isolate_handle,
        context,
        &code,
        &options,
        bridge_ctx.runtime_context(),
        bridge_ctx.timer_task_owner(),
    )?;
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
    bridge_ctx: &BridgeCallContext,
) -> Result<v8::Local<'s, v8::Value>, String> {
    let code = args.get(0).to_rust_string_lossy(scope);
    let options_value = args.get(1);
    let options = vm_options_from_value(scope, options_value);
    let context = scope.get_current_context();
    let isolate_handle = unsafe { args.get_isolate() }.thread_safe_handle();
    vm_run_script_in_context(
        scope,
        isolate_handle,
        context,
        &code,
        &options,
        bridge_ctx.runtime_context(),
        bridge_ctx.timer_task_owner(),
    )
}

fn handle_local_bridge_call<'s>(
    scope: &mut v8::HandleScope<'s>,
    method: &str,
    args: &mut v8::FunctionCallbackArguments<'s>,
    bridge_ctx: &BridgeCallContext,
) -> Result<Option<v8::Local<'s, v8::Value>>, String> {
    match method {
        "process.memoryUsage" => Ok(Some(process_memory_usage_value(scope))),
        "process.cpuUsage" => process_cpu_usage_value(scope, args).map(Some),
        "process.resourceUsage" => process_resource_usage_value(scope).map(Some),
        "process.versions" => Ok(Some(process_versions_value(scope))),
        "_vmCreateContext" => vm_create_context_value(scope, args).map(Some),
        "_vmRunInContext" => vm_run_in_context_value(scope, args, bridge_ctx).map(Some),
        "_vmRunInThisContext" => vm_run_in_this_context_value(scope, args, bridge_ctx).map(Some),
        _ => Ok(None),
    }
}

fn read_response_length_argument(
    scope: &mut v8::HandleScope,
    method: &str,
    args: &v8::FunctionCallbackArguments,
) -> Option<usize> {
    let index = match method {
        "fs.readSync" | "_fsReadRaw" => 1,
        "_fsReadFileRangeRaw" => 2,
        "_pythonStdinRead" | "_kernelStdinReadRaw" | "_kernelStdinRead" => 0,
        _ => return None,
    };
    let value = args.get(index).integer_value(scope)?;
    usize::try_from(value).ok()
}

pub(crate) fn declared_bridge_response_bytes(
    method: &str,
    requested_read_bytes: Option<usize>,
) -> usize {
    if let Some(requested_read_bytes) = requested_read_bytes {
        return requested_read_bytes.saturating_add(READ_RESPONSE_ENVELOPE_BYTES);
    }
    bridge_contract()
        .response_max_bytes
        .get(method)
        .copied()
        .unwrap_or(DEFAULT_BRIDGE_RESPONSE_MAX_BYTES)
}

fn bridge_response_declaration(
    scope: &mut v8::HandleScope,
    method: &str,
    args: &v8::FunctionCallbackArguments,
) -> usize {
    declared_bridge_response_bytes(method, read_response_length_argument(scope, method, args))
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
    methods: &[&str],
) -> BridgeFnStore {
    let context = scope.get_current_context();
    let global = context.global(scope);
    let mut data = Vec::with_capacity(methods.len());

    for &method_name in methods {
        let boxed = Box::new(SyncBridgeFnData {
            ctx,
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

    {
        let tc = &mut v8::TryCatch::new(scope);
        match handle_local_bridge_call(tc, &data.method, &mut args, ctx) {
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

    // Serialize V8 arguments using the Vec released by V8's serializer directly.
    let encoded_args = match serialize_v8_args(scope, &args) {
        Ok(encoded_args) => encoded_args,
        Err(err) => {
            let msg =
                v8::String::new(scope, &format!("bridge serialization error: {}", err)).unwrap();
            let exc = v8::Exception::error(scope, msg);
            scope.throw_exception(exc);
            return;
        }
    };

    // Perform sync-blocking bridge call
    let max_response_bytes = bridge_response_declaration(scope, &data.method, &args);
    match ctx.sync_call_response_with_max_response_bytes(
        &data.method,
        encoded_args,
        max_response_bytes,
    ) {
        Ok(Some(response)) => {
            let v8_val = bridge_response_payload_to_v8(scope, response.status, &response.payload);
            if let Some(val) = v8_val {
                rv.set(val);
            } else {
                let msg = v8::String::new(scope, "bridge response conversion failed").unwrap();
                let exc = v8::Exception::error(scope, msg);
                scope.throw_exception(exc);
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
    methods: &[&str],
) -> AsyncBridgeFnStore {
    let context = scope.get_current_context();
    let global = context.global(scope);
    let mut data = Vec::with_capacity(methods.len());

    for &method_name in methods {
        let boxed = Box::new(AsyncBridgeFnData {
            ctx,
            pending,
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

fn reject_promise_with_error(
    scope: &mut v8::HandleScope,
    resolver: v8::Local<v8::PromiseResolver>,
    message: &str,
    code: Option<&str>,
) {
    let msg = v8::String::new(scope, message).unwrap();
    let exc = v8::Exception::error(scope, msg);
    if let Some(code) = code {
        let exc_object = exc.to_object(scope).unwrap();
        let code_key = v8::String::new(scope, "code").unwrap();
        let code_value = v8::String::new(scope, code).unwrap();
        let _ = exc_object.set(scope, code_key.into(), code_value.into());
    }
    resolver.reject(scope, exc);
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

    let reservation = match pending.reserve() {
        Ok(reservation) => reservation,
        Err(err_msg) => {
            reject_promise_with_error(
                scope,
                resolver,
                &err_msg,
                Some("ERR_AGENTOS_BRIDGE_PENDING_PROMISE_LIMIT"),
            );
            rv.set(promise.into());
            return;
        }
    };

    // Serialize V8 arguments using the Vec released by V8's serializer directly.
    let encoded_args = match serialize_v8_args(scope, &args) {
        Ok(encoded_args) => encoded_args,
        Err(err) => {
            let msg =
                v8::String::new(scope, &format!("bridge serialization error: {}", err)).unwrap();
            let exc = v8::Exception::error(scope, msg);
            scope.throw_exception(exc);
            return;
        }
    };

    // Register the response target and V8 resolver before the request can
    // become host-visible. A fast response therefore cannot outrun resolver
    // installation once response ingress is concurrent with the VM thread.
    let max_response_bytes = bridge_response_declaration(scope, &data.method, &args);
    match ctx.prepare_async_call_with_max_response_bytes(
        &data.method,
        encoded_args,
        max_response_bytes,
    ) {
        Ok(prepared) => {
            let call_id = prepared.call_id;
            let global_resolver = v8::Global::new(scope, resolver);
            pending.insert_reserved(call_id, global_resolver, reservation);
            if let Err(err_msg) = ctx.dispatch_async_call(prepared) {
                pending.remove(call_id);
                reject_promise_with_error(scope, resolver, &err_msg, None);
            }
        }
        Err(err_msg) => {
            // Reject the promise immediately if send fails
            reject_promise_with_error(scope, resolver, &err_msg, None);
        }
    }

    // Return the promise
    rv.set(promise.into());
}

/// Replace stub bridge functions on a snapshot-restored context with real
/// session-local bridge functions. Overwrites the 38 stub globals with
/// functions backed by session-local BridgeCallContext.
///
/// Returns (BridgeFnStore, AsyncBridgeFnStore) that must be kept alive
/// for the lifetime of the V8 context.
pub fn replace_bridge_fns(
    scope: &mut v8::HandleScope,
    ctx: *const BridgeCallContext,
    pending: *const PendingPromises,
    sync_fns: &[&str],
    async_fns: &[&str],
) -> (BridgeFnStore, AsyncBridgeFnStore) {
    // Per-session bridge installation runs once, before any user code executes in
    // this context, so the only `node:vm` context slots present are leftovers from
    // a prior session that reused this isolate thread. Sweep them here so a new
    // session never inherits another session's contexts and the registry can never
    // accumulate across executions toward `MAX_VM_CONTEXTS`. The session should
    // also hold a `VmContextRegistryGuard` to evict its own slots at teardown.
    reset_vm_context_registry();
    let sync_store = register_sync_bridge_fns(scope, ctx, sync_fns);
    let async_store = register_async_bridge_fns(scope, ctx, pending, async_fns);
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

/// Serialize V8 function arguments as an array.
fn serialize_v8_args(
    scope: &mut v8::HandleScope,
    args: &v8::FunctionCallbackArguments,
) -> Result<Vec<u8>, String> {
    let count = args.length();
    let array = v8::Array::new(scope, count);
    for i in 0..count {
        array.set_index(scope, i as u32, args.get(i));
    }
    serialize_v8_value(scope, array.into())
}

/// Resolve or reject a pending async bridge promise by call_id.
///
/// Called when a BridgeResponse arrives during the session event loop.
/// Flushes microtasks after resolution to process .then() handlers.
pub fn resolve_pending_promise(
    scope: &mut v8::HandleScope,
    pending: &PendingPromises,
    call_id: u64,
    status: u8,
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
        let v8_val = bridge_response_payload_to_v8(scope, status, &result_bytes);
        if let Some(val) = v8_val {
            resolver.resolve(scope, val);
        } else {
            let msg = v8::String::new(scope, "bridge response conversion failed").unwrap();
            let exc = v8::Exception::error(scope, msg);
            resolver.reject(scope, exc);
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
        "ERR_AGENTOS_NODE_SYNC_RPC",
        "ERR_AGENTOS_PYTHON_VFS_RPC",
        "ERR_AGENTOS_BRIDGE",
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
    use super::{
        bridge_error_code, clear_vm_context_registry_for_test, declared_bridge_response_bytes,
        deserialize_cbor_value, fill_vm_context_registry_for_test, register_async_bridge_fns,
        register_sync_bridge_fns, reserve_vm_context_slot, reset_vm_context_registry,
        serialize_cbor_value, vm_context_capacity_error, vm_context_registry_len_for_test,
        PendingPromises, VmContextRegistryGuard, MAX_CBOR_BRIDGE_CONTAINER_ITEMS,
        MAX_CBOR_BRIDGE_DEPTH, MAX_PENDING_PROMISES, MAX_VM_CONTEXTS,
    };
    use crate::host_call::BridgeCallContext;
    use crate::ipc_binary::{self, BinaryFrame};
    use crate::isolate;
    use std::io::{Cursor, Write};
    use std::process::Command;
    use std::sync::{Arc, Mutex};

    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().write(buf)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.0.lock().unwrap().flush()
        }
    }

    fn bridge_call_count(bytes: &[u8]) -> usize {
        let mut cursor = Cursor::new(bytes);
        let mut count = 0;
        while let Ok(frame) = ipc_binary::read_frame(&mut cursor) {
            if matches!(frame, BinaryFrame::BridgeCall { .. }) {
                count += 1;
            }
        }
        count
    }

    #[test]
    fn bridge_response_declarations_use_contract_reads_and_bounded_default() {
        assert_eq!(declared_bridge_response_bytes("_log", None), 4096);
        assert_eq!(
            declared_bridge_response_bytes("_loadFileSync", None),
            16 * 1024 * 1024
        );
        assert_eq!(
            declared_bridge_response_bytes("_fsReadRaw", Some(32 * 1024)),
            36 * 1024
        );
        assert_eq!(
            declared_bridge_response_bytes("_unclassifiedBridge", None),
            256 * 1024
        );
    }

    #[test]
    fn bridge_error_code_rejects_guest_controlled_errno_segments() {
        assert_eq!(bridge_error_code("user said 'EACCES: denied'"), None);
        assert_eq!(
            bridge_error_code("prefix: user said 'EPERM': more text"),
            None
        );
        assert_eq!(bridge_error_code("ERR_AGENTOS_FAKE: EACCES: denied"), None);
    }

    #[test]
    fn bridge_error_code_accepts_trusted_agentos_prefixes() {
        assert_eq!(
            bridge_error_code("ERR_AGENTOS_NODE_SYNC_RPC: EACCES: permission denied on /foo"),
            Some("EACCES")
        );
        assert_eq!(
            bridge_error_code("ERR_AGENTOS_PYTHON_VFS_RPC: ENOENT: missing file"),
            Some("ENOENT")
        );
        assert_eq!(bridge_error_code("EEXIST: already exists"), Some("EEXIST"));
    }

    #[test]
    fn bridge_v8_hardening_rejects_cbor_abuse_and_vm_context_reentry_overflow() {
        const SUBPROCESS_ENV: &str = "AGENTOS_V8_BRIDGE_HARDENING_SUBPROCESS";
        if std::env::var_os(SUBPROCESS_ENV).is_none() {
            let output = Command::new(std::env::current_exe().expect("current test binary"))
                .arg("bridge::tests::bridge_v8_hardening_rejects_cbor_abuse_and_vm_context_reentry_overflow")
                .arg("--exact")
                .arg("--nocapture")
                .env(SUBPROCESS_ENV, "1")
                .output()
                .expect("spawn bridge hardening subprocess");
            assert!(
                output.status.success(),
                "bridge hardening subprocess failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status.code(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        isolate::init_v8_platform();

        let mut isolate = isolate::create_isolate(None);
        let context = isolate::create_context(&mut isolate);
        let scope = &mut v8::HandleScope::new(&mut isolate);
        let context = v8::Local::new(scope, &context);
        let scope = &mut v8::ContextScope::new(scope, context);

        let object = v8::Object::new(scope);
        let self_key = v8::String::new(scope, "self").unwrap();
        assert!(object.set(scope, self_key.into(), object.into()).is_some());

        let error = serialize_cbor_value(scope, object.into()).expect_err("cycle rejected");
        assert!(
            error.contains("circular object graph"),
            "unexpected error: {error}"
        );

        let source = v8::String::new(
            scope,
            &format!(
                "const sparse = []; sparse.length = {}; sparse",
                MAX_CBOR_BRIDGE_CONTAINER_ITEMS + 1
            ),
        )
        .unwrap();
        let script = v8::Script::compile(scope, source, None).unwrap();
        let sparse = script.run(scope).unwrap();
        let error = serialize_cbor_value(scope, sparse).expect_err("sparse array rejected");
        assert!(
            error.contains(&format!(
                "item count {} exceeds limit",
                MAX_CBOR_BRIDGE_CONTAINER_ITEMS + 1
            )),
            "unexpected error: {error}"
        );

        let mut value = ciborium::Value::Null;
        for _ in 0..=MAX_CBOR_BRIDGE_DEPTH {
            value = ciborium::Value::Array(vec![value]);
        }
        let mut encoded = Vec::new();
        ciborium::into_writer(&value, &mut encoded).unwrap();
        let error = deserialize_cbor_value(scope, &encoded).expect_err("depth rejected");
        assert!(
            error.contains("CBOR decode failed"),
            "unexpected error: {error}"
        );

        let oversized_len = (MAX_CBOR_BRIDGE_CONTAINER_ITEMS + 1) as u32;
        let oversized_array_header = [
            0x9a,
            (oversized_len >> 24) as u8,
            (oversized_len >> 16) as u8,
            (oversized_len >> 8) as u8,
            oversized_len as u8,
        ];
        let error = deserialize_cbor_value(scope, &oversized_array_header)
            .expect_err("oversized array rejected before element allocation");
        assert!(
            error.contains(&format!(
                "item count {} exceeds limit",
                MAX_CBOR_BRIDGE_CONTAINER_ITEMS + 1
            )),
            "unexpected error: {error}"
        );

        fill_vm_context_registry_for_test(scope, context, MAX_VM_CONTEXTS - 1);
        let bridge_ctx = BridgeCallContext::new(
            Box::new(Vec::new()),
            Box::new(Cursor::new(Vec::new())),
            String::from("test-session"),
        );
        let _bridge_fns = register_sync_bridge_fns(
            scope,
            &bridge_ctx as *const BridgeCallContext,
            &["_vmCreateContext"],
        );

        let source = r#"
            let innerCode;
            const sandbox = {};
            Object.defineProperty(sandbox, "x", {
                get() {
                    try {
                        _vmCreateContext({});
                    } catch (error) {
                        innerCode = error && error.code;
                    }
                    return 1;
                },
                enumerable: true,
            });

            const outerId = _vmCreateContext(sandbox);
            let limitCode;
            try {
                _vmCreateContext({});
            } catch (error) {
                limitCode = error && error.code;
            }

            JSON.stringify({
                innerCode,
                limitCode,
                outerIsInteger: Number.isInteger(outerId),
            })
        "#;
        {
            let tc = &mut v8::TryCatch::new(scope);
            let source = v8::String::new(tc, source).unwrap();
            let script = v8::Script::compile(tc, source, None).unwrap();
            let result = script.run(tc);
            assert!(
                !tc.has_caught(),
                "unexpected exception while testing vm cap"
            );
            let details = result
                .expect("vm context cap script result")
                .to_rust_string_lossy(tc);
            assert_eq!(
                details,
                r#"{"innerCode":"ERR_AGENTOS_VM_CONTEXT_LIMIT","limitCode":"ERR_AGENTOS_VM_CONTEXT_LIMIT","outerIsInteger":true}"#,
                "vm context cap script should observe limit errors"
            );
        }
        assert_eq!(vm_context_registry_len_for_test(), MAX_VM_CONTEXTS);
        clear_vm_context_registry_for_test();

        let source = r#"
            (() => {
                let thrownMessage;
                const sandbox = {};
                Object.defineProperty(sandbox, "x", {
                    get() {
                        throw new Error("sandbox getter failed");
                    },
                    enumerable: true,
                });
                try {
                    _vmCreateContext(sandbox);
                } catch (error) {
                    thrownMessage = error && error.message;
                }

                const nextId = _vmCreateContext({});
                return JSON.stringify({
                    thrownMessage,
                    nextIsInteger: Number.isInteger(nextId),
                });
            })()
        "#;
        {
            let tc = &mut v8::TryCatch::new(scope);
            let source = v8::String::new(tc, source).unwrap();
            let script = v8::Script::compile(tc, source, None).unwrap();
            let result = script.run(tc);
            if tc.has_caught() {
                let exception = tc
                    .exception()
                    .map(|exception| exception.to_rust_string_lossy(tc))
                    .unwrap_or_else(|| String::from("<missing exception>"));
                panic!("unexpected exception while testing vm rollback: {exception}");
            }
            let details = result
                .expect("vm context rollback script result")
                .to_rust_string_lossy(tc);
            assert_eq!(
                details, r#"{"thrownMessage":"sandbox getter failed","nextIsInteger":true}"#,
                "vm context rollback script should preserve the getter exception and keep registry usable"
            );
        }
        assert_eq!(vm_context_registry_len_for_test(), 1);
        clear_vm_context_registry_for_test();

        let async_writer = Arc::new(Mutex::new(Vec::new()));
        let async_bridge_ctx = BridgeCallContext::new(
            Box::new(SharedWriter(Arc::clone(&async_writer))),
            Box::new(Cursor::new(Vec::new())),
            String::from("test-session"),
        );
        let async_pending = PendingPromises::new();
        let _async_bridge_fns = register_async_bridge_fns(
            scope,
            &async_bridge_ctx as *const BridgeCallContext,
            &async_pending as *const PendingPromises,
            &["_asyncFn"],
        );
        let source = format!(
            r#"
            for (let i = 0; i < {fill_count}; i++) {{
                _asyncFn(i);
            }}
            globalThis.__overflowPromise = _asyncFn("overflow");
            "#,
            fill_count = MAX_PENDING_PROMISES,
        );
        {
            let tc = &mut v8::TryCatch::new(scope);
            let source = v8::String::new(tc, &source).unwrap();
            let script = v8::Script::compile(tc, source, None).unwrap();
            assert!(script.run(tc).is_some());
            assert!(!tc.has_caught(), "async overflow should reject, not throw");
        }
        assert_eq!(async_pending.len(), MAX_PENDING_PROMISES);
        assert_eq!(
            bridge_call_count(&async_writer.lock().unwrap()),
            MAX_PENDING_PROMISES
        );
        {
            let key = v8::String::new(scope, "__overflowPromise").unwrap();
            let value = context.global(scope).get(scope, key.into()).unwrap();
            let promise = v8::Local::<v8::Promise>::try_from(value).unwrap();
            assert_eq!(promise.state(), v8::PromiseState::Rejected);
            let rejection = promise.result(scope);
            let rejection = v8::Local::<v8::Object>::try_from(rejection).unwrap();
            let code_key = v8::String::new(scope, "code").unwrap();
            let code = rejection.get(scope, code_key.into()).unwrap();
            assert_eq!(
                code.to_rust_string_lossy(scope),
                "ERR_AGENTOS_BRIDGE_PENDING_PROMISE_LIMIT"
            );
        }

        let reentrant_writer = Arc::new(Mutex::new(Vec::new()));
        let reentrant_bridge_ctx = BridgeCallContext::new(
            Box::new(SharedWriter(Arc::clone(&reentrant_writer))),
            Box::new(Cursor::new(Vec::new())),
            String::from("test-session"),
        );
        let reentrant_pending = PendingPromises::new();
        let _reentrant_async_bridge_fns = register_async_bridge_fns(
            scope,
            &reentrant_bridge_ctx as *const BridgeCallContext,
            &reentrant_pending as *const PendingPromises,
            &["_asyncFn"],
        );
        let source = format!(
            r#"
            for (let i = 0; i < {fill_count}; i++) {{
                _asyncFn(i);
            }}
            let innerPromise;
            const reentrantArg = {{}};
            Object.defineProperty(reentrantArg, "x", {{
                get() {{
                    innerPromise = _asyncFn("inner");
                    return 1;
                }},
                enumerable: true,
            }});
            globalThis.__reentrantOuterPromise = _asyncFn(reentrantArg);
            globalThis.__reentrantInnerPromise = innerPromise;
            "#,
            fill_count = MAX_PENDING_PROMISES - 1,
        );
        {
            let tc = &mut v8::TryCatch::new(scope);
            let source = v8::String::new(tc, &source).unwrap();
            let script = v8::Script::compile(tc, source, None).unwrap();
            assert!(script.run(tc).is_some());
            assert!(!tc.has_caught(), "async reentry should reject, not throw");
        }
        assert_eq!(reentrant_pending.len(), MAX_PENDING_PROMISES);
        assert_eq!(
            bridge_call_count(&reentrant_writer.lock().unwrap()),
            MAX_PENDING_PROMISES
        );
        {
            let key = v8::String::new(scope, "__reentrantInnerPromise").unwrap();
            let value = context.global(scope).get(scope, key.into()).unwrap();
            let promise = v8::Local::<v8::Promise>::try_from(value).unwrap();
            assert_eq!(promise.state(), v8::PromiseState::Rejected);
            let rejection = promise.result(scope);
            let rejection = v8::Local::<v8::Object>::try_from(rejection).unwrap();
            let code_key = v8::String::new(scope, "code").unwrap();
            let code = rejection.get(scope, code_key.into()).unwrap();
            assert_eq!(
                code.to_rust_string_lossy(scope),
                "ERR_AGENTOS_BRIDGE_PENDING_PROMISE_LIMIT"
            );
        }

        let buffer_reentry_writer = Arc::new(Mutex::new(Vec::new()));
        let buffer_reentry_bridge_ctx = BridgeCallContext::new(
            Box::new(SharedWriter(Arc::clone(&buffer_reentry_writer))),
            Box::new(Cursor::new(Vec::new())),
            String::from("test-session"),
        );
        let buffer_reentry_pending = PendingPromises::new();
        let _buffer_reentry_async_bridge_fns = register_async_bridge_fns(
            scope,
            &buffer_reentry_bridge_ctx as *const BridgeCallContext,
            &buffer_reentry_pending as *const PendingPromises,
            &["_asyncFn"],
        );
        let source = r#"
            let bufferInnerPromise;
            const bufferReentrantArg = {};
            Object.defineProperty(bufferReentrantArg, "x", {
                get() {
                    bufferInnerPromise = _asyncFn("inner");
                    return 1;
                },
                enumerable: true,
            });
            globalThis.__bufferOuterPromise = _asyncFn(bufferReentrantArg);
            globalThis.__bufferInnerPromise = bufferInnerPromise;
        "#;
        {
            let tc = &mut v8::TryCatch::new(scope);
            let source = v8::String::new(tc, source).unwrap();
            let script = v8::Script::compile(tc, source, None).unwrap();
            assert!(script.run(tc).is_some());
            assert!(
                !tc.has_caught(),
                "async serialization reentry should not panic or throw"
            );
        }
        assert_eq!(buffer_reentry_pending.len(), 2);
        assert_eq!(bridge_call_count(&buffer_reentry_writer.lock().unwrap()), 2);
    }

    #[test]
    fn vm_context_capacity_error_trips_at_registry_limit() {
        assert!(vm_context_capacity_error(MAX_VM_CONTEXTS - 1).is_none());

        let error = vm_context_capacity_error(MAX_VM_CONTEXTS).expect("limit error");
        assert!(
            error.contains(&format!("limit of {MAX_VM_CONTEXTS} contexts")),
            "unexpected error: {error}"
        );
    }

    // Regression test for the `VM_CONTEXTS` leak: `reserve_vm_context_slot` adds a
    // slot per `vm.createContext()`, but the success path never removes it. On a
    // reused isolate thread that made the registry grow without bound across
    // executions until it hit `MAX_VM_CONTEXTS` and every later `createContext()`
    // failed. With the fix, a session's `VmContextRegistryGuard` evicts the slots
    // at teardown, so the registry returns to empty between executions and the cap
    // is never reached. This asserts the safeguard FIRING (slots reclaimed), it
    // does not saturate any resource, so it stays in the default suite.
    //
    // Runs in a subprocess to match the V8-initializing test convention in this
    // module (one isolate per process, no cross-test V8 interference).
    #[test]
    fn vm_context_registry_evicts_slots_on_finalize() {
        const SUBPROCESS_ENV: &str = "AGENTOS_V8_VM_CONTEXT_FINALIZE_SUBPROCESS";
        if std::env::var_os(SUBPROCESS_ENV).is_none() {
            let output = Command::new(std::env::current_exe().expect("current test binary"))
                .arg("bridge::tests::vm_context_registry_evicts_slots_on_finalize")
                .arg("--exact")
                .arg("--nocapture")
                .env(SUBPROCESS_ENV, "1")
                .output()
                .expect("spawn vm context finalize subprocess");
            assert!(
                output.status.success(),
                "vm context finalize subprocess failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status.code(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        isolate::init_v8_platform();
        let mut isolate = isolate::create_isolate(None);
        let context = isolate::create_context(&mut isolate);
        let scope = &mut v8::HandleScope::new(&mut isolate);
        let context = v8::Local::new(scope, &context);
        let scope = &mut v8::ContextScope::new(scope, context);

        reset_vm_context_registry();
        assert_eq!(
            vm_context_registry_len_for_test(),
            0,
            "registry starts empty"
        );

        // Simulate many executions on the same reused isolate. Each execution
        // reserves several contexts and then finalizes (its session guard drops).
        // Run far past the hard cap: a leaked slot per createContext would exhaust
        // MAX_VM_CONTEXTS within the first ~341 executions and the `.expect` on the
        // next reservation would panic.
        const CONTEXTS_PER_EXECUTION: usize = 3;
        let executions = MAX_VM_CONTEXTS * 4;
        for execution in 0..executions {
            {
                let _guard = VmContextRegistryGuard::new();
                for _ in 0..CONTEXTS_PER_EXECUTION {
                    reserve_vm_context_slot(scope, context)
                        .expect("reserve vm context slot below cap; a leak would hit the cap");
                }
                assert_eq!(
                    vm_context_registry_len_for_test(),
                    CONTEXTS_PER_EXECUTION,
                    "slots reserved during execution {execution} must be live"
                );
            }
            // Guard dropped above -> finalize. Asserting here, before the next
            // execution's guard is constructed, proves the Drop sweep (not the
            // start-of-session sweep) reclaimed the slots.
            assert_eq!(
                vm_context_registry_len_for_test(),
                0,
                "registry must be empty after execution {execution} finalizes"
            );
        }

        // After 4x the cap worth of reservations, a fresh createContext still
        // succeeds because nothing leaked across executions.
        let _guard = VmContextRegistryGuard::new();
        assert!(
            reserve_vm_context_slot(scope, context).is_ok(),
            "createContext must keep succeeding; leaked slots would have hit MAX_VM_CONTEXTS"
        );
    }
}
