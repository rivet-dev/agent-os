use crate::wire_dispatch::{BrowserWireDispatcher, BROWSER_SIDECAR_ID};
use crate::{
    BrowserWorkerBridge, BrowserWorkerEntrypoint, BrowserWorkerHandle, BrowserWorkerHandleRequest,
    BrowserWorkerOsConfig, BrowserWorkerProcessConfig, BrowserWorkerSpawnRequest,
};
use agentos_bridge::{
    BridgeTypes, ChmodRequest, ClockBridge, ClockRequest, CommandPermissionRequest,
    CreateDirRequest, CreateJavascriptContextRequest, CreateWasmContextRequest, DiagnosticRecord,
    DirectoryEntry, EnvironmentAccess, EnvironmentPermissionRequest, EventBridge, ExecutionBridge,
    ExecutionEvent, ExecutionExited, ExecutionHandleRequest, ExecutionSignal, ExecutionSignalState,
    FileKind, FileMetadata, FilesystemAccess, FilesystemBridge, FilesystemPermissionRequest,
    FilesystemSnapshot, FlushFilesystemStateRequest, GuestContextHandle, GuestKernelCall,
    GuestRuntime, KillExecutionRequest, LifecycleEventRecord, LoadFilesystemStateRequest, LogLevel,
    LogRecord, NetworkAccess, NetworkPermissionRequest, OutputChunk, PathRequest, PermissionBridge,
    PermissionDecision, PermissionVerdict, PersistenceBridge, PollExecutionEventRequest,
    RandomBridge, RandomBytesRequest, ReadDirRequest, ReadFileRequest, RenameRequest,
    ScheduleTimerRequest, ScheduledTimer, SignalDispositionAction, SignalHandlerRegistration,
    StartExecutionRequest, StartedExecution, StructuredEventRecord, SymlinkRequest,
    TruncateRequest, WriteExecutionStdinRequest, WriteFileRequest,
};
use agentos_sidecar_protocol::wire::WasmPermissionTier;
use base64::Engine;
use js_sys::{Error as JsError, Function, Reflect, Uint8Array, JSON};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

#[wasm_bindgen]
pub struct BrowserSidecarWasm {
    dispatcher: BrowserWireDispatcher<BrowserJsBridge>,
}

#[wasm_bindgen]
impl BrowserSidecarWasm {
    #[wasm_bindgen(constructor)]
    pub fn new(host_bridge: Option<JsValue>) -> Self {
        Self {
            dispatcher: BrowserWireDispatcher::new(BrowserJsBridge::new(host_bridge)),
        }
    }

    #[wasm_bindgen(getter, js_name = sidecarId)]
    pub fn sidecar_id(&self) -> String {
        String::from(BROWSER_SIDECAR_ID)
    }

    #[wasm_bindgen(js_name = pushFrame)]
    pub fn push_frame(&mut self, frame: Uint8Array) -> Result<JsValue, JsValue> {
        let bytes = frame.to_vec();
        let response = self
            .dispatcher
            .handle_request_bytes(&bytes)
            .map_err(js_error)?;
        Ok(Uint8Array::from(response.as_slice()).into())
    }

    #[wasm_bindgen(js_name = pollEvent)]
    pub fn poll_event(&mut self) -> Result<JsValue, JsValue> {
        match self.dispatcher.poll_event_bytes().map_err(js_error)? {
            Some(event) => Ok(Uint8Array::from(event.as_slice()).into()),
            None => Ok(JsValue::NULL),
        }
    }
}

impl Default for BrowserSidecarWasm {
    fn default() -> Self {
        Self::new(None)
    }
}

fn js_error(error: impl ToString) -> JsValue {
    JsError::new(&error.to_string()).into()
}

/// The wasm-side host bridge: marshals every kernel/host operation to the JS host
/// object installed in the Worker. Exported so wrapper crates (e.g. Agent OS) can
/// build their own `#[wasm_bindgen]` entry over `BrowserWireDispatcher` while
/// registering their own `BrowserExtension`s, instead of re-implementing the bridge.
#[derive(Clone, Debug)]
pub struct BrowserJsBridge {
    host: Option<JsValue>,
    next_timer: u64,
}

impl BrowserJsBridge {
    pub fn new(host: Option<JsValue>) -> Self {
        Self {
            host: host.filter(|value| !value.is_null() && !value.is_undefined()),
            next_timer: 0,
        }
    }

    fn unsupported<T>(&self, operation: &str) -> Result<T, String> {
        Err(format!(
            "{operation} is not available because no browser sidecar host bridge method is installed"
        ))
    }

    fn call(&self, method: &str, request: Value) -> Result<JsValue, String> {
        let Some(host) = &self.host else {
            return self.unsupported(method);
        };
        let function = Reflect::get(host, &JsValue::from_str(method))
            .map_err(format_js_error)?
            .dyn_into::<Function>()
            .map_err(|_| {
                format!("browser sidecar host bridge method {method} is not a function")
            })?;
        let request = serde_json::to_string(&request)
            .map_err(|error| format!("serialize {method} request: {error}"))?;
        function
            .call1(host, &JsValue::from_str(&request))
            .map_err(format_js_error)
    }

    fn call_void(&self, method: &str, request: Value) -> Result<(), String> {
        let _ = self.call(method, request)?;
        Ok(())
    }

    fn call_json(&self, method: &str, request: Value) -> Result<Value, String> {
        js_value_to_json(self.call(method, request)?)
    }

    fn call_bytes(&self, method: &str, request: Value) -> Result<Vec<u8>, String> {
        let value = self.call(method, request)?;
        if value.is_null() || value.is_undefined() {
            return Ok(Vec::new());
        }
        if value.is_instance_of::<Uint8Array>() {
            return Ok(Uint8Array::new(&value).to_vec());
        }
        if let Some(encoded) = value.as_string() {
            return base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|error| format!("{method} returned invalid base64: {error}"));
        }
        Err(format!(
            "{method} must return Uint8Array, base64 string, null, or undefined"
        ))
    }
}

fn format_js_error(error: JsValue) -> String {
    if let Some(message) = Reflect::get(&error, &JsValue::from_str("message"))
        .ok()
        .and_then(|value| value.as_string())
    {
        return message;
    }
    error
        .as_string()
        .unwrap_or_else(|| String::from("JavaScript error"))
}

fn js_value_to_json(value: JsValue) -> Result<Value, String> {
    if value.is_undefined() || value.is_null() {
        return Ok(Value::Null);
    }
    if let Some(text) = value.as_string() {
        return serde_json::from_str(&text).map_err(|error| format!("parse JSON string: {error}"));
    }
    let text = JSON::stringify(&value)
        .map_err(format_js_error)?
        .as_string()
        .ok_or_else(|| String::from("JSON.stringify returned a non-string value"))?;
    serde_json::from_str(&text).map_err(|error| format!("parse JSON value: {error}"))
}

fn get_string(value: &Value, key: &str) -> Result<String, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("missing string field '{key}'"))
}

fn get_optional_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn get_u64(value: &Value, key: &str) -> Result<u64, String> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("missing u64 field '{key}'"))
}

fn get_i32(value: &Value, key: &str) -> Result<i32, String> {
    value
        .get(key)
        .and_then(Value::as_i64)
        .and_then(|number| i32::try_from(number).ok())
        .ok_or_else(|| format!("missing i32 field '{key}'"))
}

fn base_request(vm_id: impl Into<String>) -> Value {
    json!({ "vmId": vm_id.into() })
}

fn path_request(request: PathRequest) -> Value {
    json!({ "vmId": request.vm_id, "path": request.path })
}

fn file_kind_from_json(value: &Value) -> Result<FileKind, String> {
    match value.as_str().unwrap_or("other") {
        "file" => Ok(FileKind::File),
        "directory" => Ok(FileKind::Directory),
        "symbolicLink" | "symlink" => Ok(FileKind::SymbolicLink),
        "other" => Ok(FileKind::Other),
        kind => Err(format!("unknown file kind '{kind}'")),
    }
}

fn metadata_from_json(value: Value) -> Result<FileMetadata, String> {
    Ok(FileMetadata {
        mode: u32::try_from(get_u64(&value, "mode")?)
            .map_err(|_| String::from("metadata mode exceeds u32"))?,
        size: get_u64(&value, "size")?,
        kind: file_kind_from_json(value.get("kind").unwrap_or(&Value::Null))?,
    })
}

fn directory_entries_from_json(value: Value) -> Result<Vec<DirectoryEntry>, String> {
    let entries = value
        .as_array()
        .ok_or_else(|| String::from("readDir response must be an array"))?;
    entries
        .iter()
        .map(|entry| {
            Ok(DirectoryEntry {
                name: get_string(entry, "name")?,
                kind: file_kind_from_json(entry.get("kind").unwrap_or(&Value::Null))?,
            })
        })
        .collect()
}

fn permission_decision_from_json(value: Value) -> Result<PermissionDecision, String> {
    let verdict = match value
        .get("verdict")
        .and_then(Value::as_str)
        .unwrap_or("deny")
    {
        "allow" => PermissionVerdict::Allow,
        "deny" => PermissionVerdict::Deny,
        "prompt" => PermissionVerdict::Prompt,
        verdict => return Err(format!("unknown permission verdict '{verdict}'")),
    };
    Ok(PermissionDecision {
        verdict,
        reason: get_optional_string(&value, "reason"),
    })
}

fn filesystem_access_to_json(access: FilesystemAccess) -> &'static str {
    match access {
        FilesystemAccess::Read => "read",
        FilesystemAccess::Write => "write",
        FilesystemAccess::Stat => "stat",
        FilesystemAccess::ReadDir => "readDir",
        FilesystemAccess::CreateDir => "createDir",
        FilesystemAccess::Remove => "remove",
        FilesystemAccess::Rename => "rename",
        FilesystemAccess::Symlink => "symlink",
        FilesystemAccess::ReadLink => "readLink",
        FilesystemAccess::Chmod => "chmod",
        FilesystemAccess::Truncate => "truncate",
    }
}

fn network_access_to_json(access: NetworkAccess) -> &'static str {
    match access {
        NetworkAccess::Fetch => "fetch",
        NetworkAccess::Http => "http",
        NetworkAccess::Dns => "dns",
        NetworkAccess::Listen => "listen",
    }
}

fn environment_access_to_json(access: EnvironmentAccess) -> &'static str {
    match access {
        EnvironmentAccess::Read => "read",
        EnvironmentAccess::Write => "write",
    }
}

fn log_level_to_json(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Trace => "trace",
        LogLevel::Debug => "debug",
        LogLevel::Info => "info",
        LogLevel::Warn => "warn",
        LogLevel::Error => "error",
    }
}

fn guest_runtime_to_json(runtime: GuestRuntime) -> &'static str {
    match runtime {
        GuestRuntime::JavaScript => "javascript",
        GuestRuntime::WebAssembly => "webassembly",
    }
}

fn guest_runtime_from_json(value: &Value) -> Result<GuestRuntime, String> {
    match value.as_str().unwrap_or("javascript") {
        "javascript" | "js" => Ok(GuestRuntime::JavaScript),
        "webassembly" | "wasm" => Ok(GuestRuntime::WebAssembly),
        runtime => Err(format!("unknown guest runtime '{runtime}'")),
    }
}

fn signal_to_json(signal: ExecutionSignal) -> &'static str {
    match signal {
        ExecutionSignal::Terminate => "terminate",
        ExecutionSignal::Interrupt => "interrupt",
        ExecutionSignal::Kill => "kill",
    }
}

fn signal_action_from_json(value: &Value) -> Result<SignalDispositionAction, String> {
    match value.as_str().unwrap_or("default") {
        "default" => Ok(SignalDispositionAction::Default),
        "ignore" => Ok(SignalDispositionAction::Ignore),
        "user" => Ok(SignalDispositionAction::User),
        action => Err(format!("unknown signal disposition action '{action}'")),
    }
}

fn signal_registration_from_json(value: &Value) -> Result<SignalHandlerRegistration, String> {
    let mask = value
        .get("mask")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .map(|entry| {
                    entry
                        .as_u64()
                        .and_then(|number| u32::try_from(number).ok())
                        .ok_or_else(|| String::from("invalid signal mask entry"))
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    Ok(SignalHandlerRegistration {
        action: signal_action_from_json(value.get("action").unwrap_or(&Value::Null))?,
        mask,
        flags: value
            .get("flags")
            .and_then(Value::as_u64)
            .and_then(|number| u32::try_from(number).ok())
            .unwrap_or(0),
    })
}

fn system_time_from_ms(ms: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(ms)
}

fn map_to_json(map: BTreeMap<String, String>) -> Value {
    Value::Object(
        map.into_iter()
            .map(|(key, value)| (key, Value::String(value)))
            .collect(),
    )
}

fn snapshot_to_json(snapshot: FilesystemSnapshot) -> Value {
    json!({
        "format": snapshot.format,
        "bytesBase64": base64::engine::general_purpose::STANDARD.encode(snapshot.bytes),
    })
}

fn snapshot_from_json(value: &Value) -> Result<FilesystemSnapshot, String> {
    Ok(FilesystemSnapshot {
        format: get_string(value, "format")?,
        bytes: base64::engine::general_purpose::STANDARD
            .decode(get_string(value, "bytesBase64")?)
            .map_err(|error| format!("invalid filesystem snapshot bytes: {error}"))?,
    })
}

fn output_chunk_from_json(value: &Value) -> Result<OutputChunk, String> {
    Ok(OutputChunk {
        vm_id: get_string(value, "vmId")?,
        execution_id: get_string(value, "executionId")?,
        chunk: base64::engine::general_purpose::STANDARD
            .decode(get_string(value, "chunkBase64")?)
            .map_err(|error| format!("invalid output chunk bytes: {error}"))?,
    })
}

fn execution_event_from_json(value: Value) -> Result<Option<ExecutionEvent>, String> {
    if value.is_null() {
        return Ok(None);
    }
    match value.get("type").and_then(Value::as_str).unwrap_or("") {
        "stdout" => Ok(Some(ExecutionEvent::Stdout(output_chunk_from_json(
            &value,
        )?))),
        "stderr" => Ok(Some(ExecutionEvent::Stderr(output_chunk_from_json(
            &value,
        )?))),
        "exited" => Ok(Some(ExecutionEvent::Exited(ExecutionExited {
            vm_id: get_string(&value, "vmId")?,
            execution_id: get_string(&value, "executionId")?,
            exit_code: get_i32(&value, "exitCode")?,
        }))),
        "guestRequest" => Ok(Some(ExecutionEvent::GuestRequest(GuestKernelCall {
            vm_id: get_string(&value, "vmId")?,
            execution_id: get_string(&value, "executionId")?,
            operation: get_string(&value, "operation")?,
            payload: base64::engine::general_purpose::STANDARD
                .decode(get_string(&value, "payloadBase64")?)
                .map_err(|error| format!("invalid guest request payload: {error}"))?,
        }))),
        "signalState" => Ok(Some(ExecutionEvent::SignalState(ExecutionSignalState {
            vm_id: get_string(&value, "vmId")?,
            execution_id: get_string(&value, "executionId")?,
            signal: u32::try_from(get_u64(&value, "signal")?)
                .map_err(|_| String::from("signal exceeds u32"))?,
            registration: signal_registration_from_json(
                value.get("registration").unwrap_or(&Value::Null),
            )?,
        }))),
        event_type => Err(format!("unknown execution event type '{event_type}'")),
    }
}

fn worker_entrypoint_to_json(entrypoint: BrowserWorkerEntrypoint) -> Value {
    match entrypoint {
        BrowserWorkerEntrypoint::JavaScript { bootstrap_module } => {
            json!({ "kind": "javascript", "bootstrapModule": bootstrap_module })
        }
        BrowserWorkerEntrypoint::WebAssembly { module_path } => {
            json!({ "kind": "webassembly", "modulePath": module_path })
        }
    }
}

fn process_config_to_json(config: BrowserWorkerProcessConfig) -> Value {
    json!({
        "cwd": config.cwd,
        "env": map_to_json(config.env),
        "argv": config.argv,
        "platform": config.platform,
        "arch": config.arch,
        "version": config.version,
        "pid": config.pid,
        "ppid": config.ppid,
        "uid": config.uid,
        "gid": config.gid,
    })
}

fn os_config_to_json(config: BrowserWorkerOsConfig) -> Value {
    json!({
        "platform": config.platform,
        "arch": config.arch,
        "type": config.r#type,
        "release": config.release,
        "version": config.version,
        "cpuCount": config.cpu_count,
        "totalmem": config.totalmem,
        "freemem": config.freemem,
        "hostname": config.hostname,
        "homedir": config.homedir,
        "tmpdir": config.tmpdir,
        "machine": config.machine,
        "user": config.user,
        "shell": config.shell,
        "uid": config.uid,
        "gid": config.gid,
    })
}

fn wasm_permission_tier_to_json(tier: Option<WasmPermissionTier>) -> Value {
    tier.map(|tier| Value::String(format!("{tier:?}").to_ascii_lowercase()))
        .unwrap_or(Value::Null)
}

impl BridgeTypes for BrowserJsBridge {
    type Error = String;
}

impl FilesystemBridge for BrowserJsBridge {
    fn read_file(&mut self, request: ReadFileRequest) -> Result<Vec<u8>, Self::Error> {
        self.call_bytes(
            "readFile",
            json!({ "vmId": request.vm_id, "path": request.path }),
        )
    }

    fn write_file(&mut self, request: WriteFileRequest) -> Result<(), Self::Error> {
        self.call_void(
            "writeFile",
            json!({
                "vmId": request.vm_id,
                "path": request.path,
                "contentsBase64": base64::engine::general_purpose::STANDARD.encode(request.contents),
            }),
        )
    }

    fn stat(&mut self, request: PathRequest) -> Result<FileMetadata, Self::Error> {
        metadata_from_json(self.call_json("stat", path_request(request))?)
    }

    fn lstat(&mut self, request: PathRequest) -> Result<FileMetadata, Self::Error> {
        metadata_from_json(self.call_json("lstat", path_request(request))?)
    }

    fn read_dir(&mut self, request: ReadDirRequest) -> Result<Vec<DirectoryEntry>, Self::Error> {
        directory_entries_from_json(self.call_json(
            "readDir",
            json!({ "vmId": request.vm_id, "path": request.path }),
        )?)
    }

    fn create_dir(&mut self, request: CreateDirRequest) -> Result<(), Self::Error> {
        self.call_void(
            "createDir",
            json!({ "vmId": request.vm_id, "path": request.path, "recursive": request.recursive }),
        )
    }

    fn remove_file(&mut self, request: PathRequest) -> Result<(), Self::Error> {
        self.call_void("removeFile", path_request(request))
    }

    fn remove_dir(&mut self, request: PathRequest) -> Result<(), Self::Error> {
        self.call_void("removeDir", path_request(request))
    }

    fn rename(&mut self, request: RenameRequest) -> Result<(), Self::Error> {
        self.call_void(
            "rename",
            json!({
                "vmId": request.vm_id,
                "fromPath": request.from_path,
                "toPath": request.to_path,
            }),
        )
    }

    fn symlink(&mut self, request: SymlinkRequest) -> Result<(), Self::Error> {
        self.call_void(
            "symlink",
            json!({
                "vmId": request.vm_id,
                "targetPath": request.target_path,
                "linkPath": request.link_path,
            }),
        )
    }

    fn read_link(&mut self, request: PathRequest) -> Result<String, Self::Error> {
        get_string(
            &self.call_json("readLink", path_request(request))?,
            "targetPath",
        )
    }

    fn chmod(&mut self, request: ChmodRequest) -> Result<(), Self::Error> {
        self.call_void(
            "chmod",
            json!({ "vmId": request.vm_id, "path": request.path, "mode": request.mode }),
        )
    }

    fn truncate(&mut self, request: TruncateRequest) -> Result<(), Self::Error> {
        self.call_void(
            "truncate",
            json!({ "vmId": request.vm_id, "path": request.path, "len": request.len }),
        )
    }

    fn exists(&mut self, request: PathRequest) -> Result<bool, Self::Error> {
        self.call_json("exists", path_request(request))?
            .as_bool()
            .ok_or_else(|| String::from("exists response must be boolean"))
    }
}

impl PermissionBridge for BrowserJsBridge {
    fn check_filesystem_access(
        &mut self,
        request: FilesystemPermissionRequest,
    ) -> Result<PermissionDecision, Self::Error> {
        permission_decision_from_json(self.call_json(
            "checkFilesystemAccess",
            json!({
                "vmId": request.vm_id,
                "path": request.path,
                "access": filesystem_access_to_json(request.access),
            }),
        )?)
    }

    fn check_network_access(
        &mut self,
        request: NetworkPermissionRequest,
    ) -> Result<PermissionDecision, Self::Error> {
        permission_decision_from_json(self.call_json(
            "checkNetworkAccess",
            json!({
                "vmId": request.vm_id,
                "access": network_access_to_json(request.access),
                "resource": request.resource,
            }),
        )?)
    }

    fn check_command_execution(
        &mut self,
        request: CommandPermissionRequest,
    ) -> Result<PermissionDecision, Self::Error> {
        permission_decision_from_json(self.call_json(
            "checkCommandExecution",
            json!({
                "vmId": request.vm_id,
                "command": request.command,
                "args": request.args,
                "cwd": request.cwd,
                "env": map_to_json(request.env),
            }),
        )?)
    }

    fn check_environment_access(
        &mut self,
        request: EnvironmentPermissionRequest,
    ) -> Result<PermissionDecision, Self::Error> {
        permission_decision_from_json(self.call_json(
            "checkEnvironmentAccess",
            json!({
                "vmId": request.vm_id,
                "access": environment_access_to_json(request.access),
                "key": request.key,
                "value": request.value,
            }),
        )?)
    }
}

impl PersistenceBridge for BrowserJsBridge {
    fn load_filesystem_state(
        &mut self,
        request: LoadFilesystemStateRequest,
    ) -> Result<Option<FilesystemSnapshot>, Self::Error> {
        let value = self.call_json("loadFilesystemState", json!({ "vmId": request.vm_id }))?;
        if value.is_null() {
            return Ok(None);
        }
        snapshot_from_json(&value).map(Some)
    }

    fn flush_filesystem_state(
        &mut self,
        request: FlushFilesystemStateRequest,
    ) -> Result<(), Self::Error> {
        self.call_void(
            "flushFilesystemState",
            json!({
                "vmId": request.vm_id,
                "snapshot": snapshot_to_json(request.snapshot),
            }),
        )
    }
}

impl ClockBridge for BrowserJsBridge {
    fn wall_clock(&mut self, request: ClockRequest) -> Result<SystemTime, Self::Error> {
        if self.host.is_none() {
            return Ok(SystemTime::now());
        }
        let value = self.call_json("wallClock", base_request(request.vm_id))?;
        Ok(system_time_from_ms(get_u64(&value, "unixTimeMs")?))
    }

    fn monotonic_clock(&mut self, request: ClockRequest) -> Result<Duration, Self::Error> {
        if self.host.is_none() {
            return Ok(Duration::ZERO);
        }
        let value = self.call_json("monotonicClock", base_request(request.vm_id))?;
        Ok(Duration::from_millis(get_u64(&value, "elapsedMs")?))
    }

    fn schedule_timer(
        &mut self,
        request: ScheduleTimerRequest,
    ) -> Result<ScheduledTimer, Self::Error> {
        if self.host.is_none() {
            self.next_timer += 1;
            return Ok(ScheduledTimer {
                timer_id: format!("browser-sidecar-wasm-timer-{}", self.next_timer),
                delay: request.delay,
            });
        }
        let value = self.call_json(
            "scheduleTimer",
            json!({
                "vmId": request.vm_id,
                "delayMs": request.delay.as_millis() as u64,
            }),
        )?;
        Ok(ScheduledTimer {
            timer_id: get_string(&value, "timerId")?,
            delay: Duration::from_millis(get_u64(&value, "delayMs")?),
        })
    }
}

impl RandomBridge for BrowserJsBridge {
    fn fill_random_bytes(&mut self, request: RandomBytesRequest) -> Result<Vec<u8>, Self::Error> {
        if self.host.is_some() {
            return self.call_bytes(
                "fillRandomBytes",
                json!({ "vmId": request.vm_id, "len": request.len }),
            );
        }
        let mut bytes = vec![0; request.len];
        getrandom::getrandom(&mut bytes)
            .map_err(|error| format!("fill_random_bytes failed: {error}"))?;
        Ok(bytes)
    }
}

impl EventBridge for BrowserJsBridge {
    fn emit_structured_event(&mut self, event: StructuredEventRecord) -> Result<(), Self::Error> {
        if self.host.is_none() {
            return Ok(());
        }
        self.call_void(
            "emitStructuredEvent",
            json!({ "vmId": event.vm_id, "name": event.name, "fields": map_to_json(event.fields) }),
        )
    }

    fn emit_diagnostic(&mut self, event: DiagnosticRecord) -> Result<(), Self::Error> {
        if self.host.is_none() {
            return Ok(());
        }
        self.call_void(
            "emitDiagnostic",
            json!({ "vmId": event.vm_id, "message": event.message, "fields": map_to_json(event.fields) }),
        )
    }

    fn emit_log(&mut self, event: LogRecord) -> Result<(), Self::Error> {
        if self.host.is_none() {
            return Ok(());
        }
        self.call_void(
            "emitLog",
            json!({
                "vmId": event.vm_id,
                "level": log_level_to_json(event.level),
                "message": event.message,
            }),
        )
    }

    fn emit_lifecycle(&mut self, event: LifecycleEventRecord) -> Result<(), Self::Error> {
        if self.host.is_none() {
            return Ok(());
        }
        self.call_void(
            "emitLifecycle",
            json!({
                "vmId": event.vm_id,
                "state": format!("{:?}", event.state).to_ascii_lowercase(),
                "detail": event.detail,
            }),
        )
    }
}

impl ExecutionBridge for BrowserJsBridge {
    fn create_javascript_context(
        &mut self,
        request: CreateJavascriptContextRequest,
    ) -> Result<GuestContextHandle, Self::Error> {
        let value = self.call_json(
            "createJavascriptContext",
            json!({
                "vmId": request.vm_id,
                "bootstrapModule": request.bootstrap_module,
            }),
        )?;
        Ok(GuestContextHandle {
            context_id: get_string(&value, "contextId")?,
            runtime: get_optional_string(&value, "runtime")
                .map(|runtime| guest_runtime_from_json(&Value::String(runtime)))
                .transpose()?
                .unwrap_or(GuestRuntime::JavaScript),
        })
    }

    fn create_wasm_context(
        &mut self,
        request: CreateWasmContextRequest,
    ) -> Result<GuestContextHandle, Self::Error> {
        let value = self.call_json(
            "createWasmContext",
            json!({ "vmId": request.vm_id, "modulePath": request.module_path }),
        )?;
        Ok(GuestContextHandle {
            context_id: get_string(&value, "contextId")?,
            runtime: get_optional_string(&value, "runtime")
                .map(|runtime| guest_runtime_from_json(&Value::String(runtime)))
                .transpose()?
                .unwrap_or(GuestRuntime::WebAssembly),
        })
    }

    fn start_execution(
        &mut self,
        request: StartExecutionRequest,
    ) -> Result<StartedExecution, Self::Error> {
        let value = self.call_json(
            "startExecution",
            json!({
                "vmId": request.vm_id,
                "contextId": request.context_id,
                "argv": request.argv,
                "env": map_to_json(request.env),
                "cwd": request.cwd,
            }),
        )?;
        Ok(StartedExecution {
            execution_id: get_string(&value, "executionId")?,
        })
    }

    fn write_stdin(&mut self, request: WriteExecutionStdinRequest) -> Result<(), Self::Error> {
        self.call_void(
            "writeExecutionStdin",
            json!({
                "vmId": request.vm_id,
                "executionId": request.execution_id,
                "chunkBase64": base64::engine::general_purpose::STANDARD.encode(request.chunk),
            }),
        )
    }

    fn close_stdin(&mut self, request: ExecutionHandleRequest) -> Result<(), Self::Error> {
        self.call_void(
            "closeExecutionStdin",
            json!({ "vmId": request.vm_id, "executionId": request.execution_id }),
        )
    }

    fn kill_execution(&mut self, request: KillExecutionRequest) -> Result<(), Self::Error> {
        self.call_void(
            "killExecution",
            json!({
                "vmId": request.vm_id,
                "executionId": request.execution_id,
                "signal": signal_to_json(request.signal),
            }),
        )
    }

    fn poll_execution_event(
        &mut self,
        request: PollExecutionEventRequest,
    ) -> Result<Option<ExecutionEvent>, Self::Error> {
        execution_event_from_json(
            self.call_json("pollExecutionEvent", json!({ "vmId": request.vm_id }))?,
        )
    }
}

impl BrowserWorkerBridge for BrowserJsBridge {
    fn create_worker(
        &mut self,
        request: BrowserWorkerSpawnRequest,
    ) -> Result<BrowserWorkerHandle, Self::Error> {
        let value = self.call_json(
            "createWorker",
            json!({
                "vmId": request.vm_id,
                "contextId": request.context_id,
                "executionId": request.execution_id,
                "runtime": guest_runtime_to_json(request.runtime),
                "entrypoint": worker_entrypoint_to_json(request.entrypoint),
                "wasmPermissionTier": wasm_permission_tier_to_json(request.wasm_permission_tier),
                "process": process_config_to_json(request.process_config),
                "os": os_config_to_json(request.os_config),
            }),
        )?;
        Ok(BrowserWorkerHandle {
            worker_id: get_string(&value, "workerId")?,
            runtime: get_optional_string(&value, "runtime")
                .map(|runtime| guest_runtime_from_json(&Value::String(runtime)))
                .transpose()?
                .unwrap_or(request.runtime),
        })
    }

    fn terminate_worker(&mut self, request: BrowserWorkerHandleRequest) -> Result<(), Self::Error> {
        if self.host.is_none() {
            return Ok(());
        }
        self.call_void(
            "terminateWorker",
            json!({
                "vmId": request.vm_id,
                "executionId": request.execution_id,
                "workerId": request.worker_id,
            }),
        )
    }
}
