use agent_os_bridge::{
    BridgeTypes, ChmodRequest, ClockBridge, ClockRequest, CommandPermissionRequest,
    CreateDirRequest, CreateJavascriptContextRequest, CreateWasmContextRequest, DiagnosticRecord,
    DirectoryEntry, EnvironmentPermissionRequest, EventBridge, ExecutionBridge, ExecutionEvent,
    ExecutionHandleRequest, FileMetadata, FilesystemBridge, FilesystemPermissionRequest,
    FilesystemSnapshot, FlushFilesystemStateRequest, GuestContextHandle, KillExecutionRequest,
    LifecycleEventRecord, LoadFilesystemStateRequest, LogRecord, NetworkPermissionRequest,
    PathRequest, PermissionBridge, PermissionDecision, PersistenceBridge,
    PollExecutionEventRequest, RandomBridge, RandomBytesRequest, ReadDirRequest, ReadFileRequest,
    RenameRequest, ScheduleTimerRequest, ScheduledTimer, StartExecutionRequest, StartedExecution,
    StructuredEventRecord, SymlinkRequest, TruncateRequest, WriteExecutionStdinRequest,
    WriteFileRequest,
};
use agent_os_sidecar::protocol::{
    AuthenticatedResponse, NativeFrameCodec, ProtocolCodecError, ProtocolFrame, RequestId,
    ResponsePayload, SessionOpenedResponse, SidecarRequestFrame, SidecarResponseFrame,
};
use agent_os_sidecar::{NativeSidecar, NativeSidecarConfig, SidecarError, SidecarRequestTransport};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{symlink as create_symlink, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::mpsc::unbounded_channel;
use tokio::time;

const EVENT_PUMP_INTERVAL: Duration = Duration::from_millis(5);

pub fn run() -> Result<(), Box<dyn Error>> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(run_async())
}

async fn run_async() -> Result<(), Box<dyn Error>> {
    let config = NativeSidecarConfig {
        compile_cache_root: Some(default_compile_cache_root()),
        ..NativeSidecarConfig::default()
    };
    let codec = NativeFrameCodec::new(config.max_frame_bytes);
    let mut sidecar = NativeSidecar::with_config(LocalBridge::default(), config)?;
    let mut active_sessions = BTreeSet::<SessionScope>::new();
    let mut active_connections = BTreeSet::<String>::new();
    let (stdin_tx, mut stdin_rx) = unbounded_channel::<Result<Option<ProtocolFrame>, String>>();
    let (event_ready_tx, mut event_ready_rx) = unbounded_channel::<()>();
    let (write_tx, write_rx) = mpsc::channel::<ProtocolFrame>();
    let (write_error_tx, mut write_error_rx) = unbounded_channel::<String>();
    let callback_transport = Arc::new(FrameSidecarRequestTransport::new(write_tx.clone()));
    sidecar.set_sidecar_request_transport(callback_transport.clone());
    let mut event_pump = time::interval(EVENT_PUMP_INTERVAL);
    let writer_codec = codec.clone();
    let reader_codec = codec.clone();

    thread::spawn(move || {
        let mut writer = io::BufWriter::new(io::stdout());
        while let Ok(frame) = write_rx.recv() {
            if let Err(error) = write_frame(&writer_codec, &mut writer, &frame) {
                let _ = write_error_tx.send(error.to_string());
                break;
            }
        }
    });

    thread::spawn({
        let callback_transport = callback_transport.clone();
        move || {
            let mut stdin = io::stdin();
            loop {
                let frame = match read_frame(&reader_codec, &mut stdin) {
                    Ok(Some(ProtocolFrame::SidecarResponse(response))) => {
                        if callback_transport.accept_response(response.clone()) {
                            continue;
                        }
                        Ok(Some(ProtocolFrame::SidecarResponse(response)))
                    }
                    other => other,
                }
                .map_err(|error: Box<dyn Error>| error.to_string());
                let should_stop = matches!(frame, Ok(None) | Err(_));
                if stdin_tx.send(frame).is_err() || should_stop {
                    break;
                }
            }
        }
    });

    flush_sidecar_requests(&mut sidecar, &write_tx)?;

    loop {
        tokio::select! {
            maybe_frame = stdin_rx.recv() => {
                let Some(frame) = maybe_frame else {
                    break;
                };
                let Some(frame) = frame.map_err(io::Error::other)? else {
                    break;
                };
                match frame {
                    ProtocolFrame::Request(request) => {
                        let dispatch = sidecar.dispatch(request.clone()).await?;
                        track_session_state(
                            &dispatch.response.payload,
                            &mut active_sessions,
                            &mut active_connections,
                        );

                        write_tx.send(ProtocolFrame::Response(dispatch.response)).map_err(|error| {
                            io::Error::new(io::ErrorKind::BrokenPipe, error.to_string())
                        })?;
                        for event in dispatch.events {
                            write_tx.send(ProtocolFrame::Event(event)).map_err(|error| {
                                io::Error::new(io::ErrorKind::BrokenPipe, error.to_string())
                            })?;
                        }
                        flush_sidecar_requests(&mut sidecar, &write_tx)?;
                    }
                    ProtocolFrame::SidecarResponse(response) => {
                        sidecar.accept_sidecar_response(response)?;
                        flush_sidecar_requests(&mut sidecar, &write_tx)?;
                    }
                    other => {
                        return Err(format!(
                            "expected request or sidecar_response frame on stdin, received {}",
                            frame_kind(&other)
                        ).into());
                    }
                }
            }
            maybe_ready = event_ready_rx.recv() => {
                let Some(()) = maybe_ready else {
                    break;
                };
                loop {
                    let mut emitted_frame = false;
                    for session in active_sessions.iter().cloned().collect::<Vec<_>>() {
                        if let Some(frame) = sidecar
                            .poll_event(&session.ownership_scope(), Duration::ZERO)
                            .await?
                        {
                            write_tx.send(ProtocolFrame::Event(frame)).map_err(|error| {
                                io::Error::new(io::ErrorKind::BrokenPipe, error.to_string())
                            })?;
                            emitted_frame = true;
                        }
                    }

                    if !emitted_frame {
                        break;
                    }
                }
                flush_sidecar_requests(&mut sidecar, &write_tx)?;
            }
            _ = event_pump.tick() => {
                for session in active_sessions.iter().cloned().collect::<Vec<_>>() {
                    if sidecar.pump_process_events(&session.ownership_scope()).await? {
                        let _ = event_ready_tx.send(());
                    }
                }
                flush_sidecar_requests(&mut sidecar, &write_tx)?;
            }
            maybe_write_error = write_error_rx.recv() => {
                if let Some(error) = maybe_write_error {
                    return Err(io::Error::new(io::ErrorKind::BrokenPipe, error).into());
                }
            }
        }
    }

    cleanup_connections(&mut sidecar, &active_connections).await;
    Ok(())
}

async fn cleanup_connections(
    sidecar: &mut NativeSidecar<LocalBridge>,
    active_connections: &BTreeSet<String>,
) {
    for connection_id in active_connections {
        let _ = sidecar.remove_connection(connection_id).await;
    }
}

fn track_session_state(
    payload: &ResponsePayload,
    active_sessions: &mut BTreeSet<SessionScope>,
    active_connections: &mut BTreeSet<String>,
) {
    match payload {
        ResponsePayload::Authenticated(AuthenticatedResponse { connection_id, .. }) => {
            active_connections.insert(connection_id.clone());
        }
        ResponsePayload::SessionOpened(SessionOpenedResponse {
            session_id,
            owner_connection_id,
        }) => {
            active_sessions.insert(SessionScope {
                connection_id: owner_connection_id.clone(),
                session_id: session_id.clone(),
            });
        }
        _ => {}
    }
}

fn read_frame(
    codec: &NativeFrameCodec,
    reader: &mut impl Read,
) -> Result<Option<ProtocolFrame>, Box<dyn Error>> {
    let mut prefix = [0u8; 4];
    match reader.read_exact(&mut prefix) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    }

    let declared_len = u32::from_be_bytes(prefix) as usize;
    if declared_len > codec.max_frame_bytes() {
        return Err(ProtocolCodecError::FrameTooLarge {
            size: declared_len,
            max: codec.max_frame_bytes(),
        }
        .into());
    }
    let total_len = prefix.len().saturating_add(declared_len);
    let mut bytes = Vec::with_capacity(total_len);
    bytes.extend_from_slice(&prefix);
    bytes.resize(total_len, 0);
    reader.read_exact(&mut bytes[prefix.len()..])?;
    Ok(Some(codec.decode(&bytes)?))
}

fn write_frame(
    codec: &NativeFrameCodec,
    writer: &mut impl Write,
    frame: &ProtocolFrame,
) -> Result<(), Box<dyn Error>> {
    let bytes = codec.encode(frame)?;
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}

fn frame_kind(frame: &ProtocolFrame) -> &'static str {
    match frame {
        ProtocolFrame::Request(_) => "request",
        ProtocolFrame::Response(_) => "response",
        ProtocolFrame::Event(_) => "event",
        ProtocolFrame::SidecarRequest(_) => "sidecar_request",
        ProtocolFrame::SidecarResponse(_) => "sidecar_response",
    }
}

fn flush_sidecar_requests(
    sidecar: &mut NativeSidecar<LocalBridge>,
    writer: &mpsc::Sender<ProtocolFrame>,
) -> Result<(), Box<dyn Error>> {
    while let Some(request) = sidecar.pop_sidecar_request() {
        writer
            .send(ProtocolFrame::SidecarRequest(request))
            .map_err(|error| io::Error::new(io::ErrorKind::BrokenPipe, error.to_string()))?;
    }
    Ok(())
}

fn default_compile_cache_root() -> PathBuf {
    std::env::temp_dir().join(format!(
        "agent-os-sidecar-compile-cache-{}",
        std::process::id()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn read_frame_rejects_oversized_prefix_before_allocating_payload() {
        let codec = NativeFrameCodec::new(16);
        let mut reader = Cursor::new((32_u32).to_be_bytes().to_vec());

        let error = read_frame(&codec, &mut reader).expect_err("oversized frame should fail");
        let error = error
            .downcast::<ProtocolCodecError>()
            .expect("protocol codec error");
        assert!(matches!(
            *error,
            ProtocolCodecError::FrameTooLarge { size: 32, max: 16 }
        ));
    }
}

#[derive(Debug, Clone)]
struct LocalBridge {
    started_at: Instant,
    next_timer_id: usize,
    snapshots: BTreeMap<String, FilesystemSnapshot>,
}

impl Default for LocalBridge {
    fn default() -> Self {
        Self {
            started_at: Instant::now(),
            next_timer_id: 0,
            snapshots: BTreeMap::new(),
        }
    }
}

impl BridgeTypes for LocalBridge {
    type Error = LocalBridgeError;
}

impl FilesystemBridge for LocalBridge {
    fn read_file(&mut self, request: ReadFileRequest) -> Result<Vec<u8>, Self::Error> {
        fs::read(Self::host_path(&request.path))
            .map_err(|error| LocalBridgeError::io("read", &request.path, error))
    }

    fn write_file(&mut self, request: WriteFileRequest) -> Result<(), Self::Error> {
        let host_path = Self::host_path(&request.path);
        if let Some(parent) = host_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| LocalBridgeError::io("mkdir", &request.path, error))?;
        }
        fs::write(host_path, request.contents)
            .map_err(|error| LocalBridgeError::io("write", &request.path, error))
    }

    fn stat(&mut self, request: PathRequest) -> Result<FileMetadata, Self::Error> {
        fs::metadata(Self::host_path(&request.path))
            .map(Self::file_metadata)
            .map_err(|error| LocalBridgeError::io("stat", &request.path, error))
    }

    fn lstat(&mut self, request: PathRequest) -> Result<FileMetadata, Self::Error> {
        fs::symlink_metadata(Self::host_path(&request.path))
            .map(Self::file_metadata)
            .map_err(|error| LocalBridgeError::io("lstat", &request.path, error))
    }

    fn read_dir(&mut self, request: ReadDirRequest) -> Result<Vec<DirectoryEntry>, Self::Error> {
        let mut entries = fs::read_dir(Self::host_path(&request.path))
            .map_err(|error| LocalBridgeError::io("readdir", &request.path, error))?
            .map(|entry| {
                let entry =
                    entry.map_err(|error| LocalBridgeError::io("readdir", &request.path, error))?;
                let kind = entry
                    .file_type()
                    .map(Self::file_kind)
                    .map_err(|error| LocalBridgeError::io("readdir", &request.path, error))?;
                Ok(DirectoryEntry {
                    name: entry.file_name().to_string_lossy().into_owned(),
                    kind,
                })
            })
            .collect::<Result<Vec<_>, LocalBridgeError>>()?;
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(entries)
    }

    fn create_dir(&mut self, request: CreateDirRequest) -> Result<(), Self::Error> {
        let host_path = Self::host_path(&request.path);
        if request.recursive {
            fs::create_dir_all(host_path)
        } else {
            fs::create_dir(host_path)
        }
        .map_err(|error| LocalBridgeError::io("mkdir", &request.path, error))
    }

    fn remove_file(&mut self, request: PathRequest) -> Result<(), Self::Error> {
        fs::remove_file(Self::host_path(&request.path))
            .map_err(|error| LocalBridgeError::io("unlink", &request.path, error))
    }

    fn remove_dir(&mut self, request: PathRequest) -> Result<(), Self::Error> {
        fs::remove_dir(Self::host_path(&request.path))
            .map_err(|error| LocalBridgeError::io("rmdir", &request.path, error))
    }

    fn rename(&mut self, request: RenameRequest) -> Result<(), Self::Error> {
        let from_path = Self::host_path(&request.from_path);
        let to_path = Self::host_path(&request.to_path);
        if let Some(parent) = to_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| LocalBridgeError::io("mkdir", &request.to_path, error))?;
        }
        fs::rename(from_path, to_path).map_err(|error| {
            LocalBridgeError::unsupported(format!(
                "rename {} -> {}: {}",
                request.from_path, request.to_path, error
            ))
        })
    }

    fn symlink(&mut self, request: SymlinkRequest) -> Result<(), Self::Error> {
        let link_path = Self::host_path(&request.link_path);
        if let Some(parent) = link_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| LocalBridgeError::io("mkdir", &request.link_path, error))?;
        }
        create_symlink(&request.target_path, link_path)
            .map_err(|error| LocalBridgeError::io("symlink", &request.link_path, error))
    }

    fn read_link(&mut self, request: PathRequest) -> Result<String, Self::Error> {
        fs::read_link(Self::host_path(&request.path))
            .map(|target| target.to_string_lossy().into_owned())
            .map_err(|error| LocalBridgeError::io("readlink", &request.path, error))
    }

    fn chmod(&mut self, request: ChmodRequest) -> Result<(), Self::Error> {
        let permissions = fs::Permissions::from_mode(request.mode);
        fs::set_permissions(Self::host_path(&request.path), permissions)
            .map_err(|error| LocalBridgeError::io("chmod", &request.path, error))
    }

    fn truncate(&mut self, request: TruncateRequest) -> Result<(), Self::Error> {
        OpenOptions::new()
            .write(true)
            .create(false)
            .open(Self::host_path(&request.path))
            .and_then(|file| file.set_len(request.len))
            .map_err(|error| LocalBridgeError::io("truncate", &request.path, error))
    }

    fn exists(&mut self, request: PathRequest) -> Result<bool, Self::Error> {
        Ok(fs::symlink_metadata(Self::host_path(&request.path)).is_ok())
    }
}

impl PermissionBridge for LocalBridge {
    fn check_filesystem_access(
        &mut self,
        request: FilesystemPermissionRequest,
    ) -> Result<PermissionDecision, Self::Error> {
        Ok(PermissionDecision::deny(format!(
            "no static filesystem policy registered for {}:{}",
            request.vm_id, request.path
        )))
    }

    fn check_network_access(
        &mut self,
        request: NetworkPermissionRequest,
    ) -> Result<PermissionDecision, Self::Error> {
        Ok(PermissionDecision::deny(format!(
            "no static network policy registered for {}:{}",
            request.vm_id, request.resource
        )))
    }

    fn check_command_execution(
        &mut self,
        request: CommandPermissionRequest,
    ) -> Result<PermissionDecision, Self::Error> {
        Ok(PermissionDecision::deny(format!(
            "no static child_process policy registered for {}:{}",
            request.vm_id, request.command
        )))
    }

    fn check_environment_access(
        &mut self,
        request: EnvironmentPermissionRequest,
    ) -> Result<PermissionDecision, Self::Error> {
        Ok(PermissionDecision::deny(format!(
            "no static env policy registered for {}:{}",
            request.vm_id, request.key
        )))
    }
}

impl PersistenceBridge for LocalBridge {
    fn load_filesystem_state(
        &mut self,
        request: LoadFilesystemStateRequest,
    ) -> Result<Option<FilesystemSnapshot>, Self::Error> {
        Ok(self.snapshots.get(&request.vm_id).cloned())
    }

    fn flush_filesystem_state(
        &mut self,
        request: FlushFilesystemStateRequest,
    ) -> Result<(), Self::Error> {
        self.snapshots.insert(request.vm_id, request.snapshot);
        Ok(())
    }
}

impl ClockBridge for LocalBridge {
    fn wall_clock(&mut self, _request: ClockRequest) -> Result<SystemTime, Self::Error> {
        Ok(SystemTime::now())
    }

    fn monotonic_clock(&mut self, _request: ClockRequest) -> Result<Duration, Self::Error> {
        Ok(self.started_at.elapsed())
    }

    fn schedule_timer(
        &mut self,
        request: ScheduleTimerRequest,
    ) -> Result<ScheduledTimer, Self::Error> {
        self.next_timer_id += 1;
        Ok(ScheduledTimer {
            timer_id: format!("timer-{}", self.next_timer_id),
            delay: request.delay,
        })
    }
}

impl RandomBridge for LocalBridge {
    fn fill_random_bytes(&mut self, request: RandomBytesRequest) -> Result<Vec<u8>, Self::Error> {
        Ok(vec![0u8; request.len])
    }
}

impl EventBridge for LocalBridge {
    fn emit_structured_event(&mut self, _event: StructuredEventRecord) -> Result<(), Self::Error> {
        Ok(())
    }

    fn emit_diagnostic(&mut self, _event: DiagnosticRecord) -> Result<(), Self::Error> {
        Ok(())
    }

    fn emit_log(&mut self, _event: LogRecord) -> Result<(), Self::Error> {
        Ok(())
    }

    fn emit_lifecycle(&mut self, _event: LifecycleEventRecord) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl ExecutionBridge for LocalBridge {
    fn create_javascript_context(
        &mut self,
        _request: CreateJavascriptContextRequest,
    ) -> Result<GuestContextHandle, Self::Error> {
        Err(LocalBridgeError::unsupported(
            "execution bridge is handled internally by the native sidecar",
        ))
    }

    fn create_wasm_context(
        &mut self,
        _request: CreateWasmContextRequest,
    ) -> Result<GuestContextHandle, Self::Error> {
        Err(LocalBridgeError::unsupported(
            "execution bridge is handled internally by the native sidecar",
        ))
    }

    fn start_execution(
        &mut self,
        _request: StartExecutionRequest,
    ) -> Result<StartedExecution, Self::Error> {
        Err(LocalBridgeError::unsupported(
            "execution bridge is handled internally by the native sidecar",
        ))
    }

    fn write_stdin(&mut self, _request: WriteExecutionStdinRequest) -> Result<(), Self::Error> {
        Err(LocalBridgeError::unsupported(
            "execution bridge is handled internally by the native sidecar",
        ))
    }

    fn close_stdin(&mut self, _request: ExecutionHandleRequest) -> Result<(), Self::Error> {
        Err(LocalBridgeError::unsupported(
            "execution bridge is handled internally by the native sidecar",
        ))
    }

    fn kill_execution(&mut self, _request: KillExecutionRequest) -> Result<(), Self::Error> {
        Err(LocalBridgeError::unsupported(
            "execution bridge is handled internally by the native sidecar",
        ))
    }

    fn poll_execution_event(
        &mut self,
        _request: PollExecutionEventRequest,
    ) -> Result<Option<ExecutionEvent>, Self::Error> {
        Ok(None)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SessionScope {
    connection_id: String,
    session_id: String,
}

impl SessionScope {
    fn ownership_scope(&self) -> agent_os_sidecar::protocol::OwnershipScope {
        agent_os_sidecar::protocol::OwnershipScope::session(&self.connection_id, &self.session_id)
    }
}

struct FrameSidecarRequestTransport {
    writer: mpsc::Sender<ProtocolFrame>,
    pending: Arc<Mutex<BTreeMap<RequestId, mpsc::SyncSender<SidecarResponseFrame>>>>,
}

impl FrameSidecarRequestTransport {
    fn new(writer: mpsc::Sender<ProtocolFrame>) -> Self {
        Self {
            writer,
            pending: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    fn accept_response(&self, response: SidecarResponseFrame) -> bool {
        let sender = {
            let mut pending = match self.pending.lock() {
                Ok(pending) => pending,
                Err(_) => return false,
            };
            pending.remove(&response.request_id)
        };
        let Some(sender) = sender else {
            return false;
        };
        let _ = sender.send(response);
        true
    }
}

impl SidecarRequestTransport for FrameSidecarRequestTransport {
    fn send_request(
        &self,
        request: SidecarRequestFrame,
        timeout: Duration,
    ) -> Result<SidecarResponseFrame, SidecarError> {
        let (sender, receiver) = mpsc::sync_channel(1);
        self.pending
            .lock()
            .map_err(|_| {
                SidecarError::Bridge(String::from("sidecar callback waiter map lock poisoned"))
            })?
            .insert(request.request_id, sender);
        if let Err(error) = self
            .writer
            .send(ProtocolFrame::SidecarRequest(request.clone()))
        {
            let _ = self
                .pending
                .lock()
                .map(|mut pending| pending.remove(&request.request_id));
            return Err(SidecarError::Io(format!(
                "failed to write sidecar request frame: {error}"
            )));
        }
        match receiver.recv_timeout(timeout) {
            Ok(response) => Ok(response),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = self
                    .pending
                    .lock()
                    .map(|mut pending| pending.remove(&request.request_id));
                Err(SidecarError::Io(format!(
                    "timed out waiting for sidecar response after {}s",
                    timeout.as_secs()
                )))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(SidecarError::Io(String::from(
                "sidecar response waiter disconnected",
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LocalBridgeError {
    message: String,
}

impl LocalBridgeError {
    fn unsupported(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    fn io(operation: &str, path: &str, error: io::Error) -> Self {
        Self::unsupported(format!("{operation} {path}: {error}"))
    }
}

impl fmt::Display for LocalBridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for LocalBridgeError {}

impl LocalBridge {
    fn host_path(path: &str) -> PathBuf {
        let candidate = Path::new(path);
        if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(candidate)
        }
    }

    fn file_metadata(metadata: fs::Metadata) -> FileMetadata {
        FileMetadata {
            mode: metadata.permissions().mode(),
            size: metadata.size(),
            kind: Self::file_kind(metadata.file_type()),
        }
    }

    fn file_kind(file_type: fs::FileType) -> agent_os_bridge::FileKind {
        if file_type.is_file() {
            agent_os_bridge::FileKind::File
        } else if file_type.is_dir() {
            agent_os_bridge::FileKind::Directory
        } else if file_type.is_symlink() {
            agent_os_bridge::FileKind::SymbolicLink
        } else {
            agent_os_bridge::FileKind::Other
        }
    }
}
