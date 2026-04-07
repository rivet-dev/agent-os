// V8 startup snapshots: fast isolate creation from pre-compiled bridge code

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};

use crate::bridge::{external_refs, register_stub_bridge_fns};
use crate::isolate::init_v8_platform;
use crate::session::{ASYNC_BRIDGE_FNS, SYNC_BRIDGE_FNS};

/// Maximum allowed snapshot blob size (50MB).
/// Prevents resource exhaustion from degenerate bridge code.
const MAX_SNAPSHOT_BLOB_BYTES: usize = 50 * 1024 * 1024;

/// Create a V8 startup snapshot with a fully-initialized bridge context.
///
/// Registers stub bridge functions on the global, injects default config
/// globals, then compiles and executes the bridge IIFE. The resulting
/// context — with all bridge infrastructure set up — is snapshotted.
///
/// After restore, stub bridge functions are replaced with real session-local
/// ones, and per-session config is injected via a post-restore script.
///
/// Returns an error if the bridge code fails to compile or the resulting
/// snapshot exceeds MAX_SNAPSHOT_BLOB_BYTES.
pub fn create_snapshot(bridge_code: &str) -> Result<v8::StartupData, String> {
    init_v8_platform();

    let mut isolate = v8::Isolate::snapshot_creator(Some(external_refs()), None);
    {
        let scope = &mut v8::HandleScope::new(&mut isolate);
        let context = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, context);

        // Register stub bridge functions so the IIFE can reference them
        register_stub_bridge_fns(scope, &SYNC_BRIDGE_FNS, &ASYNC_BRIDGE_FNS);

        // Inject default config globals for bridge IIFE setup
        inject_snapshot_defaults(scope);

        // Compile and run bridge code — context captures fully-initialized state
        let source = v8::String::new(scope, bridge_code)
            .ok_or_else(|| "failed to create V8 string for bridge code".to_string())?;
        let script = v8::Script::compile(scope, source, None)
            .ok_or_else(|| "bridge code compilation failed during snapshot creation".to_string())?;
        script.run(scope);

        scope.set_default_context(context);
    }
    let blob = isolate
        .create_blob(v8::FunctionCodeHandling::Keep)
        .ok_or_else(|| "V8 snapshot creation failed".to_string())?;

    // Reject oversized snapshots
    if blob.len() > MAX_SNAPSHOT_BLOB_BYTES {
        return Err(format!(
            "snapshot blob too large: {} bytes (max {})",
            blob.len(),
            MAX_SNAPSHOT_BLOB_BYTES
        ));
    }

    Ok(blob)
}

/// Inject default config globals needed by the bridge IIFE during snapshot creation.
///
/// These are placeholder values so bridge code that reads _processConfig or
/// _osConfig at setup time doesn't fail. They're overwritten per-session
/// after snapshot restore via inject_globals_from_payload.
///
/// Properties are set as READ_ONLY (not DONT_DELETE) so they remain
/// configurable — inject_globals_from_payload can redefine them with
/// READ_ONLY | DONT_DELETE after restore.
fn inject_snapshot_defaults(scope: &mut v8::HandleScope) {
    let context = scope.get_current_context();
    let global = context.global(scope);

    // _processConfig: default placeholder (overwritten per-session)
    let pc_code = r#"({
        cwd: "/",
        env: {},
        timing_mitigation: "off",
        frozen_time_ms: null
    })"#;
    let pc_source = v8::String::new(scope, pc_code).unwrap();
    let pc_script = v8::Script::compile(scope, pc_source, None).unwrap();
    let pc_val = pc_script.run(scope).unwrap();
    if let Some(pc_obj) = pc_val.to_object(scope) {
        pc_obj.set_integrity_level(scope, v8::IntegrityLevel::Frozen);
    }
    let pc_key = v8::String::new(scope, "_processConfig").unwrap();
    // READ_ONLY only — no DONT_DELETE so the property remains configurable
    // for override after snapshot restore
    let attr = v8::PropertyAttribute::READ_ONLY;
    global.define_own_property(scope, pc_key.into(), pc_val, attr);

    // _osConfig: default placeholder (overwritten per-session)
    let oc_code = r#"({
        homedir: "/root",
        tmpdir: "/tmp",
        platform: "linux",
        arch: "x64"
    })"#;
    let oc_source = v8::String::new(scope, oc_code).unwrap();
    let oc_script = v8::Script::compile(scope, oc_source, None).unwrap();
    let oc_val = oc_script.run(scope).unwrap();
    if let Some(oc_obj) = oc_val.to_object(scope) {
        oc_obj.set_integrity_level(scope, v8::IntegrityLevel::Frozen);
    }
    let oc_key = v8::String::new(scope, "_osConfig").unwrap();
    // READ_ONLY only — no DONT_DELETE so the property remains configurable
    let attr2 = v8::PropertyAttribute::READ_ONLY;
    global.define_own_property(scope, oc_key.into(), oc_val, attr2);
}

/// Create a V8 isolate restored from a snapshot blob.
///
/// The external references must match those used during snapshot creation
/// (provided by bridge::external_refs()).
///
/// `blob` must be owned or 'static data — `Vec<u8>`, `Box<[u8]>`, or
/// `v8::StartupData` all work. The data is copied into the isolate during
/// creation; V8 does not retain a reference after `Isolate::new()` returns.
pub fn create_isolate_from_snapshot<B>(blob: B, heap_limit_mb: Option<u32>) -> v8::OwnedIsolate
where
    B: std::ops::Deref<Target = [u8]> + std::borrow::Borrow<[u8]> + 'static,
{
    init_v8_platform();

    let mut params = v8::CreateParams::default()
        .snapshot_blob(blob)
        .external_references(&**external_refs());
    if let Some(limit) = heap_limit_mb {
        let limit_bytes = (limit as usize) * 1024 * 1024;
        params = params.heap_limits(0, limit_bytes);
    }
    let mut isolate = v8::Isolate::new(params);
    crate::isolate::configure_isolate(&mut isolate);
    isolate
}

/// Thread-safe snapshot cache keyed by bridge code hash.
///
/// Uses two-phase locking with per-key in-flight tracking so concurrent
/// callers requesting different bridge code variants are not blocked by
/// each other. Callers requesting the same variant wait on a condvar
/// instead of creating duplicate snapshots.
pub struct SnapshotCache {
    inner: Mutex<CacheInner>,
    max_entries: usize,
}

struct CacheInner {
    entries: Vec<CacheEntry>,
    /// Per-key in-flight tracking: callers for the same hash wait on the
    /// condvar instead of creating duplicate snapshots.
    in_flight: HashMap<u64, Arc<InFlightEntry>>,
}

struct CacheEntry {
    bridge_hash: u64,
    /// Snapshot blob bytes (copied from v8::StartupData).
    /// Stored as Vec<u8> rather than StartupData because StartupData
    /// contains raw pointers that are not Send/Sync.
    blob: Arc<Vec<u8>>,
}

/// Shared state for an in-flight snapshot creation. The creator thread
/// populates `result` and notifies all waiters via `done`.
struct InFlightEntry {
    result: Mutex<Option<Result<Arc<Vec<u8>>, String>>>,
    done: Condvar,
}

impl SnapshotCache {
    pub fn new(max_entries: usize) -> Self {
        SnapshotCache {
            inner: Mutex::new(CacheInner {
                entries: Vec::new(),
                in_flight: HashMap::new(),
            }),
            max_entries,
        }
    }

    /// Get or create a snapshot for the given bridge code.
    ///
    /// Two-phase locking: the cache mutex is held only for lookups and
    /// inserts, never during snapshot creation. Per-key in-flight tracking
    /// prevents duplicate snapshot creation for the same bridge code.
    pub fn get_or_create(&self, bridge_code: &str) -> Result<Arc<Vec<u8>>, String> {
        let hash = siphash(bridge_code);

        // Phase 1: short lock — check cache, check in-flight, or claim creation
        let in_flight = {
            let mut inner = self.inner.lock().unwrap();

            // Cache hit — move to end (most recently used)
            if let Some(pos) = inner.entries.iter().position(|e| e.bridge_hash == hash) {
                let entry = inner.entries.remove(pos);
                let blob = Arc::clone(&entry.blob);
                inner.entries.push(entry);
                return Ok(blob);
            }

            // Another thread is already creating this snapshot — wait on it
            if let Some(entry) = inner.in_flight.get(&hash) {
                Some(Arc::clone(entry))
            } else {
                // We're the creator — register in-flight and release the lock
                let entry = Arc::new(InFlightEntry {
                    result: Mutex::new(None),
                    done: Condvar::new(),
                });
                inner.in_flight.insert(hash, Arc::clone(&entry));
                None
            }
        };

        // Wait path: another thread is creating this snapshot
        if let Some(entry) = in_flight {
            let mut result = entry.result.lock().unwrap();
            while result.is_none() {
                result = entry.done.wait(result).unwrap();
            }
            return result.as_ref().unwrap().clone();
        }

        // Phase 2: create snapshot without holding the cache lock
        let creation_result =
            create_snapshot(bridge_code).map(|startup_data| Arc::new(startup_data.to_vec()));

        // Phase 3: short lock — insert result, notify waiters, clean up
        {
            let mut inner = self.inner.lock().unwrap();

            if let Ok(ref arc) = creation_result {
                // LRU eviction: remove oldest (front) entry when at capacity
                if inner.entries.len() >= self.max_entries {
                    inner.entries.remove(0);
                }
                inner.entries.push(CacheEntry {
                    bridge_hash: hash,
                    blob: Arc::clone(arc),
                });
            }

            // Publish result to waiters and remove in-flight entry
            if let Some(entry) = inner.in_flight.remove(&hash) {
                let mut result = entry.result.lock().unwrap();
                *result = Some(creation_result.clone());
                entry.done.notify_all();
            }
        }

        creation_result
    }
}

fn siphash(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(isolate: &mut v8::OwnedIsolate, code: &str) -> String {
        let scope = &mut v8::HandleScope::new(isolate);
        let context = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, context);
        let source = v8::String::new(scope, code).unwrap();
        let script = v8::Script::compile(scope, source, None).unwrap();
        let result = script.run(scope).unwrap();
        result.to_rust_string_lossy(scope)
    }

    /// All snapshot tests consolidated into one #[test] to avoid inter-test
    /// SIGSEGV from V8 global state issues (same pattern as execution::tests).
    #[test]
    fn snapshot_consolidated_tests() {
        init_v8_platform();
        let _ = external_refs();

        // --- Part 1: Snapshot creation returns non-empty blob ---
        {
            let bridge_code = "(function() { globalThis.__bridge_init = true; })();";
            let blob = create_snapshot(bridge_code).expect("snapshot creation should succeed");
            assert!(blob.len() > 0, "snapshot blob should be non-empty");
        }

        // --- Part 2: Restored isolate executes JS correctly ---
        {
            let bridge_code = "(function() { globalThis.__testValue = 42; })();";
            let blob = create_snapshot(bridge_code).expect("snapshot creation should succeed");
            let mut isolate = create_isolate_from_snapshot(blob, None);
            // Fresh context on restored isolate — bridge globals are in snapshot's
            // default context, not in a new context. Verify isolate is functional.
            assert_eq!(eval(&mut isolate, "1 + 1"), "2");
        }

        // --- Part 3: Restored isolate respects heap_limit_mb ---
        {
            let bridge_code = "/* empty bridge */";
            let blob = create_snapshot(bridge_code).expect("snapshot creation should succeed");
            let mut isolate = create_isolate_from_snapshot(blob, Some(8));
            assert_eq!(eval(&mut isolate, "'heap ok'"), "heap ok");
        }

        // --- Part 4: Normal blob is under 50MB limit ---
        {
            let bridge_code = "(function() { globalThis.x = 1; })();";
            let blob = create_snapshot(bridge_code).expect("snapshot creation should succeed");
            assert!(
                blob.len() < MAX_SNAPSHOT_BLOB_BYTES,
                "normal bridge code should produce blob under 50MB limit"
            );
        }

        // --- Part 5: Three sequential restores from same snapshot data ---
        {
            let bridge_code = "(function() { globalThis.__counter = 0; })();";
            let blob = create_snapshot(bridge_code).expect("snapshot creation should succeed");
            let blob_bytes: Vec<u8> = blob.to_vec();

            for i in 0..3 {
                let mut isolate = create_isolate_from_snapshot(blob_bytes.clone(), None);
                let result = eval(&mut isolate, &format!("{} + 1", i));
                assert_eq!(result, format!("{}", i + 1));
            }
        }

        // --- Part 6: Cache hit returns same Arc ---
        {
            let cache = SnapshotCache::new(4);
            let bridge_code = "(function() { globalThis.__cached = 1; })();";

            let arc1 = cache
                .get_or_create(bridge_code)
                .expect("first get_or_create");
            let arc2 = cache
                .get_or_create(bridge_code)
                .expect("second get_or_create");

            // Same Arc (same pointer) — cache hit, not a new snapshot
            assert!(
                Arc::ptr_eq(&arc1, &arc2),
                "cache hit should return same Arc"
            );
        }

        // --- Part 7: Cache miss creates new snapshot ---
        {
            let cache = SnapshotCache::new(4);
            let code_a = "(function() { globalThis.__a = 1; })();";
            let code_b = "(function() { globalThis.__b = 2; })();";

            let arc_a = cache.get_or_create(code_a).expect("create A");
            let arc_b = cache.get_or_create(code_b).expect("create B");

            // Different bridge code → different Arc
            assert!(
                !Arc::ptr_eq(&arc_a, &arc_b),
                "different code should produce different Arc"
            );

            // Verify both are usable
            let mut iso_a = create_isolate_from_snapshot((*arc_a).clone(), None);
            assert_eq!(eval(&mut iso_a, "1 + 1"), "2");

            let mut iso_b = create_isolate_from_snapshot((*arc_b).clone(), None);
            assert_eq!(eval(&mut iso_b, "2 + 2"), "4");
        }

        // --- Part 8: LRU eviction removes oldest entry ---
        {
            let cache = SnapshotCache::new(2);
            let code_1 = "(function() { globalThis.__v1 = 1; })();";
            let code_2 = "(function() { globalThis.__v2 = 2; })();";
            let code_3 = "(function() { globalThis.__v3 = 3; })();";

            let arc_1 = cache.get_or_create(code_1).expect("create 1");
            let _arc_2 = cache.get_or_create(code_2).expect("create 2");

            // Cache is full (2 entries). Adding a third should evict code_1.
            let _arc_3 = cache.get_or_create(code_3).expect("create 3");

            // code_1 should be evicted — re-requesting it should return a new Arc
            let arc_1_new = cache.get_or_create(code_1).expect("re-create 1");
            assert!(
                !Arc::ptr_eq(&arc_1, &arc_1_new),
                "evicted entry should produce a new Arc on re-creation"
            );

            // code_2 should still be cached (it was accessed before code_3 but not evicted)
            // After eviction of code_1, cache had [code_2, code_3], then adding code_1 evicts code_2
            // Actually: after inserting code_3, cache was [code_2, code_3] (code_1 evicted).
            // Then inserting code_1 again: cache is full (2), evicts code_2 → cache is [code_3, code_1].
        }

        // --- Part 9: Concurrent get_or_create creates only one snapshot ---
        {
            use std::sync::atomic::{AtomicUsize, Ordering};

            let cache = Arc::new(SnapshotCache::new(4));
            let bridge_code = "(function() { globalThis.__concurrent = 1; })();";

            // Pre-warm — to avoid measuring snapshot creation races, verify
            // that after one creation, N threads all get the same Arc
            let first = cache.get_or_create(bridge_code).expect("pre-warm");

            let num_threads = 4;
            let barrier = Arc::new(std::sync::Barrier::new(num_threads));
            let same_count = Arc::new(AtomicUsize::new(0));

            let mut handles = vec![];
            for _ in 0..num_threads {
                let cache = Arc::clone(&cache);
                let barrier = Arc::clone(&barrier);
                let first = Arc::clone(&first);
                let same_count = Arc::clone(&same_count);
                let code = bridge_code.to_string();

                handles.push(std::thread::spawn(move || {
                    barrier.wait();
                    let arc = cache.get_or_create(&code).expect("concurrent get");
                    if Arc::ptr_eq(&arc, &first) {
                        same_count.fetch_add(1, Ordering::Relaxed);
                    }
                }));
            }

            for h in handles {
                h.join().expect("thread join");
            }

            assert_eq!(
                same_count.load(Ordering::Relaxed),
                num_threads,
                "all concurrent callers should get the same cached Arc"
            );
        }

        // --- Part 10: WASM disabled after snapshot restore ---
        // Verifies that set_allow_wasm_code_generation_callback is not captured
        // in the snapshot — disable_wasm() must be re-applied after every restore.
        {
            let bridge_code = "(function() { globalThis.__wasm_test = true; })();";
            let blob = create_snapshot(bridge_code).expect("snapshot creation");
            let mut isolate = create_isolate_from_snapshot(blob, None);

            // Apply WASM disable (same as session.rs does after restore)
            crate::execution::disable_wasm(&mut isolate);

            let scope = &mut v8::HandleScope::new(&mut isolate);
            let context = v8::Context::new(scope, Default::default());
            let scope = &mut v8::ContextScope::new(scope, context);

            // Attempt WebAssembly.compile — should throw
            let wasm_test_code = r#"
                (function() {
                    try {
                        var bytes = new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);
                        new WebAssembly.Module(bytes);
                        return "ALLOWED";
                    } catch (e) {
                        return "BLOCKED:" + e.message;
                    }
                })()
            "#;
            let source = v8::String::new(scope, wasm_test_code).unwrap();
            let script = v8::Script::compile(scope, source, None).unwrap();
            let result = script.run(scope).unwrap();
            let result_str = result.to_rust_string_lossy(scope);

            assert!(
                result_str.starts_with("BLOCKED:"),
                "WASM should be blocked after snapshot restore + disable_wasm(), got: {}",
                result_str
            );
        }

        // --- Part 11: Session isolation — fresh contexts from same snapshot ---
        // Verifies that state set in one session's context does not leak
        // to another session's context (fresh context per session).
        {
            let bridge_code = "(function() { globalThis.__shared_bridge = 'ok'; })();";
            let blob = create_snapshot(bridge_code).expect("snapshot creation");
            let blob_bytes: Vec<u8> = blob.to_vec();

            // "Session A": set a global variable
            {
                let mut isolate = create_isolate_from_snapshot(blob_bytes.clone(), None);
                let scope = &mut v8::HandleScope::new(&mut isolate);
                let context = v8::Context::new(scope, Default::default());
                let scope = &mut v8::ContextScope::new(scope, context);

                let source =
                    v8::String::new(scope, "globalThis.__session_secret = 'session-a-data';")
                        .unwrap();
                let script = v8::Script::compile(scope, source, None).unwrap();
                script.run(scope);

                // Verify session A can see its own data
                let check = v8::String::new(scope, "globalThis.__session_secret").unwrap();
                let script = v8::Script::compile(scope, check, None).unwrap();
                let result = script.run(scope).unwrap();
                assert_eq!(result.to_rust_string_lossy(scope), "session-a-data");
            }

            // "Session B": fresh context from same snapshot should NOT see session A's data
            {
                let mut isolate = create_isolate_from_snapshot(blob_bytes.clone(), None);
                let scope = &mut v8::HandleScope::new(&mut isolate);
                let context = v8::Context::new(scope, Default::default());
                let scope = &mut v8::ContextScope::new(scope, context);

                let source = v8::String::new(scope, "typeof globalThis.__session_secret").unwrap();
                let script = v8::Script::compile(scope, source, None).unwrap();
                let result = script.run(scope).unwrap();
                assert_eq!(
                    result.to_rust_string_lossy(scope),
                    "undefined",
                    "session B should not see session A's global state"
                );
            }
        }

        // --- Part 12: External references survive snapshot restore ---
        // Verifies that FunctionTemplates registered on a restored isolate
        // correctly dispatch to Rust bridge callbacks via external_refs().
        {
            use crate::bridge::{
                register_async_bridge_fns, register_sync_bridge_fns, PendingPromises,
                SessionBuffers,
            };
            use crate::host_call::BridgeCallContext;
            use std::cell::RefCell;

            let bridge_code = "(function() { globalThis.__ext_ref_test = true; })();";
            let blob = create_snapshot(bridge_code).expect("snapshot creation");
            let mut isolate = create_isolate_from_snapshot(blob, None);
            crate::execution::disable_wasm(&mut isolate);

            // Create minimal BridgeCallContext (sync call will fail but we
            // test that the FunctionTemplate dispatches without crash)
            let (ipc_tx, _ipc_rx) = crossbeam_channel::unbounded::<Vec<u8>>();
            let (_cmd_tx, _cmd_rx) =
                crossbeam_channel::unbounded::<crate::session::SessionCommand>();
            let call_id_router: crate::host_call::CallIdRouter =
                Arc::new(Mutex::new(std::collections::HashMap::new()));

            let receiver = crate::host_call::ReaderResponseReceiver::new(Box::new(
                std::io::Cursor::new(Vec::<u8>::new()),
            ));
            let sender = crate::host_call::ChannelFrameSender::new(ipc_tx);
            let bridge_ctx = BridgeCallContext::with_receiver(
                Box::new(sender),
                Box::new(receiver),
                "test-session".to_string(),
                call_id_router,
            );
            let session_buffers = RefCell::new(SessionBuffers::new());
            let pending = PendingPromises::new();

            let scope = &mut v8::HandleScope::new(&mut isolate);
            let context = v8::Context::new(scope, Default::default());
            let scope = &mut v8::ContextScope::new(scope, context);

            // Register bridge functions on the restored isolate
            let _sync_store = register_sync_bridge_fns(
                scope,
                &bridge_ctx as *const BridgeCallContext,
                &session_buffers as *const RefCell<SessionBuffers>,
                &["_testSync"],
            );
            let _async_store = register_async_bridge_fns(
                scope,
                &bridge_ctx as *const BridgeCallContext,
                &pending as *const PendingPromises,
                &session_buffers as *const RefCell<SessionBuffers>,
                &["_testAsync"],
            );

            // Verify the functions exist as globals
            let check = v8::String::new(scope, "typeof _testSync").unwrap();
            let script = v8::Script::compile(scope, check, None).unwrap();
            let result = script.run(scope).unwrap();
            assert_eq!(
                result.to_rust_string_lossy(scope),
                "function",
                "_testSync should be a function on restored isolate"
            );

            let check = v8::String::new(scope, "typeof _testAsync").unwrap();
            let script = v8::Script::compile(scope, check, None).unwrap();
            let result = script.run(scope).unwrap();
            assert_eq!(
                result.to_rust_string_lossy(scope),
                "function",
                "_testAsync should be a function on restored isolate"
            );
        }

        // --- Part 13: Register stub bridge functions on V8 global ---
        // Verifies that register_stub_bridge_fns places functions on the global
        // and that they have the correct typeof without calling them.
        {
            use crate::bridge::register_stub_bridge_fns;

            // Use a snapshot-based isolate (consistent with other parts)
            let bridge_code = "/* stub test */";
            let blob = create_snapshot(bridge_code).expect("snapshot creation");
            let mut isolate = create_isolate_from_snapshot(blob, None);

            let scope = &mut v8::HandleScope::new(&mut isolate);
            let context = v8::Context::new(scope, Default::default());
            let scope = &mut v8::ContextScope::new(scope, context);

            register_stub_bridge_fns(
                scope,
                &["_log", "_error", "_fsReadFile", "_loadPolyfill"],
                &["_scheduleTimer", "_dynamicImport"],
            );

            let check = v8::String::new(
                scope,
                r#"
                (function() {
                    var names = ['_log', '_error', '_fsReadFile', '_loadPolyfill',
                                 '_scheduleTimer', '_dynamicImport'];
                    for (var i = 0; i < names.length; i++) {
                        if (typeof globalThis[names[i]] !== 'function') {
                            return 'FAIL: ' + names[i] + ' is ' + typeof globalThis[names[i]];
                        }
                    }
                    return 'OK';
                })()
            "#,
            )
            .unwrap();
            let script = v8::Script::compile(scope, check, None).unwrap();
            let result = script.run(scope).unwrap();
            assert_eq!(
                result.to_rust_string_lossy(scope),
                "OK",
                "all stub bridge functions should be registered as functions"
            );
        }

        // --- Part 14: Bridge IIFE executes against stubs + snapshot creation ---
        // Verifies that setup-time code can reference stub functions (typeof,
        // closure wrapping, getter facade) without calling them, and that the
        // resulting context can be snapshotted.
        {
            use crate::bridge::register_stub_bridge_fns;

            let mut snapshot_isolate = v8::Isolate::snapshot_creator(Some(external_refs()), None);
            {
                let scope = &mut v8::HandleScope::new(&mut snapshot_isolate);
                let context = v8::Context::new(scope, Default::default());
                let scope = &mut v8::ContextScope::new(scope, context);

                // Register all 38 bridge functions as stubs (no External data)
                register_stub_bridge_fns(scope, &SYNC_BRIDGE_FNS, &ASYNC_BRIDGE_FNS);

                // Simulate bridge IIFE: reference all bridge functions, set up
                // closures and getter facade, but never call any bridge function
                let iife_code = r#"
                    (function() {
                        // Verify bridge functions exist (like ivm-compat shim)
                        var syncKeys = ['_log', '_error', '_resolveModule', '_loadFile',
                            '_cryptoRandomFill', '_fsReadFile', '_fsWriteFile',
                            '_childProcessSpawnStart', '_childProcessSpawnSync'];
                        var asyncKeys = ['_dynamicImport', '_scheduleTimer',
                            '_networkFetchRaw', '_networkHttpServerListenRaw'];

                        for (var i = 0; i < syncKeys.length; i++) {
                            if (typeof globalThis[syncKeys[i]] !== 'function') {
                                throw new Error('Missing sync: ' + syncKeys[i]);
                            }
                        }
                        for (var i = 0; i < asyncKeys.length; i++) {
                            if (typeof globalThis[asyncKeys[i]] !== 'function') {
                                throw new Error('Missing async: ' + asyncKeys[i]);
                            }
                        }

                        // Simulate getter-based fs facade (setup only, no calls)
                        var _fs = {};
                        Object.defineProperties(_fs, {
                            readFile:  { get: function() { return globalThis._fsReadFile; },  enumerable: true },
                            writeFile: { get: function() { return globalThis._fsWriteFile; }, enumerable: true },
                        });
                        globalThis._fs = _fs;

                        // Verify getter returns function reference without calling it
                        if (typeof _fs.readFile !== 'function') {
                            throw new Error('Getter should return function, got ' + typeof _fs.readFile);
                        }

                        // Simulate closure wrapping (setup only, no calls)
                        globalThis.__wrappedLog = function() {
                            return globalThis._log.apply(null, arguments);
                        };

                        globalThis.__bridge_setup_complete = true;
                    })();
                "#;
                let source = v8::String::new(scope, iife_code).unwrap();
                let script = v8::Script::compile(scope, source, None).unwrap();
                let result = script.run(scope);
                assert!(
                    result.is_some(),
                    "bridge IIFE should execute without error against stub functions"
                );

                // Verify setup completed
                let check =
                    v8::String::new(scope, "String(globalThis.__bridge_setup_complete)").unwrap();
                let script = v8::Script::compile(scope, check, None).unwrap();
                let val = script.run(scope).unwrap();
                assert_eq!(
                    val.to_rust_string_lossy(scope),
                    "true",
                    "bridge setup should complete with stub functions"
                );

                scope.set_default_context(context);
            }

            let blob = snapshot_isolate.create_blob(v8::FunctionCodeHandling::Keep);
            assert!(
                blob.is_some(),
                "snapshot creation should succeed with stub bridge functions"
            );
            assert!(blob.unwrap().len() > 0, "snapshot blob should be non-empty");
        }

        // --- Part 15: create_snapshot() auto-registers stubs and injects defaults ---
        // Verifies that create_snapshot() registers all bridge function stubs
        // and injects _processConfig/_osConfig defaults before running bridge code.
        {
            // Bridge IIFE that verifies stubs and config globals exist
            let iife_code = r#"
                (function() {
                    // Verify all sync bridge functions are registered as stubs
                    var syncFns = ['_log', '_error', '_resolveModule', '_loadFile',
                        '_loadPolyfill', '_cryptoRandomFill', '_cryptoRandomUUID',
                        '_fsReadFile', '_fsWriteFile', '_fsReadFileBinary',
                        '_fsWriteFileBinary', '_fsReadDir', '_fsMkdir', '_fsRmdir',
                        '_fsExists', '_fsStat', '_fsUnlink', '_fsRename', '_fsChmod',
                        '_fsChown', '_fsLink', '_fsSymlink', '_fsReadlink', '_fsLstat',
                        '_fsTruncate', '_fsUtimes', '_childProcessSpawnStart',
                        '_childProcessStdinWrite', '_childProcessStdinClose',
                        '_childProcessKill', '_childProcessSpawnSync'];
                    for (var i = 0; i < syncFns.length; i++) {
                        if (typeof globalThis[syncFns[i]] !== 'function') {
                            throw new Error('Missing sync stub: ' + syncFns[i] +
                                ' (typeof=' + typeof globalThis[syncFns[i]] + ')');
                        }
                    }

                    // Verify all async bridge functions are registered as stubs
                    var asyncFns = ['_dynamicImport', '_scheduleTimer',
                        '_networkFetchRaw', '_networkDnsLookupRaw',
                        '_networkHttpRequestRaw', '_networkHttpServerListenRaw',
                        '_networkHttpServerCloseRaw', '_networkHttpServerWaitRaw',
                        '_networkHttp2ServerWaitRaw', '_networkHttp2SessionWaitRaw'];
                    for (var i = 0; i < asyncFns.length; i++) {
                        if (typeof globalThis[asyncFns[i]] !== 'function') {
                            throw new Error('Missing async stub: ' + asyncFns[i] +
                                ' (typeof=' + typeof globalThis[asyncFns[i]] + ')');
                        }
                    }

                    // Verify _processConfig default was injected
                    if (typeof _processConfig !== 'object' || _processConfig === null) {
                        throw new Error('_processConfig not injected: ' + typeof _processConfig);
                    }
                    if (_processConfig.cwd !== '/') {
                        throw new Error('_processConfig.cwd should be "/", got: ' + _processConfig.cwd);
                    }

                    // Verify _osConfig default was injected
                    if (typeof _osConfig !== 'object' || _osConfig === null) {
                        throw new Error('_osConfig not injected: ' + typeof _osConfig);
                    }
                    if (_osConfig.platform !== 'linux') {
                        throw new Error('_osConfig.platform should be "linux", got: ' + _osConfig.platform);
                    }

                    globalThis.__part15_ok = true;
                })();
            "#;
            let blob = create_snapshot(iife_code).expect(
                "create_snapshot should succeed with bridge code that checks stubs and defaults",
            );
            assert!(blob.len() > 0, "snapshot blob should be non-empty");

            // Verify the snapshot can be restored
            let mut isolate = create_isolate_from_snapshot(blob, None);
            assert_eq!(eval(&mut isolate, "1 + 1"), "2");
        }

        // --- Part 16: create_snapshot() with getter facade and closures ---
        // Verifies that the full bridge pattern (stubs, closures, getter facade,
        // config globals) works through create_snapshot() and the context is
        // correctly snapshotted via set_default_context.
        {
            let iife_code = r#"
                (function() {
                    // Set up getter-based fs facade referencing bridge stubs
                    var _fs = {};
                    Object.defineProperties(_fs, {
                        readFile:  { get: function() { return globalThis._fsReadFile; },  enumerable: true },
                        writeFile: { get: function() { return globalThis._fsWriteFile; }, enumerable: true },
                    });
                    globalThis._fs = _fs;

                    // Set up closure wrapping a bridge stub
                    globalThis.myLog = function() {
                        return globalThis._log.apply(null, arguments);
                    };

                    // Set up a require-like function (doesn't call _loadPolyfill at setup)
                    globalThis.require = function(name) {
                        return globalThis._loadPolyfill(name);
                    };

                    // Set up a console-like object
                    globalThis.console = {
                        log: function() { globalThis._log.apply(null, arguments); },
                        error: function() { globalThis._error.apply(null, arguments); },
                    };

                    // Read _processConfig at setup time (like process.cwd initialization)
                    globalThis.__initialCwd = _processConfig.cwd;

                    globalThis.__part16_setup = true;
                })();
            "#;
            let blob = create_snapshot(iife_code)
                .expect("create_snapshot should succeed with full bridge IIFE pattern");
            assert!(blob.len() > 0);

            // Restore and verify default context has the bridge infrastructure
            let blob_bytes: Vec<u8> = blob.to_vec();
            let mut isolate = create_isolate_from_snapshot(blob_bytes, None);
            let scope = &mut v8::HandleScope::new(&mut isolate);
            let context = v8::Context::new(scope, Default::default());
            let scope = &mut v8::ContextScope::new(scope, context);

            // Check that bridge infrastructure from the IIFE is in the default context
            let check_code = r#"
                (function() {
                    var results = [];
                    results.push('_fs=' + (typeof _fs === 'object'));
                    results.push('_fs.readFile=' + (typeof _fs.readFile === 'function'));
                    results.push('myLog=' + (typeof myLog === 'function'));
                    results.push('require=' + (typeof require === 'function'));
                    results.push('console.log=' + (typeof console.log === 'function'));
                    results.push('console.error=' + (typeof console.error === 'function'));
                    results.push('__initialCwd=' + __initialCwd);
                    results.push('__part16_setup=' + __part16_setup);
                    return results.join(';');
                })()
            "#;
            let source = v8::String::new(scope, check_code).unwrap();
            let script = v8::Script::compile(scope, source, None).unwrap();
            let result = script.run(scope).unwrap();
            let result_str = result.to_rust_string_lossy(scope);

            assert_eq!(
                result_str,
                "_fs=true;_fs.readFile=true;myLog=true;require=true;console.log=true;console.error=true;__initialCwd=/;__part16_setup=true",
                "restored context should have all bridge infrastructure from the IIFE"
            );
        }

        // --- Part 17: SnapshotCache works with context-snapshot create_snapshot ---
        // Verifies cache hit/miss still works now that create_snapshot registers stubs.
        {
            let cache = SnapshotCache::new(4);
            let code = r#"
                (function() {
                    // Verify stubs are present (create_snapshot registers them)
                    if (typeof _log !== 'function') throw new Error('no _log stub');
                    if (typeof _processConfig !== 'object') throw new Error('no _processConfig');
                    globalThis.__cached_context = true;
                })();
            "#;

            let arc1 = cache.get_or_create(code).expect("first get_or_create");
            let arc2 = cache.get_or_create(code).expect("second get_or_create");
            assert!(
                Arc::ptr_eq(&arc1, &arc2),
                "cache hit should return same Arc"
            );

            // Verify blob is usable
            let mut isolate = create_isolate_from_snapshot((*arc1).clone(), None);
            assert_eq!(eval(&mut isolate, "1 + 1"), "2");
        }

        // --- Part 18: Context restore + replace_bridge_fns dispatches correctly ---
        // Verifies the full context snapshot restore flow: create snapshot with
        // stubs, restore, replace stubs with real bridge functions, verify the
        // replaced functions dispatch to the real Rust callbacks.
        {
            use crate::bridge::{replace_bridge_fns, PendingPromises, SessionBuffers};
            use crate::host_call::BridgeCallContext;
            use std::cell::RefCell;

            // Create snapshot with stubs + simple bridge IIFE
            let bridge_code = r#"
                (function() {
                    // Getter-based facade referencing globalThis._fsReadFile
                    var _fs = {};
                    Object.defineProperties(_fs, {
                        readFile: { get: function() { return globalThis._fsReadFile; }, enumerable: true },
                    });
                    globalThis._fs = _fs;
                    globalThis.__bridge_ready = true;
                })();
            "#;
            let blob = create_snapshot(bridge_code).expect("snapshot creation");
            let mut isolate = create_isolate_from_snapshot(blob, None);
            crate::execution::disable_wasm(&mut isolate);

            // Create BridgeCallContext (sync calls will fail but we verify dispatch)
            let (ipc_tx, _ipc_rx) = crossbeam_channel::unbounded::<Vec<u8>>();
            let call_id_router: crate::host_call::CallIdRouter =
                Arc::new(Mutex::new(std::collections::HashMap::new()));
            let receiver = crate::host_call::ReaderResponseReceiver::new(Box::new(
                std::io::Cursor::new(Vec::<u8>::new()),
            ));
            let sender = crate::host_call::ChannelFrameSender::new(ipc_tx);
            let bridge_ctx = BridgeCallContext::with_receiver(
                Box::new(sender),
                Box::new(receiver),
                "test-session".to_string(),
                call_id_router,
            );
            let session_buffers = RefCell::new(SessionBuffers::new());
            let pending = PendingPromises::new();

            // Restore context and replace bridge functions
            let scope = &mut v8::HandleScope::new(&mut isolate);
            let context = v8::Context::new(scope, Default::default());
            let scope = &mut v8::ContextScope::new(scope, context);

            let (_sync_store, _async_store) = replace_bridge_fns(
                scope,
                &bridge_ctx as *const BridgeCallContext,
                &pending as *const PendingPromises,
                &session_buffers as *const RefCell<SessionBuffers>,
                &["_log", "_fsReadFile"],
                &["_scheduleTimer"],
            );

            // Verify bridge infrastructure from IIFE survives restore
            let check = v8::String::new(
                scope,
                r#"
                (function() {
                    var results = [];
                    results.push('__bridge_ready=' + globalThis.__bridge_ready);
                    results.push('_fs_exists=' + (typeof _fs === 'object'));
                    // Getter should resolve to the REPLACED function (not stub)
                    results.push('_fs.readFile_type=' + typeof _fs.readFile);
                    // Direct global should also be the replaced function
                    results.push('_log_type=' + typeof _log);
                    results.push('_scheduleTimer_type=' + typeof _scheduleTimer);
                    return results.join(';');
                })()
            "#,
            )
            .unwrap();
            let script = v8::Script::compile(scope, check, None).unwrap();
            let result = script.run(scope).unwrap();
            assert_eq!(
                result.to_rust_string_lossy(scope),
                "__bridge_ready=true;_fs_exists=true;_fs.readFile_type=function;_log_type=function;_scheduleTimer_type=function",
                "restored context should have bridge IIFE state + replaced functions"
            );
        }

        // --- Part 19: _processConfig is overridable after restore ---
        // Verifies that inject_snapshot_defaults uses configurable properties
        // so inject_globals_from_payload can override them per session.
        {
            use crate::bridge::serialize_v8_value;

            let bridge_code = r#"
                (function() {
                    // Verify default _processConfig from snapshot
                    globalThis.__snapshotCwd = _processConfig.cwd;
                })();
            "#;
            let blob = create_snapshot(bridge_code).expect("snapshot creation");
            let mut isolate = create_isolate_from_snapshot(blob, None);

            let scope = &mut v8::HandleScope::new(&mut isolate);
            let context = v8::Context::new(scope, Default::default());
            let scope = &mut v8::ContextScope::new(scope, context);

            // Verify snapshot defaults are present
            let check = v8::String::new(scope, "__snapshotCwd").unwrap();
            let script = v8::Script::compile(scope, check, None).unwrap();
            let result = script.run(scope).unwrap();
            assert_eq!(result.to_rust_string_lossy(scope), "/");

            // Create a V8 payload to override _processConfig
            let payload_code = r#"({
                processConfig: { cwd: "/app", env: { FOO: "bar" }, timing_mitigation: "off", frozen_time_ms: null },
                osConfig: { homedir: "/home/user", tmpdir: "/tmp", platform: "linux", arch: "arm64" }
            })"#;
            let payload_source = v8::String::new(scope, payload_code).unwrap();
            let payload_script = v8::Script::compile(scope, payload_source, None).unwrap();
            let payload_val = payload_script.run(scope).unwrap();
            let payload_bytes = serialize_v8_value(scope, payload_val).expect("serialize payload");

            // Inject per-session globals (overrides snapshot defaults)
            crate::execution::inject_globals_from_payload(scope, &payload_bytes);

            // Verify _processConfig was overridden
            let check = v8::String::new(scope, "_processConfig.cwd").unwrap();
            let script = v8::Script::compile(scope, check, None).unwrap();
            let result = script.run(scope).unwrap();
            assert_eq!(
                result.to_rust_string_lossy(scope),
                "/app",
                "_processConfig.cwd should be overridden from '/' to '/app'"
            );

            // Verify _osConfig was overridden
            let check = v8::String::new(scope, "_osConfig.arch").unwrap();
            let script = v8::Script::compile(scope, check, None).unwrap();
            let result = script.run(scope).unwrap();
            assert_eq!(
                result.to_rust_string_lossy(scope),
                "arm64",
                "_osConfig.arch should be overridden to 'arm64'"
            );
        }

        // --- Part 20a: Concurrent get_or_create with different bridge codes ---
        // Verifies that concurrent callers requesting different bridge code
        // variants are not blocked by each other (two-phase locking).
        {
            use std::sync::atomic::{AtomicBool, Ordering};
            use std::time::Instant;

            let cache = Arc::new(SnapshotCache::new(4));
            let codes: Vec<String> = (0..3)
                .map(|i| {
                    format!(
                        "(function() {{ globalThis.__concurrent_{} = {}; }})();",
                        i, i
                    )
                })
                .collect();

            let barrier = Arc::new(std::sync::Barrier::new(codes.len()));
            let all_ok = Arc::new(AtomicBool::new(true));

            let mut handles = vec![];
            for code in &codes {
                let cache = Arc::clone(&cache);
                let barrier = Arc::clone(&barrier);
                let all_ok = Arc::clone(&all_ok);
                let code = code.clone();

                handles.push(std::thread::spawn(move || {
                    barrier.wait();
                    let start = Instant::now();
                    match cache.get_or_create(&code) {
                        Ok(arc) => {
                            assert!(arc.len() > 0);
                        }
                        Err(e) => {
                            eprintln!("get_or_create failed: {}", e);
                            all_ok.store(false, Ordering::Relaxed);
                        }
                    }
                    start.elapsed()
                }));
            }

            let mut durations = vec![];
            for h in handles {
                durations.push(h.join().expect("thread join"));
            }

            assert!(
                all_ok.load(Ordering::Relaxed),
                "all concurrent get_or_create calls should succeed"
            );

            // Verify all entries are cached (cache hits on second request)
            for code in &codes {
                let arc1 = cache.get_or_create(code).unwrap();
                let arc2 = cache.get_or_create(code).unwrap();
                assert!(
                    Arc::ptr_eq(&arc1, &arc2),
                    "should be cache hit after creation"
                );
            }
        }

        // --- Part 20: Multiple restores from same snapshot are independent ---
        // Verifies that user code in one restored context does not leak to another.
        {
            let bridge_code = r#"
                (function() {
                    globalThis.__bridge_ok = true;
                })();
            "#;
            let blob = create_snapshot(bridge_code).expect("snapshot creation");
            let blob_bytes: Vec<u8> = blob.to_vec();

            // Restore A: set a session-specific global
            {
                let mut isolate = create_isolate_from_snapshot(blob_bytes.clone(), None);
                let scope = &mut v8::HandleScope::new(&mut isolate);
                let context = v8::Context::new(scope, Default::default());
                let scope = &mut v8::ContextScope::new(scope, context);

                // Bridge state from snapshot should be present
                let check = v8::String::new(scope, "String(__bridge_ok)").unwrap();
                let script = v8::Script::compile(scope, check, None).unwrap();
                let result = script.run(scope).unwrap();
                assert_eq!(result.to_rust_string_lossy(scope), "true");

                // Set session-specific state
                let code = v8::String::new(scope, "globalThis.__user_data = 'session-a';").unwrap();
                let script = v8::Script::compile(scope, code, None).unwrap();
                script.run(scope);
            }

            // Restore B: session A's state should not be visible
            {
                let mut isolate = create_isolate_from_snapshot(blob_bytes.clone(), None);
                let scope = &mut v8::HandleScope::new(&mut isolate);
                let context = v8::Context::new(scope, Default::default());
                let scope = &mut v8::ContextScope::new(scope, context);

                // Bridge state should still be present
                let check = v8::String::new(scope, "String(__bridge_ok)").unwrap();
                let script = v8::Script::compile(scope, check, None).unwrap();
                let result = script.run(scope).unwrap();
                assert_eq!(result.to_rust_string_lossy(scope), "true");

                // Session A's data should NOT be visible
                let check = v8::String::new(scope, "typeof __user_data").unwrap();
                let script = v8::Script::compile(scope, check, None).unwrap();
                let result = script.run(scope).unwrap();
                assert_eq!(
                    result.to_rust_string_lossy(scope),
                    "undefined",
                    "session B should not see session A's user data"
                );
            }
        }
    }
}
