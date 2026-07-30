// V8 startup snapshots: fast isolate creation from pre-compiled bridge code

use std::collections::HashMap;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};

use sha2::{Digest, Sha256};

use crate::bridge::{external_refs, register_stub_bridge_fns};
use crate::isolate::init_v8_platform;
use crate::session::{async_bridge_fns, sync_bridge_fns};

/// Maximum allowed snapshot blob size (50MB).
/// Prevents resource exhaustion from degenerate bridge code.
const MAX_SNAPSHOT_BLOB_BYTES: usize = 50 * 1024 * 1024;
const MAX_V8_BRIDGE_CODE_BYTES: usize = 16 * 1024 * 1024;
/// Userland (agent-SDK) bundles are whole dependency graphs flattened into one
/// IIFE, so they are larger than the bridge. Bounded so a degenerate bundle cannot
/// exhaust memory, but generous enough for a real SDK (the pi bundle is ~7.6 MB).
const MAX_V8_USERLAND_CODE_BYTES: usize = 32 * 1024 * 1024;
pub(crate) const V8_BRIDGE_CODE_LIMIT_ERROR_CODE: &str = "ERR_V8_BRIDGE_CODE_LIMIT";
pub(crate) const V8_USERLAND_CODE_LIMIT_ERROR_CODE: &str = "ERR_V8_USERLAND_CODE_LIMIT";

const SNAPSHOT_HELPER_ENV: &str = "AGENTOS_V8_SNAPSHOT_HELPER";
const SNAPSHOT_HELPER_MAGIC: &[u8; 8] = b"SEV8SNP1";
const SNAPSHOT_HELPER_OK: &[u8; 2] = b"OK";
const SNAPSHOT_HELPER_ERR: &[u8; 3] = b"ERR";
const SNAPSHOT_HELPER_NONE_LEN: u64 = u64::MAX;

#[cfg(any(target_os = "linux", target_os = "android"))]
#[used]
#[link_section = ".init_array"]
static SNAPSHOT_HELPER_CTOR: extern "C" fn() = snapshot_helper_ctor;

// Same pre-main hook on macOS: the darwin sidecar build ships this crate, and
// without the constructor the helper child would fall through to the normal
// main and the parent's snapshot request would fail.
#[cfg(target_os = "macos")]
#[used]
#[link_section = "__DATA,__mod_init_func"]
static SNAPSHOT_HELPER_CTOR: extern "C" fn() = snapshot_helper_ctor;

#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
extern "C" fn snapshot_helper_ctor() {
    if std::env::var_os(SNAPSHOT_HELPER_ENV).is_some() {
        let code = run_snapshot_helper_from_stdio();
        std::process::exit(code);
    }
}

pub(crate) fn validate_bridge_code_size(bridge_code: &str) -> Result<(), String> {
    if bridge_code.len() > MAX_V8_BRIDGE_CODE_BYTES {
        return Err(format!(
            "{V8_BRIDGE_CODE_LIMIT_ERROR_CODE}: bridge code too large for V8 bridge setup: {} bytes (max {})",
            bridge_code.len(),
            MAX_V8_BRIDGE_CODE_BYTES
        ));
    }

    Ok(())
}

pub(crate) fn validate_userland_code_size(userland_code: &str) -> Result<(), String> {
    if userland_code.len() > MAX_V8_USERLAND_CODE_BYTES {
        return Err(format!(
            "{V8_USERLAND_CODE_LIMIT_ERROR_CODE}: userland snapshot code too large: {} bytes (max {})",
            userland_code.len(),
            MAX_V8_USERLAND_CODE_BYTES
        ));
    }

    Ok(())
}

/// Runs after the bridge but before the userland (agent-SDK) IIFE during snapshot
/// creation. Replaces bridge-backed lazy getters that would dispatch a host call
/// (unavailable when bridge fns are stubs) with static, snapshot-safe values. These
/// are environment-identity values (not per-session config), so baking them is
/// correct; per-session config is still injected post-restore.
const SNAPSHOT_USERLAND_PREP: &str = r#"
(function () {
    // Agent-SDK bundles are esbuild IIFEs that expect a global CJS `require` for
    // their node-builtin imports, but the bridge exposes require only via module
    // wrappers. Bind one from the bridge's namespaced createRequire so the bundle's
    // __require resolves builtins (via the in-context loadBuiltinModule) during
    // snapshot eval; it also works post-restore (resolution flows through the real
    // bridge fns swapped in after restore).
    if (typeof globalThis.require === "undefined" &&
        typeof globalThis.__agentOsGuestCreateRequire === "function") {
        try {
            globalThis.require = globalThis.__agentOsGuestCreateRequire("/root/index.js");
        } catch (e) {}
    }
    // `process.versions` is a bridge-backed lazy getter: it derives `.node` from the
    // per-session `config2.version` and merges a host bridge call. During snapshot
    // creation the bridge fns are stubs AND the default `_processConfig` has no
    // `version`, so the live getter THROWS — an agent SDK that reads it at
    // module-init would fail. Rather than REPLACE the getter with a static value
    // (which would permanently shadow the real per-session version for every
    // restored session — they'd all read a frozen, fabricated identity), WRAP it:
    // defer to the live getter so that post-restore — once the real bridge fns and
    // per-session config are injected — sessions read accurate per-session versions,
    // and fall back to the static, snapshot-safe identity only while the live getter
    // throws (i.e. during snapshot creation, before any real config exists).
    if (typeof process !== "undefined" && process) {
        var staticVersions = { node: "20.0.0", v8: "12.0.0", uv: "1.0.0", modules: "115" };
        try {
            var __verDesc = Object.getOwnPropertyDescriptor(process, "versions");
            var __liveVersions = __verDesc && __verDesc.get;
            Object.defineProperty(process, "versions", {
                configurable: true,
                enumerable: true,
                get: function () {
                    if (__liveVersions) {
                        try {
                            var v = __liveVersions.call(this);
                            // A real per-session result has a node version; during
                            // snapshot creation the live getter throws before here.
                            if (v && typeof v === "object" && v.node) return v;
                        } catch (e) {}
                    }
                    return staticVersions;
                },
            });
        } catch (e) {}
    }
})();
"#;

/// Compile and run a Script in the snapshot-creation context, returning a
/// descriptive error (with the V8 exception message) on failure. `label`
/// identifies the phase (e.g. "bridge code" / "userland code") in error text.
fn run_snapshot_script(
    scope: &mut v8::HandleScope,
    code: &str,
    label: &str,
    eager_compile: bool,
) -> Result<(), String> {
    let try_catch = &mut v8::TryCatch::new(scope);
    let source = match v8::String::new(try_catch, code) {
        Some(source) => source,
        None => return Err(format!("failed to create V8 string for {label}")),
    };
    // NOTE(perf, measured 2026-07-02): EagerCompile moves cost into the snapshot
    // blob: user_code_execute dropped 8.9ms -> 7.9ms on the wasm-runner floor,
    // but isolate_new rose 4.0ms -> 7.4ms when isolate creation was still
    // per-exec. Parked warm workers now prepay snapshot deserialization off-path,
    // so only the userland script opts into eager compilation; bridge code and
    // userland prep stay lazy.
    let script = if eager_compile {
        let resource_name = v8::String::new(try_catch, label).unwrap();
        let origin = v8::ScriptOrigin::new(
            try_catch,
            resource_name.into(),
            0,
            0,
            false,
            0,
            None,
            false,
            false,
            false,
            None,
        );
        let mut source = v8::script_compiler::Source::new(source, Some(&origin));
        v8::script_compiler::compile(
            try_catch,
            &mut source,
            v8::script_compiler::CompileOptions::EagerCompile,
            v8::script_compiler::NoCacheReason::NoReason,
        )
    } else {
        v8::Script::compile(try_catch, source, None)
    };
    let Some(script) = script else {
        let message = try_catch
            .exception()
            .map(|exception| exception.to_rust_string_lossy(try_catch))
            .unwrap_or_else(|| format!("{label} compilation failed during snapshot creation"));
        return Err(format!(
            "{label} compilation failed during snapshot creation: {message}"
        ));
    };
    if script.run(try_catch).is_none() {
        let message = try_catch
            .exception()
            .map(|exception| exception.to_rust_string_lossy(try_catch))
            .unwrap_or_else(|| format!("{label} execution failed during snapshot creation"));
        return Err(format!(
            "{label} execution failed during snapshot creation: {message}"
        ));
    }
    Ok(())
}

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
pub fn create_snapshot(bridge_code: &str) -> Result<Vec<u8>, String> {
    create_snapshot_inner(bridge_code, None)
}

/// Create a V8 startup snapshot whose default context has BOTH the bridge
/// infrastructure AND an evaluated userland module graph (e.g. a bundled agent
/// SDK) captured into it.
///
/// `userland_code` is an IIFE (esbuild `format:'iife'`) that runs after the bridge
/// in the same context and publishes its evaluated exports on `globalThis`. Because
/// the bridge has already installed node-builtin polyfills and stub bridge fns at
/// this point, the userland code can `require`/reference them while it evaluates.
/// The whole post-evaluation heap is frozen into the blob, so restoring a fresh
/// isolate skips re-evaluating the SDK entirely — collapsing the per-session
/// module-load/eval tax. Per-session bridge fns and config are still swapped/injected
/// post-restore exactly as for the bridge-only snapshot.
pub fn create_snapshot_with_userland(
    bridge_code: &str,
    userland_code: &str,
) -> Result<Vec<u8>, String> {
    create_snapshot_inner(bridge_code, Some(userland_code))
}

fn create_snapshot_inner(
    bridge_code: &str,
    userland_code: Option<&str>,
) -> Result<Vec<u8>, String> {
    validate_bridge_code_size(bridge_code)?;
    if let Some(userland_code) = userland_code {
        validate_userland_code_size(userland_code)?;
    }

    create_snapshot_in_subprocess(bridge_code, userland_code)
}

fn create_snapshot_inner_in_process(
    bridge_code: &str,
    userland_code: Option<&str>,
) -> Result<Vec<u8>, String> {
    validate_bridge_code_size(bridge_code)?;
    if let Some(userland_code) = userland_code {
        validate_userland_code_size(userland_code)?;
    }

    init_v8_platform();
    crate::isolate::prepare_current_thread();

    crate::isolate::with_isolate_lifecycle_lock(|| {
        let mut isolate = v8::Isolate::snapshot_creator(Some(external_refs()), None);
        let bridge_result = {
            let scope = &mut v8::HandleScope::new(&mut isolate);
            let context = v8::Context::new(scope, Default::default());
            let scope = &mut v8::ContextScope::new(scope, context);

            // Register stub bridge functions so the IIFE can reference them.
            let sync_bridge_fns = sync_bridge_fns();
            let async_bridge_fns = async_bridge_fns();
            register_stub_bridge_fns(scope, sync_bridge_fns, async_bridge_fns);

            // Inject default config globals for bridge IIFE setup
            inject_snapshot_defaults(scope);

            // Compile and run bridge code — context captures fully-initialized state.
            // Then, if present, run the userland (agent-SDK) IIFE in the SAME context so
            // its evaluated graph is captured alongside the bridge.
            let result = (|| -> Result<(), String> {
                run_snapshot_script(scope, bridge_code, "bridge code", false)?;
                if let Some(userland_code) = userland_code {
                    // Some bridge-backed globals (e.g. `process.versions`) are lazy
                    // getters that dispatch a host bridge call on first access. During
                    // snapshot creation the bridge fns are stubs, so an agent SDK that
                    // reads them at module-init would fail. Freeze them to static values
                    // first; per-session config is still injected post-restore.
                    run_snapshot_script(scope, SNAPSHOT_USERLAND_PREP, "userland prep", false)?;
                    run_snapshot_script(scope, userland_code, "userland code", true)?;
                }
                Ok(())
            })();

            scope.set_default_context(context);
            result
        };
        let blob = isolate
            .create_blob(v8::FunctionCodeHandling::Keep)
            .ok_or_else(|| "V8 snapshot creation failed".to_string())?;
        bridge_result?;

        // Reject oversized snapshots
        if blob.len() > MAX_SNAPSHOT_BLOB_BYTES {
            return Err(format!(
                "snapshot blob too large: {} bytes (max {})",
                blob.len(),
                MAX_SNAPSHOT_BLOB_BYTES
            ));
        }

        Ok(blob.to_vec())
    })
}

fn create_snapshot_in_subprocess(
    bridge_code: &str,
    userland_code: Option<&str>,
) -> Result<Vec<u8>, String> {
    let current_exe =
        std::env::current_exe().map_err(|error| format!("snapshot helper path: {error}"))?;
    let mut child = Command::new(current_exe)
        .env(SNAPSHOT_HELPER_ENV, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("spawn snapshot helper: {error}"))?;

    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| String::from("snapshot helper stdin unavailable"))?;
        write_snapshot_helper_request(&mut stdin, bridge_code, userland_code)?;
    }

    let output = child
        .wait_with_output()
        .map_err(|error| format!("wait for snapshot helper: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "snapshot helper exited with status {}: {}",
            output.status, stderr
        ));
    }

    parse_snapshot_helper_response(&output.stdout, &output.stderr)
}

fn write_snapshot_helper_request(
    writer: &mut impl Write,
    bridge_code: &str,
    userland_code: Option<&str>,
) -> Result<(), String> {
    writer
        .write_all(SNAPSHOT_HELPER_MAGIC)
        .map_err(|error| format!("write snapshot helper magic: {error}"))?;
    write_u64(writer, bridge_code.len() as u64)?;
    write_u64(
        writer,
        userland_code
            .map(|code| code.len() as u64)
            .unwrap_or(SNAPSHOT_HELPER_NONE_LEN),
    )?;
    writer
        .write_all(bridge_code.as_bytes())
        .map_err(|error| format!("write snapshot helper bridge code: {error}"))?;
    if let Some(userland_code) = userland_code {
        writer
            .write_all(userland_code.as_bytes())
            .map_err(|error| format!("write snapshot helper userland code: {error}"))?;
    }
    Ok(())
}

fn parse_snapshot_helper_response(stdout: &[u8], stderr: &[u8]) -> Result<Vec<u8>, String> {
    if stdout.starts_with(SNAPSHOT_HELPER_OK) {
        let payload = &stdout[SNAPSHOT_HELPER_OK.len()..];
        let (len, rest) = read_u64_from_slice(payload)?;
        let len = usize::try_from(len)
            .map_err(|_| String::from("snapshot helper OK payload length overflows usize"))?;
        if rest.len() != len {
            return Err(format!(
                "snapshot helper OK payload length mismatch: declared {}, got {}",
                len,
                rest.len()
            ));
        }
        return Ok(rest.to_vec());
    }

    if stdout.starts_with(SNAPSHOT_HELPER_ERR) {
        let payload = &stdout[SNAPSHOT_HELPER_ERR.len()..];
        let (len, rest) = read_u64_from_slice(payload)?;
        let len = usize::try_from(len)
            .map_err(|_| String::from("snapshot helper error length overflows usize"))?;
        if rest.len() != len {
            return Err(format!(
                "snapshot helper error length mismatch: declared {}, got {}",
                len,
                rest.len()
            ));
        }
        let message = String::from_utf8_lossy(rest);
        return Err(message.into_owned());
    }

    Err(format!(
        "snapshot helper returned invalid response (stdout {} bytes, stderr: {})",
        stdout.len(),
        String::from_utf8_lossy(stderr)
    ))
}

fn run_snapshot_helper_from_stdio() -> i32 {
    let mut input = Vec::new();
    if let Err(error) = std::io::stdin().read_to_end(&mut input) {
        let _ = write_snapshot_helper_error(format!("read snapshot helper request: {error}"));
        return 2;
    }

    let result = (|| -> Result<Vec<u8>, String> {
        let (bridge_code, userland_code) = parse_snapshot_helper_request(&input)?;
        create_snapshot_inner_in_process(&bridge_code, userland_code.as_deref())
    })();

    match result {
        Ok(blob) => match write_snapshot_helper_ok(&blob) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("write snapshot helper response: {error}");
                2
            }
        },
        Err(error) => match write_snapshot_helper_error(error) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("write snapshot helper error response: {error}");
                2
            }
        },
    }
}

fn parse_snapshot_helper_request(input: &[u8]) -> Result<(String, Option<String>), String> {
    let Some(rest) = input.strip_prefix(SNAPSHOT_HELPER_MAGIC) else {
        return Err(String::from("snapshot helper request missing magic"));
    };
    let (bridge_len, rest) = read_u64_from_slice(rest)?;
    let (userland_len, rest) = read_u64_from_slice(rest)?;
    let bridge_len = usize::try_from(bridge_len)
        .map_err(|_| String::from("snapshot helper bridge length overflows usize"))?;
    if rest.len() < bridge_len {
        return Err(format!(
            "snapshot helper bridge length mismatch: declared {}, got {}",
            bridge_len,
            rest.len()
        ));
    }
    let (bridge_bytes, rest) = rest.split_at(bridge_len);
    let userland_code = if userland_len == SNAPSHOT_HELPER_NONE_LEN {
        if !rest.is_empty() {
            return Err(format!(
                "snapshot helper request has {} trailing bytes after bridge-only payload",
                rest.len()
            ));
        }
        None
    } else {
        let userland_len = usize::try_from(userland_len)
            .map_err(|_| String::from("snapshot helper userland length overflows usize"))?;
        if rest.len() != userland_len {
            return Err(format!(
                "snapshot helper userland length mismatch: declared {}, got {}",
                userland_len,
                rest.len()
            ));
        }
        Some(
            String::from_utf8(rest.to_vec())
                .map_err(|error| format!("snapshot helper userland is not UTF-8: {error}"))?,
        )
    };
    let bridge_code = String::from_utf8(bridge_bytes.to_vec())
        .map_err(|error| format!("snapshot helper bridge is not UTF-8: {error}"))?;
    Ok((bridge_code, userland_code))
}

fn write_snapshot_helper_ok(blob: &[u8]) -> std::io::Result<()> {
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(SNAPSHOT_HELPER_OK)?;
    write_u64_io(&mut stdout, blob.len() as u64)?;
    stdout.write_all(blob)?;
    stdout.flush()
}

fn write_snapshot_helper_error(message: String) -> std::io::Result<()> {
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(SNAPSHOT_HELPER_ERR)?;
    write_u64_io(&mut stdout, message.len() as u64)?;
    stdout.write_all(message.as_bytes())?;
    stdout.flush()
}

fn write_u64(writer: &mut impl Write, value: u64) -> Result<(), String> {
    write_u64_io(writer, value).map_err(|error| format!("write snapshot helper length: {error}"))
}

fn write_u64_io(writer: &mut impl Write, value: u64) -> std::io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

fn read_u64_from_slice(input: &[u8]) -> Result<(u64, &[u8]), String> {
    let bytes = input
        .get(..8)
        .ok_or_else(|| String::from("snapshot helper payload ended before u64"))?;
    let mut value = [0_u8; 8];
    value.copy_from_slice(bytes);
    Ok((u64::from_le_bytes(value), &input[8..]))
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
        frozen_time_ms: null,
        high_resolution_time: false
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
    crate::isolate::prepare_current_thread();

    // `None` applies the bounded-by-default cap (`DEFAULT_HEAP_LIMIT_MB`), same as
    // the fresh-isolate path — a snapshot-restored isolate is never unbounded.
    let limit = heap_limit_mb.unwrap_or(crate::isolate::DEFAULT_HEAP_LIMIT_MB);
    let limit_bytes = (limit as usize) * 1024 * 1024;
    let params = v8::CreateParams::default()
        .snapshot_blob(blob)
        .external_references(&**external_refs())
        .heap_limits(0, limit_bytes);
    let mut isolate = crate::isolate::with_isolate_lifecycle_lock(|| v8::Isolate::new(params));
    crate::isolate::configure_isolate(&mut isolate);
    // Same OOM guard as the fresh-isolate path: terminate this isolate on heap
    // exhaustion instead of fatal-aborting the shared process (F-003).
    crate::isolate::install_heap_limit_guard(&mut isolate);
    isolate
}

pub type SnapshotCacheKey = [u8; 32];

/// Thread-safe snapshot cache keyed by bridge code digest.
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
    /// Per-key in-flight tracking: callers for the same digest wait on the
    /// condvar instead of creating duplicate snapshots.
    in_flight: HashMap<SnapshotCacheKey, Arc<InFlightEntry>>,
}

struct CacheEntry {
    key: SnapshotCacheKey,
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
        self.get_or_create_with_userland(bridge_code, None)
    }

    /// Return a cached snapshot if present. This never creates a snapshot or
    /// waits on in-flight creation.
    pub fn try_get_with_userland(
        &self,
        bridge_code: &str,
        userland_code: Option<&str>,
    ) -> Option<Arc<Vec<u8>>> {
        let key = snapshot_cache_key(bridge_code, userland_code);
        let mut inner = self.inner.lock().unwrap();
        if let Some(pos) = inner.entries.iter().position(|e| e.key == key) {
            let entry = inner.entries.remove(pos);
            let blob = Arc::clone(&entry.blob);
            inner.entries.push(entry);
            Some(blob)
        } else {
            None
        }
    }

    /// Like [`get_or_create`], but the snapshot also captures an evaluated userland
    /// (agent-SDK) graph. The cache key is the digest of BOTH `bridge_code` and
    /// `userland_code`, so a change to either — i.e. any change in the bundled
    /// dependency graph — invalidates the entry and triggers exactly one rebuild.
    pub fn get_or_create_with_userland(
        &self,
        bridge_code: &str,
        userland_code: Option<&str>,
    ) -> Result<Arc<Vec<u8>>, String> {
        let key = snapshot_cache_key(bridge_code, userland_code);

        // Phase 1: short lock — check cache, check in-flight, or claim creation
        let in_flight = {
            let mut inner = self.inner.lock().unwrap();

            // Cache hit — move to end (most recently used)
            if let Some(pos) = inner.entries.iter().position(|e| e.key == key) {
                let entry = inner.entries.remove(pos);
                let blob = Arc::clone(&entry.blob);
                inner.entries.push(entry);
                return Ok(blob);
            }

            // Another thread is already creating this snapshot — wait on it
            if let Some(entry) = inner.in_flight.get(&key) {
                Some(Arc::clone(entry))
            } else {
                // We're the creator — register in-flight and release the lock
                let entry = Arc::new(InFlightEntry {
                    result: Mutex::new(None),
                    done: Condvar::new(),
                });
                inner.in_flight.insert(key, Arc::clone(&entry));
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
        let creation_result = create_snapshot_inner(bridge_code, userland_code).map(Arc::new);

        // Phase 3: short lock — insert result, notify waiters, clean up
        {
            let mut inner = self.inner.lock().unwrap();

            if let Ok(ref arc) = creation_result {
                // LRU eviction: remove oldest (front) entry when at capacity
                if inner.entries.len() >= self.max_entries {
                    inner.entries.remove(0);
                }
                inner.entries.push(CacheEntry {
                    key,
                    blob: Arc::clone(arc),
                });
            }

            // Publish result to waiters and remove in-flight entry
            if let Some(entry) = inner.in_flight.remove(&key) {
                let mut result = entry.result.lock().unwrap();
                *result = Some(creation_result.clone());
                entry.done.notify_all();
            }
        }

        creation_result
    }
}

/// Cache key over bridge + optional userland code. With no userland this is just
/// the sha256 of the bridge code (a NUL separator is only added when userland is
/// present), so existing bridge-only entries keep their historical keys.
pub fn snapshot_cache_key(bridge_code: &str, userland_code: Option<&str>) -> SnapshotCacheKey {
    match userland_code {
        None => {
            let mut hasher = Sha256::new();
            hasher.update(bridge_code.as_bytes());
            hasher.finalize().into()
        }
        Some(userland_code) => {
            let mut buf = Vec::with_capacity(bridge_code.len() + 1 + userland_code.len());
            buf.extend_from_slice(bridge_code.as_bytes());
            buf.push(0);
            buf.extend_from_slice(userland_code.as_bytes());
            let mut hasher = Sha256::new();
            hasher.update(&buf);
            hasher.finalize().into()
        }
    }
}

#[doc(hidden)]
#[cfg(any(test, feature = "test-support"))]
pub fn run_snapshot_consolidated_checks() {
    fn eval(isolate: &mut v8::OwnedIsolate, code: &str) -> String {
        let scope = &mut v8::HandleScope::new(isolate);
        let context = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, context);
        let source = v8::String::new(scope, code).unwrap();
        let script = v8::Script::compile(scope, source, None).unwrap();
        let result = script.run(scope).unwrap();
        result.to_rust_string_lossy(scope)
    }

    // Keep snapshot coverage in a dedicated integration-test process.
    // Running it in the shared unit-test binary still triggers a V8 teardown
    // SIGSEGV after the test completes.
    init_v8_platform();
    let _ = external_refs();

    // --- Part 1: Snapshot creation returns non-empty blob ---
    {
        let bridge_code = "(function() { globalThis.__bridge_init = true; })();";
        let blob = create_snapshot(bridge_code).expect("snapshot creation should succeed");
        assert!(!blob.is_empty(), "snapshot blob should be non-empty");
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

    // --- Part 10: Guest WebAssembly remains available after snapshot restore ---
    {
        let bridge_code = "(function() { globalThis.__wasm_test = true; })();";
        let blob = create_snapshot(bridge_code).expect("snapshot creation");
        let mut isolate = create_isolate_from_snapshot(blob, None);

        let scope = &mut v8::HandleScope::new(&mut isolate);
        let context = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, context);

        let wasm_test_code = r#"
                (function() {
                    var bytes = new Uint8Array([
                        0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00,
                        0x01, 0x07, 0x01, 0x60, 0x02, 0x7f, 0x7f, 0x01, 0x7f,
                        0x03, 0x02, 0x01, 0x00,
                        0x07, 0x07, 0x01, 0x03, 0x61, 0x64, 0x64, 0x00, 0x00,
                        0x0a, 0x09, 0x01, 0x07, 0x00, 0x20, 0x00, 0x20, 0x01, 0x6a, 0x0b,
                    ]);
                    var module = new WebAssembly.Module(bytes);
                    var instance = new WebAssembly.Instance(module, {});
                    return String(instance.exports.add(2, 3));
                })()
            "#;
        let source = v8::String::new(scope, wasm_test_code).unwrap();
        let script = v8::Script::compile(scope, source, None).unwrap();
        let result = script.run(scope).unwrap();
        let result_str = result.to_rust_string_lossy(scope);

        assert_eq!(
            result_str, "5",
            "WASM should remain enabled after snapshot restore"
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
                v8::String::new(scope, "globalThis.__session_secret = 'session-a-data';").unwrap();
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
        use crate::bridge::{register_async_bridge_fns, register_sync_bridge_fns, PendingPromises};
        use crate::host_call::BridgeCallContext;

        let bridge_code = "(function() { globalThis.__ext_ref_test = true; })();";
        let blob = create_snapshot(bridge_code).expect("snapshot creation");
        let mut isolate = create_isolate_from_snapshot(blob, None);

        // Create minimal BridgeCallContext (sync call will fail but we
        // test that the FunctionTemplate dispatches without crash)
        let (event_tx, _event_rx) =
            crossbeam_channel::bounded::<crate::session::RuntimeEventEnvelope>(16);
        let (_cmd_tx, _cmd_rx) = crossbeam_channel::bounded::<crate::session::SessionCommand>(1);
        let call_id_router: crate::host_call::CallIdRouter =
            Arc::new(crate::host_call::BridgeCallRegistry::with_default_limit());

        let receiver = crate::host_call::ReaderBridgeResponseReceiver::new(Box::new(
            std::io::Cursor::new(Vec::<u8>::new()),
        ));
        let sender = crate::host_call::ChannelRuntimeEventSender::new(event_tx, None);
        let bridge_ctx = BridgeCallContext::with_receiver(
            Box::new(sender),
            Box::new(receiver),
            "test-session".to_string(),
            call_id_router,
            Arc::new(std::sync::atomic::AtomicU64::new(1)),
        );
        let pending = PendingPromises::new();

        let scope = &mut v8::HandleScope::new(&mut isolate);
        let context = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, context);

        // Register bridge functions on the restored isolate
        let _sync_store = register_sync_bridge_fns(
            scope,
            &bridge_ctx as *const BridgeCallContext,
            &["_testSync"],
        );
        let _async_store = register_async_bridge_fns(
            scope,
            &bridge_ctx as *const BridgeCallContext,
            &pending as *const PendingPromises,
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

            // Register bridge functions as stubs (no External data).
            let sync_bridge_fns = sync_bridge_fns();
            let async_bridge_fns = async_bridge_fns();
            register_stub_bridge_fns(scope, sync_bridge_fns, async_bridge_fns);

            // Simulate bridge IIFE: reference all bridge functions, set up
            // closures and getter facade, but never call any bridge function
            let iife_code = r#"
                    (function() {
                        // Verify bridge functions exist (like ivm-compat shim)
                        var syncKeys = ['_log', '_error', '_resolveModule', '_loadFile', '_moduleFormat',
                            '_cryptoRandomFill', '_fsReadFile', '_fsWriteFile',
                            '_childProcessSpawnStart', '_childProcessPoll', '_childProcessSpawnSync'];
                        var asyncKeys = ['_dynamicImport', '_scheduleTimer',
                            '_networkHttpServerListenRaw'];

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
        assert!(
            !blob.unwrap().is_empty(),
            "snapshot blob should be non-empty"
        );
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
                        '_moduleFormat', '_loadPolyfill', '_cryptoRandomFill', '_cryptoRandomUUID',
                        '_fsReadFile', '_fsWriteFile', '_fsReadFileBinary',
                        '_fsWriteFileBinary', '_fsReadDir', '_fsMkdir', '_fsRmdir',
                        '_fsExists', '_fsStat', '_fsUnlink', '_fsRename', '_fsChmod',
                        '_fsChown', '_fsLink', '_fsSymlink', '_fsReadlink', '_fsLstat',
                        '_fsTruncate', '_fsUtimes', '_childProcessSpawnStart',
                        '_childProcessPoll', '_childProcessStdinWrite', '_childProcessStdinClose',
                        '_childProcessKill', '_childProcessSpawnSync'];
                    for (var i = 0; i < syncFns.length; i++) {
                        if (typeof globalThis[syncFns[i]] !== 'function') {
                            throw new Error('Missing sync stub: ' + syncFns[i] +
                                ' (typeof=' + typeof globalThis[syncFns[i]] + ')');
                        }
                    }

                    // Verify all async bridge functions are registered as stubs
                    var asyncFns = ['_dynamicImport', '_scheduleTimer',
                        '_networkDnsLookupRaw',
                        '_networkDnsResolveRaw',
                        '_networkHttpServerListenRaw',
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
        assert!(!blob.is_empty(), "snapshot blob should be non-empty");

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
        assert!(!blob.is_empty());

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
        use crate::bridge::{replace_bridge_fns, PendingPromises};
        use crate::host_call::BridgeCallContext;

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

        // Create BridgeCallContext (sync calls will fail but we verify dispatch)
        let (event_tx, _event_rx) =
            crossbeam_channel::bounded::<crate::session::RuntimeEventEnvelope>(16);
        let call_id_router: crate::host_call::CallIdRouter =
            Arc::new(crate::host_call::BridgeCallRegistry::with_default_limit());
        let receiver = crate::host_call::ReaderBridgeResponseReceiver::new(Box::new(
            std::io::Cursor::new(Vec::<u8>::new()),
        ));
        let sender = crate::host_call::ChannelRuntimeEventSender::new(event_tx, None);
        let bridge_ctx = BridgeCallContext::with_receiver(
            Box::new(sender),
            Box::new(receiver),
            "test-session".to_string(),
            call_id_router,
            Arc::new(std::sync::atomic::AtomicU64::new(1)),
        );
        let pending = PendingPromises::new();

        // Restore context and replace bridge functions
        let scope = &mut v8::HandleScope::new(&mut isolate);
        let context = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, context);

        let (_sync_store, _async_store) = replace_bridge_fns(
            scope,
            &bridge_ctx as *const BridgeCallContext,
            &pending as *const PendingPromises,
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
                osConfig: { homedir: "/home/agentos", tmpdir: "/tmp", platform: "linux", arch: "arm64" }
            })"#;
        let payload_source = v8::String::new(scope, payload_code).unwrap();
        let payload_script = v8::Script::compile(scope, payload_source, None).unwrap();
        let payload_val = payload_script.run(scope).unwrap();
        let payload_bytes = serialize_v8_value(scope, payload_val).expect("serialize payload");

        // Inject per-session globals (overrides snapshot defaults)
        crate::execution::inject_globals_from_payload(scope, &payload_bytes)
            .expect("inject globals payload");

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

    // --- Part 19a: function globals survive snapshot restore ---
    {
        let bridge_code = r#"
                (function() {
                    globalThis.__snapshotFn = async function () { return "ok"; };
                })();
            "#;
        let blob = create_snapshot(bridge_code).expect("snapshot creation");
        let mut isolate = create_isolate_from_snapshot(blob, None);

        let scope = &mut v8::HandleScope::new(&mut isolate);
        let context = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, context);

        let check = v8::String::new(
            scope,
            r#"(function() {
                    return JSON.stringify({
                        fnType: typeof globalThis.__snapshotFn,
                        promiseType: typeof globalThis.__snapshotFn?.(),
                    });
                })()"#,
        )
        .unwrap();
        let script = v8::Script::compile(scope, check, None).unwrap();
        let result = script.run(scope).unwrap();
        assert_eq!(
            result.to_rust_string_lossy(scope),
            r#"{"fnType":"function","promiseType":"object"}"#,
            "function-valued globals should survive snapshot restore"
        );
    }

    // --- Part 19b: bundled bridge installs fetch globals before snapshot restore ---
    {
        let bridge_code = concat!(
            include_str!(concat!(env!("OUT_DIR"), "/v8-bridge.js")),
            "\n",
            include_str!(concat!(env!("OUT_DIR"), "/v8-bridge-zlib.js"))
        );
        let blob = create_snapshot(bridge_code).expect("snapshot creation");
        let mut isolate = create_isolate_from_snapshot(blob, None);

        let scope = &mut v8::HandleScope::new(&mut isolate);
        let context = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, context);

        let check = v8::String::new(
            scope,
            r#"(function() {
                    return JSON.stringify({
                        fetchType: typeof globalThis.fetch,
                        headersType: typeof globalThis.Headers,
                        requestType: typeof globalThis.Request,
                        responseType: typeof globalThis.Response,
                    });
                })()"#,
        )
        .unwrap();
        let script = v8::Script::compile(scope, check, None).unwrap();
        let result = script.run(scope).unwrap();
        assert_eq!(
            result.to_rust_string_lossy(scope),
            r#"{"fetchType":"function","headersType":"function","requestType":"function","responseType":"function"}"#,
            "bundled bridge should expose fetch globals in restored contexts"
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
                        assert!(!arc.is_empty());
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

    // --- Part 21: Userland snapshot — evaluated graph captured, ZERO re-eval on
    // restore, isolation preserved (2b acceptance). ---
    {
        // Evaluate a string in a fresh context on a restored isolate. Unlike the
        // function-level `eval`, this takes an existing ContextScope so successive
        // checks observe globals set by earlier scripts in the SAME context.
        fn run_in(scope: &mut v8::ContextScope<v8::HandleScope>, code: &str) -> String {
            let source = v8::String::new(scope, code).unwrap();
            let script = v8::Script::compile(scope, source, None).unwrap();
            let result = script.run(scope).unwrap();
            result.to_rust_string_lossy(scope)
        }

        // `userland` stands in for an esbuild IIFE bundle: it evaluates a small
        // module graph, references a bridge-provided global (proving the bridge is
        // available when userland runs), publishes exports on globalThis, and bumps
        // a side-effect counter so we can prove the top-level runs exactly once.
        let bridge_code = "(function(){ globalThis.__bridge_ok = true; })();";
        let userland = r#"
            (function () {
                if (typeof globalThis._fsReadFile !== "function") {
                    throw new Error("bridge fns missing during userland eval");
                }
                globalThis.__sideEffectCount = (globalThis.__sideEffectCount || 0) + 1;
                var secret = 42;
                globalThis.__x = { f: function () { return secret; } };
            })();
        "#;

        let blob = create_snapshot_with_userland(bridge_code, userland)
            .expect("userland snapshot creation should succeed");
        let blob_bytes: Vec<u8> = blob.to_vec();

        // Restore A: fresh isolate + fresh context cloned from the snapshot default
        // context. The userland top-level must NOT run again here.
        {
            let mut isolate = create_isolate_from_snapshot(blob_bytes.clone(), None);
            let scope = &mut v8::HandleScope::new(&mut isolate);
            let context = v8::Context::new(scope, Default::default());
            let scope = &mut v8::ContextScope::new(scope, context);

            assert_eq!(
                run_in(scope, "String(globalThis.__x.f())"),
                "42",
                "userland export __x.f() should return 42 from the snapshot"
            );
            assert_eq!(
                run_in(scope, "String(globalThis.__sideEffectCount)"),
                "1",
                "userland top-level must run exactly once (zero re-eval on restore)"
            );
            assert_eq!(
                run_in(scope, "String(globalThis.__bridge_ok)"),
                "true",
                "bridge state should coexist with userland state in the snapshot"
            );

            // Mutate a global in session A.
            run_in(scope, "globalThis.__leak = 'session-a'; ''");
        }

        // Restore B: a separate fresh isolate from the SAME blob must see the
        // captured userland state but NOT session A's mutation (isolation).
        {
            let mut isolate = create_isolate_from_snapshot(blob_bytes.clone(), None);
            let scope = &mut v8::HandleScope::new(&mut isolate);
            let context = v8::Context::new(scope, Default::default());
            let scope = &mut v8::ContextScope::new(scope, context);

            assert_eq!(
                run_in(scope, "String(globalThis.__x.f())"),
                "42",
                "session B should see the captured userland export"
            );
            assert_eq!(
                run_in(scope, "String(globalThis.__sideEffectCount)"),
                "1",
                "session B counter must still be 1 (no re-eval, no cross-session bump)"
            );
            assert_eq!(
                run_in(scope, "typeof globalThis.__leak"),
                "undefined",
                "session B must NOT observe session A's mutation"
            );
        }

        // Cache: identical (bridge, userland) → same Arc; changed userland → new Arc.
        {
            let cache = SnapshotCache::new(4);
            let a = cache
                .get_or_create_with_userland(bridge_code, Some(userland))
                .expect("userland cache create");
            let b = cache
                .get_or_create_with_userland(bridge_code, Some(userland))
                .expect("userland cache hit");
            assert!(
                Arc::ptr_eq(&a, &b),
                "identical userland should hit the cache"
            );

            let userland2 = "(function(){ globalThis.__x = { f: function(){ return 7; } }; })();";
            let c = cache
                .get_or_create_with_userland(bridge_code, Some(userland2))
                .expect("changed userland create");
            assert!(
                !Arc::ptr_eq(&a, &c),
                "changed userland (dep-graph change) should rebuild"
            );
        }
    }

    // --- Part 22: REAL agent-SDK bundle snapshots + restores (env-gated). ---
    // End-to-end primitive validation against the actual pi SDK snapshot bundle:
    // the real bridge bundle + the real esbuild IIFE evaluate together into the
    // snapshot, and a fresh restored isolate exposes the SDK runtime global with a
    // working createAgentSession. Gated on PI_SNAPSHOT_BUNDLE_PATH so CI without the
    // bundle skips it; run with that env var pointing at dist/pi-sdk-snapshot.js.
    if let Ok(bundle_path) = std::env::var("PI_SNAPSHOT_BUNDLE_PATH") {
        let userland = std::fs::read_to_string(&bundle_path)
            .unwrap_or_else(|e| panic!("read pi bundle at {bundle_path}: {e}"));
        let bridge_code = concat!(
            include_str!(concat!(env!("OUT_DIR"), "/v8-bridge.js")),
            "\n",
            include_str!(concat!(env!("OUT_DIR"), "/v8-bridge-zlib.js"))
        );

        let blob = create_snapshot_with_userland(bridge_code, &userland)
            .expect("real pi SDK bundle should snapshot cleanly (pure-JS, no top-level I/O)");
        let mut isolate = create_isolate_from_snapshot(blob, None);
        let scope = &mut v8::HandleScope::new(&mut isolate);
        let context = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, context);

        let check = v8::String::new(
            scope,
            "(function(){ var r = globalThis.__PI_SDK_RUNTIME__; \
             return r && typeof r.createAgentSession === 'function' && \
             typeof r.createAllTools === 'function' ? 'ok' : 'missing'; })()",
        )
        .unwrap();
        let script = v8::Script::compile(scope, check, None).unwrap();
        let result = script.run(scope).unwrap();
        assert_eq!(
            result.to_rust_string_lossy(scope),
            "ok",
            "restored isolate must expose the pi SDK runtime global from the snapshot"
        );
    }

    // --- Part 23: cross-thread snapshot build → restore (diagnoses pre-warm). ---
    // Build a userland snapshot on a SEPARATE spawned+joined thread, then restore and
    // eval it on the main thread. If V8 fundamentally forbids restoring a blob built
    // on a different thread (the suspected cause of the pre-warm wedge), this aborts.
    {
        let bridge_code = "(function(){ globalThis.__xt_bridge = true; })();";
        let userland = "(function(){ globalThis.__xt = { f: function(){ return 99; } }; })();";
        let blob_bytes: Vec<u8> = std::thread::spawn(move || {
            create_snapshot_with_userland(bridge_code, userland)
                .expect("cross-thread snapshot build should succeed")
                .to_vec()
        })
        .join()
        .expect("build thread join");

        let mut isolate = create_isolate_from_snapshot(blob_bytes, None);
        let scope = &mut v8::HandleScope::new(&mut isolate);
        let context = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, context);
        let check = v8::String::new(scope, "String(globalThis.__xt.f())").unwrap();
        let script = v8::Script::compile(scope, check, None).unwrap();
        let result = script.run(scope).unwrap();
        assert_eq!(
            result.to_rust_string_lossy(scope),
            "99",
            "a snapshot built on another thread must restore correctly on this thread"
        );
    }

    // --- Part 24: session-level isolation (global AND prototype). ---
    // Each agent session leases a FRESH context cloned from the same snapshot's
    // default context. This is the isolation unit: a global or built-in-prototype
    // mutation in "session A" must NOT be observable in "session B".
    {
        let bridge_code = "(function(){ globalThis.__iso_ok = true; })();";
        let userland = "(function(){ globalThis.__sdk = { v: 1 }; })();";
        let blob_bytes: Vec<u8> = create_snapshot_with_userland(bridge_code, userland)
            .expect("isolation snapshot")
            .to_vec();

        // Session A: mutate a global, the captured SDK object, AND a built-in prototype.
        {
            let mut isolate = create_isolate_from_snapshot(blob_bytes.clone(), None);
            let scope = &mut v8::HandleScope::new(&mut isolate);
            let context = v8::Context::new(scope, Default::default());
            let scope = &mut v8::ContextScope::new(scope, context);
            let src = v8::String::new(
                scope,
                "globalThis.__leakG = 'A'; globalThis.__sdk.v = 999; \
                 Array.prototype.__leakP = 'A'; ''",
            )
            .unwrap();
            let script = v8::Script::compile(scope, src, None).unwrap();
            script.run(scope);
        }

        // Session B: a separate fresh context from the SAME blob sees the captured
        // snapshot state but NONE of session A's mutations.
        {
            let mut isolate = create_isolate_from_snapshot(blob_bytes.clone(), None);
            let scope = &mut v8::HandleScope::new(&mut isolate);
            let context = v8::Context::new(scope, Default::default());
            let scope = &mut v8::ContextScope::new(scope, context);
            let check = v8::String::new(
                scope,
                "(function(){ return [ \
                   String(globalThis.__iso_ok), \
                   typeof globalThis.__leakG, \
                   String(globalThis.__sdk.v), \
                   typeof ([].__leakP) \
                 ].join(','); })()",
            )
            .unwrap();
            let script = v8::Script::compile(scope, check, None).unwrap();
            let result = script.run(scope).unwrap();
            assert_eq!(
                result.to_rust_string_lossy(scope),
                "true,undefined,1,undefined",
                "session B must see snapshot state but NOT session A's global/SDK/prototype mutations"
            );
        }
    }

    // --- Part 25: H-1 regression — the userland-prep `process.versions` wrapper
    // DEFERS to the bridge's live getter post-restore instead of freezing a static
    // identity. ---
    // `SNAPSHOT_USERLAND_PREP` wraps (does not replace) the bridge's lazy
    // `process.versions` getter: during snapshot creation the live getter throws
    // (bridge fns are stubs) so a static identity is used, but post-restore — once
    // the real bridge fns + per-session config are injected — sessions must read the
    // LIVE per-session value. A plain static pin (the prior bug) permanently shadowed
    // it. This models the deferral with a getter whose result depends on the
    // post-restore-injected `_processConfig`, so a regression back to a static pin
    // fails here.
    {
        use crate::bridge::serialize_v8_value;

        // Bridge installs a lazy `process.versions` getter that derives its value from
        // the per-session `_processConfig` (resolved live on each access). During
        // snapshot creation `_processConfig` is the default (no `version`), so the
        // getter throws — exactly the case the prep wraps. A userland is required so
        // SNAPSHOT_USERLAND_PREP runs.
        let bridge_code = r#"
            (function () {
                globalThis.process = {
                    get versions() {
                        // Throws during creation (default _processConfig has no
                        // version); returns the live per-session value post-restore.
                        return { node: _processConfig.version.replace(/^v/, "") };
                    },
                };
            })();
        "#;
        let userland = "(function(){ globalThis.__sdk_ready = true; })();";
        let blob = create_snapshot_with_userland(bridge_code, userland)
            .expect("userland snapshot with a lazy process.versions getter");

        let mut isolate = create_isolate_from_snapshot(blob, None);
        let scope = &mut v8::HandleScope::new(&mut isolate);
        let context = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, context);

        // Inject a per-session config carrying a distinctive version (as the sidecar
        // does post-restore via inject_globals_from_payload).
        let payload_code = r#"({
            processConfig: { cwd: "/", env: {}, version: "v99.1.2", timing_mitigation: "off", frozen_time_ms: null },
            osConfig: { homedir: "/root", tmpdir: "/tmp", platform: "linux", arch: "x64" }
        })"#;
        let payload_source = v8::String::new(scope, payload_code).unwrap();
        let payload_script = v8::Script::compile(scope, payload_source, None).unwrap();
        let payload_val = payload_script.run(scope).unwrap();
        let payload_bytes = serialize_v8_value(scope, payload_val).expect("serialize payload");
        crate::execution::inject_globals_from_payload(scope, &payload_bytes)
            .expect("inject per-session config");

        // The wrapper must defer to the live getter → per-session "99.1.2", NOT the
        // snapshot-build-time static identity "20.0.0".
        let check = v8::String::new(scope, "String(process.versions.node)").unwrap();
        let script = v8::Script::compile(scope, check, None).unwrap();
        let result = script.run(scope).unwrap();
        assert_eq!(
            result.to_rust_string_lossy(scope),
            "99.1.2",
            "process.versions must defer to the live per-session getter post-restore, \
             not the frozen snapshot-build-time static identity (H-1 regression)"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_cache_key_uses_full_sha256_digest() {
        // With no userland the key is the plain sha256 of the bridge code, so
        // bridge-only snapshot entries keep their historical keys.
        assert_eq!(
            snapshot_cache_key("abc", None),
            [
                0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
                0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
                0xf2, 0x00, 0x15, 0xad,
            ]
        );
    }

    #[test]
    fn create_snapshot_rejects_oversized_bridge_code_before_v8_creation() {
        let bridge_code = " ".repeat(MAX_V8_BRIDGE_CODE_BYTES + 1);
        let error = match create_snapshot(&bridge_code) {
            Ok(_) => panic!("oversized bridge code should be rejected"),
            Err(error) => error,
        };

        assert!(error.contains(V8_BRIDGE_CODE_LIMIT_ERROR_CODE));
        assert!(error.contains("bridge code too large for V8 bridge setup"));
        assert!(error.contains(&MAX_V8_BRIDGE_CODE_BYTES.to_string()));
    }

    #[test]
    fn snapshot_cache_rejects_oversized_bridge_code_without_retaining_in_flight_state() {
        let cache = SnapshotCache::new(1);
        let bridge_code = " ".repeat(MAX_V8_BRIDGE_CODE_BYTES + 1);

        for _ in 0..2 {
            let error = match cache.get_or_create(&bridge_code) {
                Ok(_) => panic!("oversized bridge code should be rejected"),
                Err(error) => error,
            };

            assert!(error.contains(V8_BRIDGE_CODE_LIMIT_ERROR_CODE));
        }
    }

    #[test]
    fn snapshot_cache_key_is_dep_keyed_over_bridge_and_userland() {
        let bridge = "bridge-a";
        // Userland presence changes the key vs bridge-only.
        assert_ne!(
            snapshot_cache_key(bridge, None),
            snapshot_cache_key(bridge, Some("user-1")),
            "adding userland must change the key"
        );
        // Different userland → different key (any dep-graph change invalidates).
        assert_ne!(
            snapshot_cache_key(bridge, Some("user-1")),
            snapshot_cache_key(bridge, Some("user-2")),
            "different userland must produce a different key"
        );
        // Identical inputs → identical key (cache hit).
        assert_eq!(
            snapshot_cache_key(bridge, Some("user-1")),
            snapshot_cache_key(bridge, Some("user-1")),
        );
        // The NUL separator prevents bridge/userland boundary collisions.
        assert_ne!(
            snapshot_cache_key("ab", Some("c")),
            snapshot_cache_key("a", Some("bc")),
            "the bridge/userland split must be unambiguous"
        );
    }

    #[test]
    fn create_snapshot_with_userland_rejects_oversized_userland_code() {
        let bridge_code = "(function(){})();";
        let userland = " ".repeat(MAX_V8_USERLAND_CODE_BYTES + 1);
        let error = match create_snapshot_with_userland(bridge_code, &userland) {
            Ok(_) => panic!("oversized userland code should be rejected"),
            Err(error) => error,
        };

        assert!(error.contains(V8_USERLAND_CODE_LIMIT_ERROR_CODE));
        assert!(error.contains("userland snapshot code too large"));
    }
}
