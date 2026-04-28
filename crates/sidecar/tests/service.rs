pub trait NativeSidecarBridge: agent_os_bridge::HostBridge {}
impl<T> NativeSidecarBridge for T where T: agent_os_bridge::HostBridge {}

#[path = "../src/acp/mod.rs"]
mod acp;
#[path = "../src/bootstrap.rs"]
mod bootstrap;
#[path = "../src/bridge.rs"]
mod bridge;
#[path = "../src/execution.rs"]
mod execution;
#[path = "../src/filesystem.rs"]
mod filesystem;
#[path = "../src/plugins/mod.rs"]
mod plugins;
#[path = "../src/protocol.rs"]
mod protocol;
#[path = "../src/state.rs"]
mod state;
#[path = "../src/tools.rs"]
mod tools;
#[path = "../src/vm.rs"]
mod vm;

mod service {
    include!("../src/service.rs");

    mod tests {
        mod bridge_support {
            include!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../bridge/tests/support.rs"
            ));
        }

        use super::*;
        use crate::bridge::{bridge_permissions, HostFilesystem, ScopedHostFilesystem};
        use crate::execution::{
            clamp_javascript_net_poll_wait, format_dns_resource, format_tcp_resource,
            service_javascript_net_sync_rpc, signal_runtime_process,
        };
        use crate::filesystem::service_javascript_fs_sync_rpc;
        use crate::plugins::s3::test_support::MockS3Server;
        use crate::plugins::sandbox_agent::test_support::MockSandboxAgentServer;
        use crate::protocol::VmCreatedResponse;
        use crate::protocol::{
            AuthenticateRequest, BootstrapRootFilesystemRequest, CloseStdinRequest,
            ConfigureVmRequest, CreateSessionRequest, CreateVmRequest, DisposeReason,
            DisposeVmRequest, FindBoundUdpRequest, FindListenerRequest, FsPermissionRule,
            FsPermissionRuleSet, FsPermissionScope, GetProcessSnapshotRequest,
            GetZombieTimerCountRequest, GuestFilesystemCallRequest, GuestFilesystemOperation,
            GuestRuntimeKind, MountDescriptor, MountPluginDescriptor, OpenSessionRequest,
            OwnershipScope, PatternPermissionRule, PatternPermissionRuleSet,
            PatternPermissionScope, PermissionMode, PermissionsPolicy, RegisterToolkitRequest,
            RegisteredToolDefinition, RequestFrame, RequestPayload, ResponsePayload,
            RootFilesystemEntry, RootFilesystemEntryEncoding, RootFilesystemEntryKind,
            SidecarAcpResultResponse, SidecarPlacement, SidecarRequestFrame, SidecarRequestPayload,
            SidecarResponseFrame, SidecarResponsePayload, ToolInvocationResultResponse,
            WriteStdinRequest,
        };
        use crate::state::{
            ActiveExecution, ActiveExecutionEvent, ActiveProcess, ActiveTcpListener,
            ActiveUdpSocket, ProcessEventEnvelope, SidecarKernel, ToolExecution, VmListenPolicy,
            EXECUTION_SANDBOX_ROOT_ENV, JAVASCRIPT_COMMAND, LOOPBACK_EXEMPT_PORTS_ENV,
            PYTHON_COMMAND, VM_DNS_SERVERS_METADATA_KEY, VM_LISTEN_ALLOW_PRIVILEGED_METADATA_KEY,
            VM_LISTEN_PORT_MAX_METADATA_KEY, VM_LISTEN_PORT_MIN_METADATA_KEY, WASM_COMMAND,
            WASM_STDIO_SYNC_RPC_ENV,
        };
        use agent_os_bridge::{FileKind, SymlinkRequest};
        use agent_os_execution::{
            CreateJavascriptContextRequest, CreatePythonContextRequest, CreateWasmContextRequest,
            JavascriptSyncRpcRequest, PythonVfsRpcMethod, PythonVfsRpcRequest,
            StartJavascriptExecutionRequest, StartPythonExecutionRequest,
            StartWasmExecutionRequest, WasmPermissionTier,
        };
        use agent_os_kernel::command_registry::CommandDriver;
        use agent_os_kernel::kernel::{KernelVmConfig, SpawnOptions, VirtualProcessOptions};
        use agent_os_kernel::mount_table::{MountEntry, MountOptions, MountTable};
        use agent_os_kernel::permissions::{FsAccessRequest, FsOperation, Permissions};
        use agent_os_kernel::poll::{PollTargetEntry, POLLIN};
        use agent_os_kernel::process_table::SIGTERM;
        use agent_os_kernel::resource_accounting::ResourceLimits;
        use agent_os_kernel::vfs::{
            MemoryFileSystem, VfsError, VirtualDirEntry, VirtualFileSystem, VirtualStat,
        };
        use base64::Engine;
        use bridge_support::RecordingBridge;
        use hickory_resolver::proto::op::{Message, OpCode, Query};
        use hickory_resolver::proto::rr::domain::Name;
        use hickory_resolver::proto::rr::rdata::{
            A, AAAA, CAA, CNAME, MX, NAPTR, NS, PTR, SOA, SRV, TXT,
        };
        use hickory_resolver::proto::rr::{RData, Record, RecordType};
        use nix::fcntl::{flock, FlockArg};
        use nix::libc;
        use rustls::client::danger::{
            HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
        };
        use rustls::crypto::aws_lc_rs;
        use rustls::pki_types::{CertificateDer, ServerName};
        use rustls::{
            ClientConfig, ClientConnection, DigitallySignedStruct, RootCertStore, ServerConfig,
            ServerConnection, SignatureScheme,
        };
        use serde_json::{json, Map, Value};
        use socket2::SockRef;
        use std::collections::BTreeMap;
        use std::fs;
        use std::fs::OpenOptions;
        use std::io::{BufReader, Read, Write};
        use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, UdpSocket};
        use std::os::fd::AsRawFd;
        use std::path::{Path, PathBuf};
        use std::process::Command;
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, Barrier, Mutex, OnceLock,
        };
        use std::thread;
        use std::time::{Duration, SystemTime, UNIX_EPOCH};

        const TEST_AUTH_TOKEN: &str = "sidecar-test-token";
        const TLS_TEST_KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----\n\
MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQClvETzHfSyd1Y+\n\
sjCfGkuyGxFMzwQlYjUrE0iwdMF774LYHFdpvtEo3sLOW6/b1xfXS/55jq+aggxS\n\
v+vgtjrhGf/y33XzdrjxcVBRWIsgAtxMHsNKO4EQ/uA1g6zlbaSIu+ZWX3bkDuTi\n\
K45VW69M0XSVyv8XFGYOcf8LTI87gTtXHuT92iej77IM2lHqLXCzQVr+NQ9yvXld\n\
9yHlA2ZfYqhkSTLdDablqfgirrQIzZzLypSGQwZUU06nCtZ+dg6SNV4TGL4NqekD\n\
jXR3BvmZu5l4sGAsNfFVjLx6hxsLt8uqn65sCAwBDdfucR+39+pHA+esj6NAWAFO\n\
J9CB94sfAgMBAAECggEABQTA772x+a98aJSbvU2eCiwgp3tDTGB/bKj+U/2NGFQl\n\
2aZuDTEugzbPnlEPb7BBNA9EiujDr4GNnvnZyimqecOASRn0J+Wp7wG35Waxe8wq\n\
YJGz5y0LGPkmz+gHVcEusMdDz8y/PGOpEaIxAquukLxs89Y8SDYhawGPsAdm9O3F\n\
4a+aosyQwS26mkZ/1WZOTsOVd4A1/1pxBvsANURj+pq7ed/1WqgrZBN/BG1TX5Xm\n\
DZeYy01kTCMWtcAb4f8PxGpbkSGMvBb+Mj5XtZByvfQeC+Cs5ECXhmJtVaYVUHhT\n\
vI0oTMGvit9ffoYNds0qTeZpEeineaDH3sD16D037QKBgQDX5b65KfIVH0/WvcbJ\n\
Gx2Wh7knXdDBky40wdq4buKK+ImzPPRxOsQ+xEMgEaZs8gb7LBapbB0cZ+YsKBOt\n\
4FY86XQU5V5ju2ntldIIIaugIGgvGS0jdRMH3ux6iEjPZE6Fm7/s8bjIgqB7keWh\n\
1rcZwDrwMzqwAUoBTJX58OY/fQKBgQDEhT5U7TqgEFVSspYh8c8yVRV9udiphPH3\n\
3XIbo9iV3xzNFdwtNHC+2eLM+4J3WKjhB0UvzrlIegSqKPIsy+0nD1uzaU+O72gg\n\
7+NKSh0RT61UDolk+P4s/2+5tnZqSNYO7Sd/svE/rkwIEtDEI5tb1nqq75h/HDEW\n\
k56GHAxvywKBgGmGmTdmIjZizKJYti4b+9VU15I/T8ceCmqtChw1zrNAkgWy2IPz\n\
xnIreefV2LPNhM4GGbmL55q3yhBxMlU9nsk9DokcJ4u10ivXnAZvdrTYwjOrKZ34\n\
HmotcwbdUEFWdO7nVuMYr0oKVyivAj+ddHe4ttYrJBddOe/yoCe/sLr9AoGBAKHL\n\
IVpCRXXqfJStOzWPI4rIyfzMuTg3oA71XjCrYHFjUw715GPDPN+j+znQB8XCVKeP\n\
mMKXa6vj6Vs+gsOm0QTLfC/lj/6Z1Bzp4zMSeYP7GTSPE0bySDE7y/wV4L/4X2PC\n\
lDZqWHyZPzeWZhJVTl754dxBjkd4KmHv/x9ikEqpAoGBAJNA0u0fKhdWDz32+a2F\n\
+plJ18kQvGuwKFWIIVHBDc0wCxLKWKr5wgkhdcAEpy4mgosiZ09DzV/OpQBBHVWZ\n\
v/Cn/DwZyoiXIi5onf7AqWIhw+aem+oMbugbSIYqDwYkwnN79tsza0KC1ScphIuf\n\
vKoOAdY4xOcG9BEZZoKVOa8R\n\
-----END PRIVATE KEY-----\n";
        const TLS_TEST_CERT_PEM: &str = "-----BEGIN CERTIFICATE-----\n\
MIIDCTCCAfGgAwIBAgIUJqRgTEIlpbfqbQnyo9hxLyIn3qYwDQYJKoZIhvcNAQEL\n\
BQAwFDESMBAGA1UEAwwJbG9jYWxob3N0MB4XDTI2MDQwNTA3MTAwOVoXDTI2MDQw\n\
NjA3MTAwOVowFDESMBAGA1UEAwwJbG9jYWxob3N0MIIBIjANBgkqhkiG9w0BAQEF\n\
AAOCAQ8AMIIBCgKCAQEApbxE8x30sndWPrIwnxpLshsRTM8EJWI1KxNIsHTBe++C\n\
2BxXab7RKN7Czluv29cX10v+eY6vmoIMUr/r4LY64Rn/8t9183a48XFQUViLIALc\n\
TB7DSjuBEP7gNYOs5W2kiLvmVl925A7k4iuOVVuvTNF0lcr/FxRmDnH/C0yPO4E7\n\
Vx7k/dono++yDNpR6i1ws0Fa/jUPcr15Xfch5QNmX2KoZEky3Q2m5an4Iq60CM2c\n\
y8qUhkMGVFNOpwrWfnYOkjVeExi+DanpA410dwb5mbuZeLBgLDXxVYy8eocbC7fL\n\
qp+ubAgMAQ3X7nEft/fqRwPnrI+jQFgBTifQgfeLHwIDAQABo1MwUTAdBgNVHQ4E\n\
FgQUwViZyKE6S2vgTAkexnZFccSwoPMwHwYDVR0jBBgwFoAUwViZyKE6S2vgTAke\n\
xnZFccSwoPMwDwYDVR0TAQH/BAUwAwEB/zANBgkqhkiG9w0BAQsFAAOCAQEAadmK\n\
3Ugrvep6glHAfgPP54um9cjJZQZDPn5I7yvgDr/Zp/u/UMW/OUKSfL1VNHlbAVLc\n\
Yzq2RVTrJKObiTSoy99OzYkEdgfuEBBP7XBEQlqoOGYNRR+IZXBBiQ+m9CtajNwQ\n\
G6mr9//zZtV1y2UUBgtxVpry5iOekpkr8iXyDLnGpS2gKL5dwXCzWCKVCO3qVotn\n\
r6FBg4DCBMkwO6xOVN2yInPd6CPy/JAUPW50zWPnn4DKfeAAU0C+E75HN65jozdi\n\
12yT4K772P8oSecGPInZhqJgOv1q0BDG8gccOxX1PA4sE00Enqlbvxz7sku9y4zp\n\
ykAheWCsAteSEWVc0w==\n\
-----END CERTIFICATE-----\n";
        const ACP_GRACEFUL_TERMINATION_AGENT: &str = r#"
import fs from "node:fs";

let buffer = "";
let sessionId = "mock-session-1";
let cancelSeen = false;

function writeMessage(message) {
  process.stdout.write(JSON.stringify(message) + "\n");
}

process.stdin.resume();
process.stdin.on("data", (chunk) => {
  buffer += chunk instanceof Uint8Array ? new TextDecoder().decode(chunk) : String(chunk);

  while (true) {
    const newlineIndex = buffer.indexOf("\n");
    if (newlineIndex === -1) break;
    const line = buffer.slice(0, newlineIndex);
    buffer = buffer.slice(newlineIndex + 1);
    if (!line.trim()) continue;

    const message = JSON.parse(line);
    if (message.method === "session/cancel") {
      cancelSeen = true;
      fs.writeFileSync("/workspace/cancel.json", JSON.stringify({ cancelSeen: true }));
    }

    if (message.id === undefined) {
      continue;
    }

    switch (message.method) {
      case "initialize":
        writeMessage({
          jsonrpc: "2.0",
          id: message.id,
          result: {
            protocolVersion: 1,
            agentInfo: { name: "mock-acp", version: "1.0.0" },
          },
        });
        break;
      case "session/new":
        if (message.params && typeof message.params.sessionId === "string") {
          sessionId = message.params.sessionId;
        }
        writeMessage({
          jsonrpc: "2.0",
          id: message.id,
          result: { sessionId },
        });
        break;
      default:
        writeMessage({
          jsonrpc: "2.0",
          id: message.id,
          error: { code: -32601, message: "Method not found" },
        });
    }
  }
});

process.on("SIGTERM", () => {
  fs.writeFileSync(
    "/workspace/sigterm.json",
    JSON.stringify({ cancelSeen, phase: "sigterm" }),
  );
  setTimeout(() => process.exit(0), 10);
});

setInterval(() => {}, 1000);
"#;
        const ACP_IGNORE_TERM_AGENT: &str = r#"
import fs from "node:fs";

let buffer = "";
let sessionId = "mock-session-1";

function writeMessage(message) {
  process.stdout.write(JSON.stringify(message) + "\n");
}

process.stdin.resume();
process.stdin.on("data", (chunk) => {
  buffer += chunk instanceof Uint8Array ? new TextDecoder().decode(chunk) : String(chunk);

  while (true) {
    const newlineIndex = buffer.indexOf("\n");
    if (newlineIndex === -1) break;
    const line = buffer.slice(0, newlineIndex);
    buffer = buffer.slice(newlineIndex + 1);
    if (!line.trim()) continue;

    const message = JSON.parse(line);
    if (message.id === undefined) {
      continue;
    }

    switch (message.method) {
      case "initialize":
        writeMessage({
          jsonrpc: "2.0",
          id: message.id,
          result: {
            protocolVersion: 1,
            agentInfo: { name: "mock-acp", version: "1.0.0" },
          },
        });
        break;
      case "session/new":
        if (message.params && typeof message.params.sessionId === "string") {
          sessionId = message.params.sessionId;
        }
        writeMessage({
          jsonrpc: "2.0",
          id: message.id,
          result: { sessionId },
        });
        break;
      default:
        writeMessage({
          jsonrpc: "2.0",
          id: message.id,
          error: { code: -32601, message: "Method not found" },
        });
    }
  }
});

process.on("SIGTERM", () => {
  fs.writeFileSync("/workspace/sigterm-ignored.json", JSON.stringify({ sessionId }));
});

setInterval(() => {}, 1000);
"#;

        fn request(
            request_id: agent_os_sidecar::protocol::RequestId,
            ownership: OwnershipScope,
            payload: RequestPayload,
        ) -> RequestFrame {
            RequestFrame::new(request_id, ownership, payload)
        }

        fn acquire_sidecar_runtime_test_lock() {
            static LOCK_FILE: OnceLock<std::fs::File> = OnceLock::new();
            let _ = LOCK_FILE.get_or_init(|| {
                let path = std::env::temp_dir().join("agent-os-sidecar-runtime-tests.lock");
                let file = OpenOptions::new()
                    .create(true)
                    .read(true)
                    .write(true)
                    .open(&path)
                    .unwrap_or_else(|error| {
                        panic!("open sidecar test runtime lock {}: {error}", path.display())
                    });
                flock(file.as_raw_fd(), FlockArg::LockExclusive).unwrap_or_else(|error| {
                    panic!("lock sidecar test runtime {}: {error}", path.display())
                });
                file
            });
        }

        fn create_test_sidecar_with_config(
            config: NativeSidecarConfig,
        ) -> NativeSidecar<RecordingBridge> {
            acquire_sidecar_runtime_test_lock();
            NativeSidecar::with_config(
                RecordingBridge::default(),
                NativeSidecarConfig {
                    sidecar_id: String::from("sidecar-test"),
                    compile_cache_root: Some(
                        std::env::temp_dir().join("agent-os-sidecar-test-cache"),
                    ),
                    expected_auth_token: Some(String::from(TEST_AUTH_TOKEN)),
                    ..config
                },
            )
            .expect("create sidecar")
        }
        fn create_test_sidecar() -> NativeSidecar<RecordingBridge> {
            create_test_sidecar_with_config(NativeSidecarConfig::default())
        }
        fn session_timeout_response_includes_structured_diagnostics() {
            let mut session = AcpSessionState::new(
                String::from("acp-session-1"),
                String::from("vm-1"),
                String::from("codex"),
                String::from("acp-agent-1"),
                Some(42),
                &Map::new(),
                &Map::new(),
            );
            session.record_activity(String::from("sent request session/prompt id=7"));
            session.record_activity(String::from("received notification session/update"));
            session.exit_code = Some(137);
            session.mark_termination_requested();

            let diagnostics = NativeSidecar::<RecordingBridge>::session_timeout_diagnostics(
                &session,
                "session/prompt",
                &JsonRpcId::Number(7),
            );
            let response = NativeSidecar::<RecordingBridge>::session_timeout_response(
                JsonRpcId::Number(7),
                diagnostics,
            );

            let error = response.into_error().expect("timeout error");
            assert!(error.message.contains("process exitCode=137"));
            assert!(error.message.contains("killed=true"));

            let data = error.data.expect("timeout diagnostics data");
            assert_eq!(data["kind"], json!("acp_timeout"));
            assert_eq!(data["method"], json!("session/prompt"));
            assert_eq!(data["id"], json!(7));
            assert_eq!(data["timeoutMs"], json!(120000));
            assert_eq!(data["exitCode"], json!(137));
            assert_eq!(data["killed"], json!(true));
            assert_eq!(
                data["recentActivity"],
                json!([
                    "sent request session/prompt id=7",
                    "received notification session/update"
                ])
            );
        }

        fn create_kernel_process_handle_for_tests() -> agent_os_kernel::kernel::KernelProcessHandle
        {
            let mut config = KernelVmConfig::new("vm-js-crypto-rpc");
            config.permissions = Permissions::allow_all();
            let mut kernel = SidecarKernel::new(MountTable::new(MemoryFileSystem::new()), config);
            kernel
                .register_driver(CommandDriver::new(
                    EXECUTION_DRIVER_NAME,
                    [JAVASCRIPT_COMMAND],
                ))
                .expect("register execution driver");
            kernel
                .spawn_process(
                    JAVASCRIPT_COMMAND,
                    Vec::new(),
                    SpawnOptions {
                        requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                        ..SpawnOptions::default()
                    },
                )
                .expect("spawn javascript kernel process")
        }

        fn create_active_execution_for_tests() -> ActiveExecution {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate sidecar");
            let vm_id = create_vm_with_metadata(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
                BTreeMap::new(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-js-crypto-rpc");
            write_fixture(&cwd.join("entry.mjs"), "export {};\n");
            let context = sidecar.javascript_engine.create_context(
                agent_os_execution::CreateJavascriptContextRequest {
                    vm_id: vm_id.clone(),
                    bootstrap_module: None,
                    compile_cache_root: None,
                },
            );
            let execution = sidecar
                .javascript_engine
                .start_execution(agent_os_execution::StartJavascriptExecutionRequest {
                    vm_id,
                    context_id: context.context_id,
                    argv: vec![String::from("./entry.mjs")],
                    env: BTreeMap::new(),
                    cwd,
                    inline_code: Some(String::from("")),
                })
                .expect("start javascript execution");
            ActiveExecution::Javascript(execution)
        }

        fn create_crypto_test_process() -> ActiveProcess {
            let kernel_handle = create_kernel_process_handle_for_tests();
            ActiveProcess::new(
                kernel_handle.pid(),
                kernel_handle,
                GuestRuntimeKind::JavaScript,
                create_active_execution_for_tests(),
            )
        }

        #[derive(Debug, Clone, PartialEq, Eq)]
        struct JsBridgeCallRecord {
            ownership: OwnershipScope,
            mount_id: String,
            operation: String,
            path: Option<String>,
        }

        fn js_bridge_result(
            request: SidecarRequestFrame,
            result: Option<Value>,
            error: Option<&str>,
        ) -> Result<SidecarResponsePayload, SidecarError> {
            let SidecarRequestPayload::JsBridgeCall(call) = request.payload else {
                return Err(SidecarError::InvalidState(String::from(
                    "expected js_bridge_call payload",
                )));
            };
            Ok(SidecarResponsePayload::JsBridgeResult(
                crate::protocol::JsBridgeResultResponse {
                    call_id: call.call_id,
                    result,
                    error: error.map(String::from),
                },
            ))
        }

        fn stat_json(stat: VirtualStat) -> Value {
            json!({
                "mode": stat.mode,
                "size": stat.size,
                "blocks": stat.blocks,
                "dev": stat.dev,
                "rdev": stat.rdev,
                "isDirectory": stat.is_directory,
                "isSymbolicLink": stat.is_symbolic_link,
                "atimeMs": stat.atime_ms,
                "mtimeMs": stat.mtime_ms,
                "ctimeMs": stat.ctime_ms,
                "birthtimeMs": stat.birthtime_ms,
                "ino": stat.ino,
                "nlink": stat.nlink,
                "uid": stat.uid,
                "gid": stat.gid,
            })
        }

        fn dir_entry_json(entry: VirtualDirEntry) -> Value {
            json!({
                "name": entry.name,
                "isDirectory": entry.is_directory,
                "isSymbolicLink": entry.is_symbolic_link,
            })
        }

        fn install_memory_js_bridge_handler(
            sidecar: &mut NativeSidecar<RecordingBridge>,
        ) -> (
            Arc<Mutex<MemoryFileSystem>>,
            Arc<Mutex<Vec<JsBridgeCallRecord>>>,
        ) {
            let filesystem = Arc::new(Mutex::new(MemoryFileSystem::new()));
            let calls = Arc::new(Mutex::new(Vec::<JsBridgeCallRecord>::new()));
            let handler_filesystem = filesystem.clone();
            let handler_calls = calls.clone();

            sidecar.set_sidecar_request_handler(move |request| {
                let ownership = request.ownership.clone();
                let SidecarRequestPayload::JsBridgeCall(call) = &request.payload else {
                    return Err(SidecarError::InvalidState(String::from(
                        "expected js_bridge_call payload",
                    )));
                };
                handler_calls
                    .lock()
                    .expect("lock js bridge calls")
                    .push(JsBridgeCallRecord {
                        ownership,
                        mount_id: call.mount_id.clone(),
                        operation: call.operation.clone(),
                        path: call
                            .args
                            .get("path")
                            .and_then(Value::as_str)
                            .map(String::from),
                    });

                let mut filesystem = handler_filesystem.lock().expect("lock js bridge fs");
                let response: Result<Option<Value>, String> = match call.operation.as_str() {
                    "readFile" => {
                        let path = call.args["path"].as_str().expect("readFile path");
                        filesystem
                            .read_file(path)
                            .map(|bytes| {
                                Some(Value::String(
                                    base64::engine::general_purpose::STANDARD.encode(bytes),
                                ))
                            })
                            .map_err(|error| format!("{}: {error}", error.code()))
                    }
                    "readDir" => {
                        let path = call.args["path"].as_str().expect("readDir path");
                        filesystem
                            .read_dir(path)
                            .map(|entries| Some(json!(entries)))
                            .map_err(|error| format!("{}: {error}", error.code()))
                    }
                    "readDirWithTypes" => {
                        let path = call.args["path"].as_str().expect("readDirWithTypes path");
                        filesystem
                            .read_dir_with_types(path)
                            .map(|entries| {
                                Some(Value::Array(
                                    entries.into_iter().map(dir_entry_json).collect(),
                                ))
                            })
                            .map_err(|error| format!("{}: {error}", error.code()))
                    }
                    "writeFile" => {
                        let path = call.args["path"].as_str().expect("writeFile path");
                        let content = call.args["content"].as_str().expect("writeFile content");
                        let bytes = base64::engine::general_purpose::STANDARD
                            .decode(content)
                            .expect("decode js bridge write content");
                        filesystem
                            .write_file(path, bytes)
                            .map(|()| None)
                            .map_err(|error| format!("{}: {error}", error.code()))
                    }
                    "createDir" => {
                        let path = call.args["path"].as_str().expect("createDir path");
                        filesystem
                            .create_dir(path)
                            .map(|()| None)
                            .map_err(|error| format!("{}: {error}", error.code()))
                    }
                    "mkdir" => {
                        let path = call.args["path"].as_str().expect("mkdir path");
                        let recursive = call.args["recursive"].as_bool().unwrap_or(false);
                        filesystem
                            .mkdir(path, recursive)
                            .map(|()| None)
                            .map_err(|error| format!("{}: {error}", error.code()))
                    }
                    "exists" => {
                        let path = call.args["path"].as_str().expect("exists path");
                        Ok(Some(Value::Bool(filesystem.exists(path))))
                    }
                    "stat" => {
                        let path = call.args["path"].as_str().expect("stat path");
                        filesystem
                            .stat(path)
                            .map(|stat| Some(stat_json(stat)))
                            .map_err(|error| format!("{}: {error}", error.code()))
                    }
                    "removeFile" => {
                        let path = call.args["path"].as_str().expect("removeFile path");
                        filesystem
                            .remove_file(path)
                            .map(|()| None)
                            .map_err(|error| format!("{}: {error}", error.code()))
                    }
                    "removeDir" => {
                        let path = call.args["path"].as_str().expect("removeDir path");
                        filesystem
                            .remove_dir(path)
                            .map(|()| None)
                            .map_err(|error| format!("{}: {error}", error.code()))
                    }
                    "rename" => {
                        let old_path = call.args["oldPath"].as_str().expect("rename oldPath");
                        let new_path = call.args["newPath"].as_str().expect("rename newPath");
                        filesystem
                            .rename(old_path, new_path)
                            .map(|()| None)
                            .map_err(|error| format!("{}: {error}", error.code()))
                    }
                    "realpath" => {
                        let path = call.args["path"].as_str().expect("realpath path");
                        filesystem
                            .realpath(path)
                            .map(|resolved| Some(json!(resolved)))
                            .map_err(|error| format!("{}: {error}", error.code()))
                    }
                    "symlink" => {
                        let target = call.args["target"].as_str().expect("symlink target");
                        let link_path = call.args["linkPath"].as_str().expect("symlink linkPath");
                        filesystem
                            .symlink(target, link_path)
                            .map(|()| None)
                            .map_err(|error| format!("{}: {error}", error.code()))
                    }
                    "readlink" => {
                        let path = call.args["path"].as_str().expect("readlink path");
                        filesystem
                            .read_link(path)
                            .map(|target| Some(json!(target)))
                            .map_err(|error| format!("{}: {error}", error.code()))
                    }
                    "lstat" => {
                        let path = call.args["path"].as_str().expect("lstat path");
                        filesystem
                            .lstat(path)
                            .map(|stat| Some(stat_json(stat)))
                            .map_err(|error| format!("{}: {error}", error.code()))
                    }
                    "link" => {
                        let old_path = call.args["oldPath"].as_str().expect("link oldPath");
                        let new_path = call.args["newPath"].as_str().expect("link newPath");
                        filesystem
                            .link(old_path, new_path)
                            .map(|()| None)
                            .map_err(|error| format!("{}: {error}", error.code()))
                    }
                    "chmod" => {
                        let path = call.args["path"].as_str().expect("chmod path");
                        let mode = call.args["mode"].as_u64().expect("chmod mode") as u32;
                        filesystem
                            .chmod(path, mode)
                            .map(|()| None)
                            .map_err(|error| format!("{}: {error}", error.code()))
                    }
                    "chown" => {
                        let path = call.args["path"].as_str().expect("chown path");
                        let uid = call.args["uid"].as_u64().expect("chown uid") as u32;
                        let gid = call.args["gid"].as_u64().expect("chown gid") as u32;
                        filesystem
                            .chown(path, uid, gid)
                            .map(|()| None)
                            .map_err(|error| format!("{}: {error}", error.code()))
                    }
                    "utimes" => {
                        let path = call.args["path"].as_str().expect("utimes path");
                        let atime = call.args["atimeMs"].as_u64().expect("utimes atimeMs");
                        let mtime = call.args["mtimeMs"].as_u64().expect("utimes mtimeMs");
                        filesystem
                            .utimes(path, atime, mtime)
                            .map(|()| None)
                            .map_err(|error| format!("{}: {error}", error.code()))
                    }
                    "truncate" => {
                        let path = call.args["path"].as_str().expect("truncate path");
                        let length = call.args["length"].as_u64().expect("truncate length");
                        filesystem
                            .truncate(path, length)
                            .map(|()| None)
                            .map_err(|error| format!("{}: {error}", error.code()))
                    }
                    "pread" => {
                        let path = call.args["path"].as_str().expect("pread path");
                        let offset = call.args["offset"].as_u64().expect("pread offset");
                        let length = call.args["length"].as_u64().expect("pread length") as usize;
                        filesystem
                            .pread(path, offset, length)
                            .map(|bytes| {
                                Some(Value::String(
                                    base64::engine::general_purpose::STANDARD.encode(bytes),
                                ))
                            })
                            .map_err(|error| format!("{}: {error}", error.code()))
                    }
                    "pwrite" => {
                        let path = call.args["path"].as_str().expect("pwrite path");
                        let offset = call.args["offset"].as_u64().expect("pwrite offset");
                        let content = call.args["content"].as_str().expect("pwrite content");
                        let bytes = base64::engine::general_purpose::STANDARD
                            .decode(content)
                            .expect("decode js bridge pwrite content");
                        filesystem
                            .pwrite(path, bytes, offset)
                            .map(|()| None)
                            .map_err(|error| format!("{}: {error}", error.code()))
                    }
                    other => {
                        return Err(SidecarError::Unsupported(format!(
                            "unsupported op: {other}"
                        )));
                    }
                };

                match response {
                    Ok(result) => js_bridge_result(request, result, None),
                    Err(error) => js_bridge_result(request, None, Some(&error)),
                }
            });

            (filesystem, calls)
        }

        fn unexpected_response_error(expected: &str, other: ResponsePayload) -> SidecarError {
            SidecarError::InvalidState(format!("expected {expected} response, got {other:?}"))
        }

        fn authenticated_connection_id(auth: DispatchResult) -> Result<String, SidecarError> {
            match auth.response.payload {
                ResponsePayload::Authenticated(response) => {
                    assert_eq!(
                        auth.response.ownership,
                        OwnershipScope::connection(&response.connection_id)
                    );
                    Ok(response.connection_id)
                }
                other => Err(unexpected_response_error("authenticated", other)),
            }
        }

        fn opened_session_id(session: DispatchResult) -> Result<String, SidecarError> {
            match session.response.payload {
                ResponsePayload::SessionOpened(response) => Ok(response.session_id),
                other => Err(unexpected_response_error("session_opened", other)),
            }
        }

        fn created_vm_id(response: DispatchResult) -> Result<String, SidecarError> {
            match response.response.payload {
                ResponsePayload::VmCreated(response) => Ok(response.vm_id),
                other => Err(unexpected_response_error("vm_created", other)),
            }
        }

        fn authenticate_and_open_session(
            sidecar: &mut NativeSidecar<RecordingBridge>,
        ) -> Result<(String, String), SidecarError> {
            let auth = sidecar
                .dispatch_blocking(request(
                    1,
                    OwnershipScope::connection("conn-1"),
                    RequestPayload::Authenticate(AuthenticateRequest {
                        client_name: String::from("service-tests"),
                        auth_token: String::from(TEST_AUTH_TOKEN),
                        bridge_version: agent_os_bridge::bridge_contract().version,
                    }),
                ))
                .expect("authenticate");
            let connection_id = authenticated_connection_id(auth)?;

            let session = sidecar
                .dispatch_blocking(request(
                    2,
                    OwnershipScope::connection(&connection_id),
                    RequestPayload::OpenSession(OpenSessionRequest {
                        placement: SidecarPlacement::Shared { pool: None },
                        metadata: BTreeMap::new(),
                    }),
                ))
                .expect("open session");
            let session_id = opened_session_id(session)?;
            Ok((connection_id, session_id))
        }

        fn create_vm(
            sidecar: &mut NativeSidecar<RecordingBridge>,
            connection_id: &str,
            session_id: &str,
            permissions: PermissionsPolicy,
        ) -> Result<String, SidecarError> {
            create_vm_with_metadata(
                sidecar,
                connection_id,
                session_id,
                permissions,
                BTreeMap::new(),
            )
        }

        fn create_vm_with_metadata(
            sidecar: &mut NativeSidecar<RecordingBridge>,
            connection_id: &str,
            session_id: &str,
            permissions: PermissionsPolicy,
            metadata: BTreeMap<String, String>,
        ) -> Result<String, SidecarError> {
            let response = sidecar
                .dispatch_blocking(request(
                    3,
                    OwnershipScope::session(connection_id, session_id),
                    RequestPayload::CreateVm(CreateVmRequest {
                        runtime: GuestRuntimeKind::JavaScript,
                        metadata,
                        root_filesystem: Default::default(),
                        permissions: Some(permissions),
                    }),
                ))
                .expect("create vm");

            created_vm_id(response)
        }

        fn create_mock_acp_session(
            sidecar: &mut NativeSidecar<RecordingBridge>,
            connection_id: &str,
            session_id: &str,
            vm_id: &str,
            entrypoint_name: &str,
            source: &str,
        ) -> (String, u32) {
            {
                let vm = sidecar.vms.get_mut(vm_id).expect("mock ACP vm");
                let entrypoint_path = format!("/workspace/{entrypoint_name}");
                vm.kernel
                    .write_file(&entrypoint_path, source.as_bytes().to_vec())
                    .expect("write mock ACP agent entrypoint");
            }

            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build local runtime for mock ACP session");
            let local = tokio::task::LocalSet::new();
            let response = runtime
                .block_on(local.run_until(async {
                    sidecar
                        .dispatch(request(
                            4,
                            OwnershipScope::vm(connection_id, session_id, vm_id),
                            RequestPayload::CreateSession(CreateSessionRequest {
                                agent_type: String::from("mock-acp"),
                                runtime: GuestRuntimeKind::JavaScript,
                                adapter_entrypoint: format!("/workspace/{entrypoint_name}"),
                                args: Vec::new(),
                                env: BTreeMap::new(),
                                cwd: String::from("/workspace"),
                                mcp_servers: Vec::new(),
                                protocol_version: 1,
                                client_capabilities: json!({}),
                            }),
                        ))
                        .await
                }))
                .expect("create mock ACP session");

            match response.response.payload {
                ResponsePayload::SessionCreated(created) => {
                    (created.session_id, created.pid.expect("mock ACP pid"))
                }
                other => panic!("unexpected create session response: {other:?}"),
            }
        }

        fn empty_permissions_policy() -> PermissionsPolicy {
            PermissionsPolicy {
                fs: None,
                network: None,
                child_process: None,
                process: None,
                env: None,
                tool: None,
            }
        }

        fn capability_permissions(entries: &[(&str, PermissionMode)]) -> PermissionsPolicy {
            let mut policy = empty_permissions_policy();

            for (capability, mode) in entries {
                match *capability {
                    "fs" => policy.fs = Some(FsPermissionScope::Mode(mode.clone())),
                    "network" => policy.network = Some(PatternPermissionScope::Mode(mode.clone())),
                    "child_process" => {
                        policy.child_process = Some(PatternPermissionScope::Mode(mode.clone()));
                    }
                    "process" => {
                        policy.process = Some(PatternPermissionScope::Mode(mode.clone()));
                    }
                    "env" => policy.env = Some(PatternPermissionScope::Mode(mode.clone())),
                    "tool" => policy.tool = Some(PatternPermissionScope::Mode(mode.clone())),
                    _ if capability.starts_with("fs.") => {
                        append_fs_rule(
                            &mut policy,
                            capability.trim_start_matches("fs."),
                            mode.clone(),
                        );
                    }
                    _ if capability.starts_with("network.") => {
                        append_pattern_rule(
                            &mut policy.network,
                            capability.trim_start_matches("network."),
                            mode.clone(),
                        );
                    }
                    _ if capability.starts_with("child_process.") => {
                        append_pattern_rule(
                            &mut policy.child_process,
                            capability.trim_start_matches("child_process."),
                            mode.clone(),
                        );
                    }
                    _ if capability.starts_with("process.") => {
                        append_pattern_rule(
                            &mut policy.process,
                            capability.trim_start_matches("process."),
                            mode.clone(),
                        );
                    }
                    _ if capability.starts_with("env.") => {
                        append_pattern_rule(
                            &mut policy.env,
                            capability.trim_start_matches("env."),
                            mode.clone(),
                        );
                    }
                    _ if capability.starts_with("tool.") => {
                        append_pattern_rule(
                            &mut policy.tool,
                            capability.trim_start_matches("tool."),
                            mode.clone(),
                        );
                    }
                    _ => panic!("unsupported test capability {capability}"),
                }
            }

            policy
        }

        fn test_toolkit_payload(
            name: &str,
            description: &str,
            tool_name: &str,
        ) -> RegisterToolkitRequest {
            test_toolkit_payload_with_schema(
                name,
                description,
                tool_name,
                json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false,
                }),
            )
        }

        fn test_toolkit_payload_with_schema(
            name: &str,
            description: &str,
            tool_name: &str,
            input_schema: Value,
        ) -> RegisterToolkitRequest {
            RegisterToolkitRequest {
                name: String::from(name),
                description: String::from(description),
                tools: BTreeMap::from([(
                    String::from(tool_name),
                    RegisteredToolDefinition {
                        description: format!("{tool_name} tool"),
                        input_schema,
                        timeout_ms: None,
                        examples: Vec::new(),
                    },
                )]),
            }
        }

        fn append_fs_rule(policy: &mut PermissionsPolicy, operation: &str, mode: PermissionMode) {
            let scope = policy
                .fs
                .take()
                .unwrap_or(FsPermissionScope::Rules(FsPermissionRuleSet {
                    default: None,
                    rules: Vec::new(),
                }));
            policy.fs = Some(match scope {
                FsPermissionScope::Mode(existing) => {
                    FsPermissionScope::Rules(FsPermissionRuleSet {
                        default: Some(existing),
                        rules: vec![FsPermissionRule {
                            mode,
                            operations: vec![operation.to_owned()],
                            paths: vec![String::from("/**")],
                        }],
                    })
                }
                FsPermissionScope::Rules(mut rules) => {
                    rules.rules.push(FsPermissionRule {
                        mode,
                        operations: vec![operation.to_owned()],
                        paths: vec![String::from("/**")],
                    });
                    FsPermissionScope::Rules(rules)
                }
            });
        }

        fn append_pattern_rule(
            scope: &mut Option<PatternPermissionScope>,
            operation: &str,
            mode: PermissionMode,
        ) {
            let existing =
                scope
                    .take()
                    .unwrap_or(PatternPermissionScope::Rules(PatternPermissionRuleSet {
                        default: None,
                        rules: Vec::new(),
                    }));
            *scope = Some(match existing {
                PatternPermissionScope::Mode(default) => {
                    PatternPermissionScope::Rules(PatternPermissionRuleSet {
                        default: Some(default),
                        rules: vec![PatternPermissionRule {
                            mode,
                            operations: vec![operation.to_owned()],
                            patterns: vec![String::from("**")],
                        }],
                    })
                }
                PatternPermissionScope::Rules(mut rules) => {
                    rules.rules.push(PatternPermissionRule {
                        mode,
                        operations: vec![operation.to_owned()],
                        patterns: vec![String::from("**")],
                    });
                    PatternPermissionScope::Rules(rules)
                }
            });
        }

        fn inspect_permissions(network: bool, process: bool) -> PermissionsPolicy {
            PermissionsPolicy {
                fs: None,
                network: Some(PatternPermissionScope::Rules(PatternPermissionRuleSet {
                    default: Some(PermissionMode::Deny),
                    rules: vec![
                        PatternPermissionRule {
                            mode: PermissionMode::Allow,
                            operations: vec![String::from("listen")],
                            patterns: vec![String::from("**")],
                        },
                        PatternPermissionRule {
                            mode: if network {
                                PermissionMode::Allow
                            } else {
                                PermissionMode::Deny
                            },
                            operations: vec![String::from("inspect")],
                            patterns: vec![String::from("**")],
                        },
                    ],
                })),
                child_process: Some(PatternPermissionScope::Mode(PermissionMode::Allow)),
                process: Some(PatternPermissionScope::Rules(PatternPermissionRuleSet {
                    default: Some(PermissionMode::Deny),
                    rules: vec![PatternPermissionRule {
                        mode: if process {
                            PermissionMode::Allow
                        } else {
                            PermissionMode::Deny
                        },
                        operations: vec![String::from("inspect")],
                        patterns: vec![String::from("**")],
                    }],
                })),
                env: None,
                tool: None,
            }
        }

        fn temp_dir(prefix: &str) -> PathBuf {
            let suffix = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be monotonic enough for temp paths")
                .as_nanos();
            let path = std::env::temp_dir().join(format!("{prefix}-{suffix}"));
            fs::create_dir_all(&path).expect("create temp dir");
            path
        }

        fn write_fixture(path: &Path, contents: impl AsRef<[u8]>) {
            fs::write(path, contents).expect("write fixture");
        }

        fn cleanup_fake_runtime_process(process: ActiveProcess) {
            let child_pid = process.execution.child_pid();
            let uses_shared_v8_runtime = match &process.execution {
                ActiveExecution::Javascript(execution) => execution.uses_shared_v8_runtime(),
                ActiveExecution::Python(execution) => execution.uses_shared_v8_runtime(),
                ActiveExecution::Wasm(_) => false,
                ActiveExecution::Tool(_) => false,
            };
            if !uses_shared_v8_runtime {
                let _ = signal_runtime_process(child_pid, SIGTERM);
            }
        }

        fn allow_synthetic_python_vfs_reply_drop(result: Result<(), SidecarError>, context: &str) {
            match result {
                Ok(()) => {}
                Err(SidecarError::Execution(message))
                    if message
                        .contains("failed to reply to guest Python VFS RPC request: session ")
                        && message.contains(" does not exist") => {}
                Err(error) => panic!("{context}: {error}"),
            }
        }

        fn assert_node_available() {
            let output = Command::new("node")
                .arg("--version")
                .output()
                .expect("spawn node --version");
            assert!(
                output.status.success(),
                "node must be available for python dispatch tests"
            );
        }

        fn run_javascript_entry(
            sidecar: &mut NativeSidecar<RecordingBridge>,
            vm_id: &str,
            cwd: &Path,
            process_id: &str,
            allowed_node_builtins: &str,
        ) -> (String, String, Option<i32>) {
            let context =
                sidecar
                    .javascript_engine
                    .create_context(CreateJavascriptContextRequest {
                        vm_id: vm_id.to_owned(),
                        bootstrap_module: None,
                        compile_cache_root: None,
                    });
            let env = BTreeMap::from([(
                String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
                allowed_node_builtins.to_owned(),
            )]);
            let execution = sidecar
                .javascript_engine
                .start_execution(StartJavascriptExecutionRequest {
                    vm_id: vm_id.to_owned(),
                    context_id: context.context_id,
                    argv: vec![String::from("./entry.mjs")],
                    env,
                    cwd: cwd.to_path_buf(),
                    inline_code: None,
                })
                .expect("start fake javascript execution");

            let kernel_handle = {
                let vm = sidecar.vms.get_mut(vm_id).expect("javascript vm");
                vm.kernel
                    .spawn_process(
                        JAVASCRIPT_COMMAND,
                        vec![String::from("./entry.mjs")],
                        SpawnOptions {
                            requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                            cwd: Some(String::from("/")),
                            ..SpawnOptions::default()
                        },
                    )
                    .expect("spawn kernel javascript process")
            };

            {
                let vm = sidecar.vms.get_mut(vm_id).expect("javascript vm");
                vm.active_processes.insert(
                    process_id.to_owned(),
                    ActiveProcess::new(
                        kernel_handle.pid(),
                        kernel_handle,
                        GuestRuntimeKind::JavaScript,
                        ActiveExecution::Javascript(execution),
                    )
                    .with_host_cwd(cwd.to_path_buf()),
                );
            }

            drain_process_output(sidecar, vm_id, process_id)
        }

        struct FixtureDnsServer {
            addr: SocketAddr,
            running: Arc<std::sync::atomic::AtomicBool>,
            thread: Option<thread::JoinHandle<()>>,
        }

        impl FixtureDnsServer {
            fn start() -> Self {
                let socket = UdpSocket::bind("127.0.0.1:0").expect("bind fixture DNS server");
                socket
                    .set_read_timeout(Some(Duration::from_millis(100)))
                    .expect("set fixture DNS timeout");
                let addr = socket.local_addr().expect("fixture DNS local addr");
                let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
                let thread_running = Arc::clone(&running);
                let thread = thread::spawn(move || {
                    let mut buffer = [0_u8; 2048];
                    while thread_running.load(Ordering::SeqCst) {
                        let Ok((len, peer)) = socket.recv_from(&mut buffer) else {
                            continue;
                        };
                        let Ok(request) = Message::from_vec(&buffer[..len]) else {
                            continue;
                        };
                        let response = fixture_dns_response(&request);
                        let bytes = response.to_vec().expect("encode fixture DNS response");
                        let _ = socket.send_to(&bytes, peer);
                    }
                });
                Self {
                    addr,
                    running,
                    thread: Some(thread),
                }
            }
        }

        impl Drop for FixtureDnsServer {
            fn drop(&mut self) {
                self.running.store(false, Ordering::SeqCst);
                if let Ok(socket) = UdpSocket::bind("127.0.0.1:0") {
                    let _ = socket.send_to(&[0], self.addr);
                }
                if let Some(thread) = self.thread.take() {
                    thread.join().expect("join fixture DNS thread");
                }
            }
        }

        fn fixture_dns_response(request: &Message) -> Message {
            let mut response = Message::response(request.metadata.id, request.metadata.op_code);
            response.metadata.authoritative = true;
            response.metadata.recursion_available = true;
            response.add_queries(request.queries.iter().cloned());
            if let Some(query) = request.queries.first() {
                response.add_answers(fixture_dns_answers(query));
            }
            response
        }

        fn fixture_dns_answers(query: &Query) -> Vec<Record> {
            let name = query.name().to_ascii();
            match (name.as_str(), query.query_type()) {
                ("bundle.example.test.", RecordType::A) => vec![fixture_dns_record(
                    "bundle.example.test.",
                    RData::A(A::new(203, 0, 113, 10)),
                )],
                ("bundle.example.test.", RecordType::AAAA) => vec![fixture_dns_record(
                    "bundle.example.test.",
                    RData::AAAA(AAAA::new(0x2001, 0x0db8, 0, 0, 0, 0, 0, 0x0010)),
                )],
                ("bundle.example.test.", RecordType::MX) => vec![fixture_dns_record(
                    "bundle.example.test.",
                    RData::MX(MX::new(10, fixture_dns_name("mail.example.test."))),
                )],
                ("bundle.example.test.", RecordType::TXT) => vec![
                    fixture_dns_record(
                        "bundle.example.test.",
                        RData::TXT(TXT::new(vec![String::from("v=spf1"), String::from("-all")])),
                    ),
                    fixture_dns_record(
                        "bundle.example.test.",
                        RData::TXT(TXT::new(vec![String::from("agent-os")])),
                    ),
                ],
                ("bundle.example.test.", RecordType::ANY) => vec![
                    fixture_dns_record("bundle.example.test.", RData::A(A::new(203, 0, 113, 10))),
                    fixture_dns_record(
                        "bundle.example.test.",
                        RData::AAAA(AAAA::new(0x2001, 0x0db8, 0, 0, 0, 0, 0, 0x0010)),
                    ),
                    fixture_dns_record(
                        "bundle.example.test.",
                        RData::MX(MX::new(10, fixture_dns_name("mail.example.test."))),
                    ),
                    fixture_dns_record(
                        "bundle.example.test.",
                        RData::TXT(TXT::new(vec![String::from("v=spf1"), String::from("-all")])),
                    ),
                ],
                ("alias.example.test.", RecordType::CNAME) => vec![fixture_dns_record(
                    "alias.example.test.",
                    RData::CNAME(CNAME(fixture_dns_name("bundle.example.test."))),
                )],
                ("ptr.example.test.", RecordType::PTR) => vec![fixture_dns_record(
                    "ptr.example.test.",
                    RData::PTR(PTR(fixture_dns_name("host.example.test."))),
                )],
                ("zone.example.test.", RecordType::NS) => vec![fixture_dns_record(
                    "zone.example.test.",
                    RData::NS(NS(fixture_dns_name("ns1.example.test."))),
                )],
                ("zone.example.test.", RecordType::SOA) => vec![fixture_dns_record(
                    "zone.example.test.",
                    RData::SOA(SOA::new(
                        fixture_dns_name("ns1.example.test."),
                        fixture_dns_name("hostmaster.example.test."),
                        2026041601,
                        3600,
                        600,
                        86400,
                        60,
                    )),
                )],
                ("_svc._tcp.example.test.", RecordType::SRV) => vec![fixture_dns_record(
                    "_svc._tcp.example.test.",
                    RData::SRV(SRV::new(
                        1,
                        5,
                        8443,
                        fixture_dns_name("svc-target.example.test."),
                    )),
                )],
                ("naptr.example.test.", RecordType::NAPTR) => vec![fixture_dns_record(
                    "naptr.example.test.",
                    RData::NAPTR(NAPTR::new(
                        10,
                        20,
                        b"s".to_vec().into_boxed_slice(),
                        b"SIP+D2U".to_vec().into_boxed_slice(),
                        b"!^.*$!sip:service@example.test!"
                            .to_vec()
                            .into_boxed_slice(),
                        fixture_dns_name("_sip._udp.example.test."),
                    )),
                )],
                ("caa.example.test.", RecordType::CAA) => vec![
                    fixture_dns_record(
                        "caa.example.test.",
                        RData::CAA(CAA::new_issue(
                            false,
                            Some(fixture_dns_name("letsencrypt.org.")),
                            vec![],
                        )),
                    ),
                    fixture_dns_record(
                        "caa.example.test.",
                        RData::CAA(CAA::new_iodef(
                            false,
                            url::Url::parse("https://iodef.example.test/report")
                                .expect("fixture CAA iodef URL"),
                        )),
                    ),
                ],
                _ => Vec::new(),
            }
        }

        fn fixture_dns_record(name: &str, data: RData) -> Record {
            Record::from_rdata(fixture_dns_name(name), 60, data)
        }

        fn fixture_dns_name(name: &str) -> Name {
            name.parse().expect("valid fixture DNS name")
        }

        fn drain_process_output(
            sidecar: &mut NativeSidecar<RecordingBridge>,
            vm_id: &str,
            process_id: &str,
        ) -> (String, String, Option<i32>) {
            let mut stdout = String::new();
            let mut stderr = String::new();
            let mut exit_code = None;
            for _ in 0..64 {
                let next_event = {
                    let vm = sidecar.vms.get_mut(vm_id).expect("active vm");
                    vm.active_processes
                        .get_mut(process_id)
                        .map(|process| {
                            if let Some(event) = process.pending_execution_events.pop_front() {
                                Some(event)
                            } else {
                                process
                                    .execution
                                    .poll_event_blocking(Duration::from_secs(5))
                                    .expect("poll process event")
                            }
                        })
                        .flatten()
                };
                let Some(event) = next_event else {
                    if exit_code.is_some() {
                        break;
                    }
                    panic!("process {process_id} disappeared before exit");
                };

                match &event {
                    ActiveExecutionEvent::Stdout(chunk) => {
                        stdout.push_str(&String::from_utf8_lossy(chunk));
                    }
                    ActiveExecutionEvent::Stderr(chunk) => {
                        stderr.push_str(&String::from_utf8_lossy(chunk));
                    }
                    ActiveExecutionEvent::Exited(code) => {
                        exit_code = Some(*code);
                    }
                    _ => {}
                }

                sidecar
                    .handle_execution_event(vm_id, process_id, event)
                    .expect("handle process event");
            }

            (stdout, stderr, exit_code)
        }

        fn wasm_stdout_module(message: &str) -> Vec<u8> {
            wat::parse_str(format!(
                r#"
(module
  (type $fd_write_t (func (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (type $fd_write_t)))
  (memory (export "memory") 1)
  (data (i32.const 16) "{message}\n")
  (func $_start (export "_start")
    (i32.store (i32.const 0) (i32.const 16))
    (i32.store (i32.const 4) (i32.const {length}))
    (drop
      (call $fd_write
        (i32.const 1)
        (i32.const 0)
        (i32.const 1)
        (i32.const 32)
      )
    )
  )
)
"#,
                length = message.len() + 1,
            ))
            .expect("compile wasm stdout fixture")
        }

        fn start_fake_wasm_process(
            sidecar: &mut NativeSidecar<RecordingBridge>,
            vm_id: &str,
            cwd: &Path,
            process_id: &str,
            attach_stdout_pty: bool,
        ) -> Option<u32> {
            let context = sidecar
                .wasm_engine
                .create_context(CreateWasmContextRequest {
                    vm_id: vm_id.to_owned(),
                    module_path: Some(String::from("./guest.wasm")),
                });

            let env = {
                let vm = sidecar.vms.get(vm_id).expect("wasm vm");
                BTreeMap::from([
                    (
                        String::from(EXECUTION_SANDBOX_ROOT_ENV),
                        normalize_host_path(&vm.cwd).to_string_lossy().into_owned(),
                    ),
                    (String::from(WASM_STDIO_SYNC_RPC_ENV), String::from("1")),
                ])
            };

            let execution = sidecar
                .wasm_engine
                .start_execution(StartWasmExecutionRequest {
                    vm_id: vm_id.to_owned(),
                    context_id: context.context_id,
                    argv: vec![String::from("./guest.wasm")],
                    env: env.clone(),
                    cwd: cwd.to_path_buf(),
                    permission_tier: WasmPermissionTier::Full,
                })
                .expect("start fake wasm execution");

            let (kernel_handle, master_fd) = {
                let vm = sidecar.vms.get_mut(vm_id).expect("wasm vm");
                let kernel_handle = vm
                    .kernel
                    .spawn_process(
                        WASM_COMMAND,
                        vec![String::from("./guest.wasm")],
                        SpawnOptions {
                            requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                            cwd: Some(String::from("/")),
                            ..SpawnOptions::default()
                        },
                    )
                    .expect("spawn kernel wasm process");
                let kernel_pid = kernel_handle.pid();
                let master_fd = if attach_stdout_pty {
                    let (master_fd, slave_fd, _pty_path) = vm
                        .kernel
                        .open_pty(EXECUTION_DRIVER_NAME, kernel_pid)
                        .expect("open kernel pty");
                    vm.kernel
                        .fd_dup2(EXECUTION_DRIVER_NAME, kernel_pid, slave_fd, 1)
                        .expect("dup kernel pty slave onto fd 1");
                    vm.kernel
                        .fd_close(EXECUTION_DRIVER_NAME, kernel_pid, slave_fd)
                        .expect("close extra kernel pty slave fd");
                    Some(master_fd)
                } else {
                    None
                };
                (kernel_handle, master_fd)
            };

            let vm = sidecar.vms.get_mut(vm_id).expect("wasm vm");
            let kernel_pid = kernel_handle.pid();
            vm.active_processes.insert(
                process_id.to_owned(),
                ActiveProcess::new(
                    kernel_pid,
                    kernel_handle,
                    GuestRuntimeKind::WebAssembly,
                    ActiveExecution::Wasm(execution),
                )
                .with_guest_cwd(String::from("/"))
                .with_env(env)
                .with_host_cwd(cwd.to_path_buf()),
            );

            master_fd
        }

        fn start_fake_javascript_process(
            sidecar: &mut NativeSidecar<RecordingBridge>,
            vm_id: &str,
            cwd: &Path,
            process_id: &str,
            allowed_node_builtins: &str,
        ) {
            let context =
                sidecar
                    .javascript_engine
                    .create_context(CreateJavascriptContextRequest {
                        vm_id: vm_id.to_owned(),
                        bootstrap_module: None,
                        compile_cache_root: None,
                    });
            let execution = sidecar
                .javascript_engine
                .start_execution(StartJavascriptExecutionRequest {
                    vm_id: vm_id.to_owned(),
                    context_id: context.context_id,
                    argv: vec![String::from("./entry.mjs")],
                    env: BTreeMap::from([(
                        String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
                        allowed_node_builtins.to_owned(),
                    )]),
                    cwd: cwd.to_path_buf(),
                    inline_code: None,
                })
                .expect("start fake javascript execution");

            let kernel_handle = {
                let vm = sidecar.vms.get_mut(vm_id).expect("javascript vm");
                vm.kernel
                    .spawn_process(
                        JAVASCRIPT_COMMAND,
                        vec![String::from("./entry.mjs")],
                        SpawnOptions {
                            requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                            cwd: Some(String::from("/")),
                            ..SpawnOptions::default()
                        },
                    )
                    .expect("spawn kernel javascript process")
            };

            let vm = sidecar.vms.get_mut(vm_id).expect("javascript vm");
            vm.active_processes.insert(
                process_id.to_owned(),
                ActiveProcess::new(
                    kernel_handle.pid(),
                    kernel_handle,
                    GuestRuntimeKind::JavaScript,
                    ActiveExecution::Javascript(execution),
                )
                .with_host_cwd(cwd.to_path_buf()),
            );
        }

        fn insert_fake_javascript_parent_process(
            sidecar: &mut NativeSidecar<RecordingBridge>,
            vm_id: &str,
            cwd: &Path,
            process_id: &str,
        ) {
            let (kernel_handle, guest_env) = {
                let vm = sidecar.vms.get_mut(vm_id).expect("javascript vm");
                let handle = vm
                    .kernel
                    .create_virtual_process(
                        EXECUTION_DRIVER_NAME,
                        EXECUTION_DRIVER_NAME,
                        JAVASCRIPT_COMMAND,
                        vec![String::from(JAVASCRIPT_COMMAND)],
                        VirtualProcessOptions {
                            env: vm.guest_env.clone(),
                            cwd: Some(String::from("/")),
                            ..VirtualProcessOptions::default()
                        },
                    )
                    .expect("create virtual javascript parent");
                (handle, vm.guest_env.clone())
            };

            let vm = sidecar.vms.get_mut(vm_id).expect("javascript vm");
            vm.active_processes.insert(
                process_id.to_owned(),
                ActiveProcess::new(
                    kernel_handle.pid(),
                    kernel_handle,
                    GuestRuntimeKind::JavaScript,
                    ActiveExecution::Tool(ToolExecution::default()),
                )
                .with_env(guest_env)
                .with_host_cwd(cwd.to_path_buf()),
            );
        }

        fn call_javascript_sync_rpc(
            sidecar: &mut NativeSidecar<RecordingBridge>,
            vm_id: &str,
            process_id: &str,
            request: JavascriptSyncRpcRequest,
        ) -> Result<Value, SidecarError> {
            let bridge = sidecar.bridge.clone();
            let (dns, socket_paths, counts, limits) = {
                let vm = sidecar.vms.get(vm_id).expect("javascript vm");
                (
                    vm.dns.clone(),
                    build_javascript_socket_path_context(vm).expect("build socket path context"),
                    vm.active_processes
                        .get(process_id)
                        .expect("javascript process")
                        .network_resource_counts(),
                    ResourceLimits::default(),
                )
            };

            let vm = sidecar.vms.get_mut(vm_id).expect("javascript vm");
            let process = vm
                .active_processes
                .get_mut(process_id)
                .expect("javascript process");
            service_javascript_sync_rpc(
                &bridge,
                vm_id,
                &dns,
                &socket_paths,
                &mut vm.kernel,
                process,
                &request,
                &limits,
                counts,
            )
        }
        fn kernel_socket_queries_ignore_stale_sidecar_guest_addresses() {
            assert_node_available();

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-kernel-socket-query-state");
            write_fixture(&cwd.join("entry.mjs"), "setInterval(() => {}, 1000);");
            start_fake_javascript_process(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-kernel-query",
                "[\"dgram\",\"net\"]",
            );

            let listen = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-kernel-query",
                JavascriptSyncRpcRequest {
                    id: 1,
                    method: String::from("net.listen"),
                    args: vec![json!({
                        "host": "127.0.0.1",
                        "port": 43111,
                    })],
                },
            )
            .expect("listen on kernel-backed tcp socket");
            let listener_id = listen["serverId"]
                .as_str()
                .expect("listener id")
                .to_string();

            let udp_socket = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-kernel-query",
                JavascriptSyncRpcRequest {
                    id: 2,
                    method: String::from("dgram.createSocket"),
                    args: vec![json!({ "type": "udp4" })],
                },
            )
            .expect("create kernel-backed udp socket");
            let udp_socket_id = udp_socket["socketId"]
                .as_str()
                .expect("udp socket id")
                .to_string();
            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-kernel-query",
                JavascriptSyncRpcRequest {
                    id: 3,
                    method: String::from("dgram.bind"),
                    args: vec![
                        json!(udp_socket_id.clone()),
                        json!({
                            "address": "127.0.0.1",
                            "port": 43112,
                        }),
                    ],
                },
            )
            .expect("bind kernel-backed udp socket");

            {
                let vm = sidecar.vms.get_mut(&vm_id).expect("vm state");
                let process = vm
                    .active_processes
                    .get_mut("proc-js-kernel-query")
                    .expect("javascript process");
                let listener = process
                    .tcp_listeners
                    .get_mut(&listener_id)
                    .expect("tcp listener state");
                listener.local_addr = Some(SocketAddr::from(([127, 0, 0, 1], 49991)));
                listener.guest_local_addr = SocketAddr::from(([127, 0, 0, 1], 49991));

                let udp_socket = process
                    .udp_sockets
                    .get_mut(&udp_socket_id)
                    .expect("udp socket state");
                udp_socket.guest_local_addr = Some(SocketAddr::from(([127, 0, 0, 1], 49992)));
            }

            let listener_response = sidecar
                .dispatch_blocking(request(
                    10,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::FindListener(FindListenerRequest {
                        host: Some(String::from("127.0.0.1")),
                        port: Some(43111),
                        path: None,
                    }),
                ))
                .expect("query kernel-backed listener");
            match listener_response.response.payload {
                ResponsePayload::ListenerSnapshot(snapshot) => {
                    let listener = snapshot.listener.expect("listener snapshot");
                    assert_eq!(listener.process_id, "proc-js-kernel-query");
                    assert_eq!(listener.host.as_deref(), Some("127.0.0.1"));
                    assert_eq!(listener.port, Some(43111));
                }
                other => panic!("unexpected listener response payload: {other:?}"),
            }

            let udp_response = sidecar
                .dispatch_blocking(request(
                    11,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::FindBoundUdp(FindBoundUdpRequest {
                        host: Some(String::from("127.0.0.1")),
                        port: Some(43112),
                    }),
                ))
                .expect("query kernel-backed udp socket");
            match udp_response.response.payload {
                ResponsePayload::BoundUdpSnapshot(snapshot) => {
                    let socket = snapshot.socket.expect("bound udp snapshot");
                    assert_eq!(socket.process_id, "proc-js-kernel-query");
                    assert_eq!(socket.host.as_deref(), Some("127.0.0.1"));
                    assert_eq!(socket.port, Some(43112));
                }
                other => panic!("unexpected bound udp response payload: {other:?}"),
            }
        }
        fn find_listener_rejects_without_network_inspect_permission() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                inspect_permissions(false, false),
            )
            .expect("create vm");

            let response = sidecar
                .dispatch_blocking(request(
                    12,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::FindListener(FindListenerRequest {
                        host: Some(String::from("127.0.0.1")),
                        port: Some(43111),
                        path: None,
                    }),
                ))
                .expect("dispatch listener query");

            match response.response.payload {
                ResponsePayload::Rejected(rejected) => {
                    assert_eq!(rejected.code, "execution_error");
                    assert!(
                        rejected
                            .message
                            .contains("blocked by network.inspect policy"),
                        "unexpected rejection: {rejected:?}"
                    );
                }
                other => panic!("expected rejected response, got {other:?}"),
            }
        }
        fn find_listener_returns_listener_with_network_inspect_permission() {
            assert_node_available();

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                inspect_permissions(true, false),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-inspect-listener");
            write_fixture(&cwd.join("entry.mjs"), "setInterval(() => {}, 1000);");
            start_fake_javascript_process(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-inspect-listener",
                "[\"net\"]",
            );

            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-inspect-listener",
                JavascriptSyncRpcRequest {
                    id: 1,
                    method: String::from("net.listen"),
                    args: vec![json!({
                        "host": "127.0.0.1",
                        "port": 43111,
                    })],
                },
            )
            .expect("listen on kernel-backed tcp socket");

            let response = sidecar
                .dispatch_blocking(request(
                    13,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::FindListener(FindListenerRequest {
                        host: Some(String::from("127.0.0.1")),
                        port: Some(43111),
                        path: None,
                    }),
                ))
                .expect("query listener");

            match response.response.payload {
                ResponsePayload::ListenerSnapshot(snapshot) => {
                    let listener = snapshot.listener.expect("listener snapshot");
                    assert_eq!(listener.process_id, "proc-js-inspect-listener");
                    assert_eq!(listener.host.as_deref(), Some("127.0.0.1"));
                    assert_eq!(listener.port, Some(43111));
                }
                other => panic!("unexpected listener response payload: {other:?}"),
            }
        }
        fn find_bound_udp_rejects_without_network_inspect_permission() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                inspect_permissions(false, false),
            )
            .expect("create vm");

            let response = sidecar
                .dispatch_blocking(request(
                    14,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::FindBoundUdp(FindBoundUdpRequest {
                        host: Some(String::from("127.0.0.1")),
                        port: Some(43112),
                    }),
                ))
                .expect("dispatch udp query");

            match response.response.payload {
                ResponsePayload::Rejected(rejected) => {
                    assert_eq!(rejected.code, "execution_error");
                    assert!(
                        rejected
                            .message
                            .contains("blocked by network.inspect policy"),
                        "unexpected rejection: {rejected:?}"
                    );
                }
                other => panic!("expected rejected response, got {other:?}"),
            }
        }
        fn find_bound_udp_returns_socket_with_network_inspect_permission() {
            assert_node_available();

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                inspect_permissions(true, false),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-inspect-udp");
            write_fixture(&cwd.join("entry.mjs"), "setInterval(() => {}, 1000);");
            start_fake_javascript_process(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-inspect-udp",
                "[\"dgram\"]",
            );

            let udp_socket = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-inspect-udp",
                JavascriptSyncRpcRequest {
                    id: 2,
                    method: String::from("dgram.createSocket"),
                    args: vec![json!({ "type": "udp4" })],
                },
            )
            .expect("create kernel-backed udp socket");
            let udp_socket_id = udp_socket["socketId"]
                .as_str()
                .expect("udp socket id")
                .to_string();
            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-inspect-udp",
                JavascriptSyncRpcRequest {
                    id: 3,
                    method: String::from("dgram.bind"),
                    args: vec![
                        json!(udp_socket_id),
                        json!({
                            "address": "127.0.0.1",
                            "port": 43112,
                        }),
                    ],
                },
            )
            .expect("bind kernel-backed udp socket");

            let response = sidecar
                .dispatch_blocking(request(
                    15,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::FindBoundUdp(FindBoundUdpRequest {
                        host: Some(String::from("127.0.0.1")),
                        port: Some(43112),
                    }),
                ))
                .expect("query bound udp socket");

            match response.response.payload {
                ResponsePayload::BoundUdpSnapshot(snapshot) => {
                    let socket = snapshot.socket.expect("bound udp snapshot");
                    assert_eq!(socket.process_id, "proc-js-inspect-udp");
                    assert_eq!(socket.host.as_deref(), Some("127.0.0.1"));
                    assert_eq!(socket.port, Some(43112));
                }
                other => panic!("unexpected bound udp response payload: {other:?}"),
            }
        }
        fn get_process_snapshot_rejects_without_process_inspect_permission() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                inspect_permissions(false, false),
            )
            .expect("create vm");

            let response = sidecar
                .dispatch_blocking(request(
                    16,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::GetProcessSnapshot(GetProcessSnapshotRequest {}),
                ))
                .expect("dispatch process snapshot");

            match response.response.payload {
                ResponsePayload::Rejected(rejected) => {
                    assert_eq!(rejected.code, "execution_error");
                    assert!(
                        rejected
                            .message
                            .contains("blocked by process.inspect policy"),
                        "unexpected rejection: {rejected:?}"
                    );
                }
                other => panic!("expected rejected response, got {other:?}"),
            }
        }
        fn get_process_snapshot_returns_processes_with_process_inspect_permission() {
            assert_node_available();

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                inspect_permissions(false, true),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-inspect-processes");
            write_fixture(&cwd.join("entry.mjs"), "setInterval(() => {}, 1000);");
            start_fake_javascript_process(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-inspect-processes",
                "[]",
            );

            let response = sidecar
                .dispatch_blocking(request(
                    17,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::GetProcessSnapshot(GetProcessSnapshotRequest {}),
                ))
                .expect("query process snapshot");

            match response.response.payload {
                ResponsePayload::ProcessSnapshot(snapshot) => {
                    assert!(
                        snapshot
                            .processes
                            .iter()
                            .any(|entry| entry.process_id == "proc-js-inspect-processes"),
                        "expected active process in snapshot: {:?}",
                        snapshot.processes
                    );
                }
                other => panic!("unexpected process snapshot response payload: {other:?}"),
            }
        }
        fn vm_network_resource_counts_ignore_duplicate_sidecar_kernel_entries() {
            assert_node_available();

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-kernel-network-counts");
            write_fixture(&cwd.join("entry.mjs"), "setInterval(() => {}, 1000);");
            start_fake_javascript_process(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-kernel-counts",
                "[\"dgram\",\"net\"]",
            );

            let listen = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-kernel-counts",
                JavascriptSyncRpcRequest {
                    id: 1,
                    method: String::from("net.listen"),
                    args: vec![json!({
                        "host": "127.0.0.1",
                        "port": 43121,
                    })],
                },
            )
            .expect("listen on kernel-backed tcp socket");
            let listener_id = listen["serverId"]
                .as_str()
                .expect("listener id")
                .to_string();

            let udp_socket = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-kernel-counts",
                JavascriptSyncRpcRequest {
                    id: 2,
                    method: String::from("dgram.createSocket"),
                    args: vec![json!({ "type": "udp4" })],
                },
            )
            .expect("create kernel-backed udp socket");
            let udp_socket_id = udp_socket["socketId"]
                .as_str()
                .expect("udp socket id")
                .to_string();
            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-kernel-counts",
                JavascriptSyncRpcRequest {
                    id: 3,
                    method: String::from("dgram.bind"),
                    args: vec![
                        json!(udp_socket_id.clone()),
                        json!({
                            "address": "127.0.0.1",
                            "port": 43122,
                        }),
                    ],
                },
            )
            .expect("bind kernel-backed udp socket");

            let vm = sidecar.vms.get_mut(&vm_id).expect("vm state");
            let process = vm
                .active_processes
                .get_mut("proc-js-kernel-counts")
                .expect("javascript process");

            let duplicate_listener = {
                let listener = process
                    .tcp_listeners
                    .get(&listener_id)
                    .expect("tcp listener state");
                ActiveTcpListener {
                    listener: None,
                    kernel_socket_id: listener.kernel_socket_id,
                    local_addr: Some(SocketAddr::from(([127, 0, 0, 1], 49993))),
                    guest_local_addr: SocketAddr::from(([127, 0, 0, 1], 49993)),
                    backlog: listener.backlog,
                    active_connection_ids: std::collections::BTreeSet::new(),
                }
            };
            process
                .tcp_listeners
                .insert(String::from("listener-dup"), duplicate_listener);

            let duplicate_udp = {
                let socket = process
                    .udp_sockets
                    .get(&udp_socket_id)
                    .expect("udp socket state");
                ActiveUdpSocket {
                    family: socket.family,
                    socket: None,
                    kernel_socket_id: socket.kernel_socket_id,
                    guest_local_addr: Some(SocketAddr::from(([127, 0, 0, 1], 49994))),
                    recv_buffer_size: socket.recv_buffer_size,
                    send_buffer_size: socket.send_buffer_size,
                }
            };
            process
                .udp_sockets
                .insert(String::from("udp-socket-dup"), duplicate_udp);

            let kernel_snapshot = vm.kernel.resource_snapshot();
            assert_eq!(kernel_snapshot.sockets, 2);
            assert_eq!(kernel_snapshot.socket_connections, 0);

            let counts = vm_network_resource_counts(vm);
            assert_eq!(counts.sockets, 2);
            assert_eq!(counts.connections, 0);
        }

        fn create_acp_session_for_tests(
            sidecar: &mut NativeSidecar<RecordingBridge>,
            vm_id: &str,
            cwd: &Path,
        ) -> String {
            let process_id = format!("acp-agent-test-{}", sidecar.acp_sessions.len() + 1);
            let kernel_handle = {
                let vm = sidecar.vms.get_mut(vm_id).expect("active vm");
                vm.kernel
                    .spawn_process(
                        JAVASCRIPT_COMMAND,
                        Vec::new(),
                        SpawnOptions {
                            requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                            cwd: Some(String::from("/")),
                            ..SpawnOptions::default()
                        },
                    )
                    .expect("spawn ACP kernel process")
            };
            let vm = sidecar.vms.get_mut(vm_id).expect("active vm");
            vm.active_processes.insert(
                process_id.clone(),
                ActiveProcess::new(
                    kernel_handle.pid(),
                    kernel_handle,
                    GuestRuntimeKind::JavaScript,
                    ActiveExecution::Tool(ToolExecution::default()),
                )
                .with_host_cwd(cwd.to_path_buf()),
            );

            let session_id = format!("acp-session-{}", sidecar.acp_sessions.len() + 1);
            sidecar.acp_sessions.insert(
                session_id.clone(),
                AcpSessionState::new(
                    session_id.clone(),
                    String::from(vm_id),
                    String::from("pi"),
                    process_id,
                    None,
                    &Map::new(),
                    &Map::new(),
                ),
            );
            session_id
        }
        fn acp_inbound_fs_requests_read_and_write_vm_files() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-acp-fs");
            let acp_session_id = create_acp_session_for_tests(&mut sidecar, &vm_id, &cwd);

            {
                let vm = sidecar.vms.get_mut(&vm_id).expect("active vm");
                vm.kernel
                    .write_file("/workspace/notes.txt", b"alpha\nbeta\ngamma\n".to_vec())
                    .expect("seed test file");
            }

            let read_result = sidecar
                .handle_inbound_acp_request(
                    &acp_session_id,
                    &JsonRpcRequest {
                        jsonrpc: String::from("2.0"),
                        id: JsonRpcId::Number(41),
                        method: String::from("fs/read_text_file"),
                        params: Some(json!({
                            "path": "/workspace/notes.txt",
                            "line": 2,
                            "limit": 2,
                        })),
                    },
                )
                .expect("read ACP request")
                .expect("read ACP result");
            assert_eq!(read_result, json!({ "content": "beta\ngamma" }));

            let write_result = sidecar
                .handle_inbound_acp_request(
                    &acp_session_id,
                    &JsonRpcRequest {
                        jsonrpc: String::from("2.0"),
                        id: JsonRpcId::Number(42),
                        method: String::from("fs/write_text_file"),
                        params: Some(json!({
                            "path": "/workspace/notes.txt",
                            "content": "rewritten",
                        })),
                    },
                )
                .expect("write ACP request")
                .expect("write ACP result");
            assert_eq!(write_result, Value::Null);

            let bytes = {
                let vm = sidecar.vms.get_mut(&vm_id).expect("active vm");
                vm.kernel
                    .read_file("/workspace/notes.txt")
                    .expect("read rewritten file")
            };
            assert_eq!(String::from_utf8(bytes).expect("utf8 file"), "rewritten");
        }
        fn acp_inbound_terminal_requests_manage_internal_processes() {
            assert_node_available();

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-acp-terminal");
            let acp_session_id = create_acp_session_for_tests(&mut sidecar, &vm_id, &cwd);

            let created = sidecar
                .handle_inbound_acp_request(
                    &acp_session_id,
                    &JsonRpcRequest {
                        jsonrpc: String::from("2.0"),
                        id: JsonRpcId::Number(50),
                        method: String::from("terminal/create"),
                        params: Some(json!({
                            "command": "node",
                            "args": [
                                "--eval",
                                "process.stdout.write('hello\\n'); process.stderr.write('oops\\n');",
                            ],
                        })),
                    },
                )
                .expect("create terminal")
                .expect("terminal create result");
            let short_terminal_id = created["terminalId"]
                .as_str()
                .expect("terminal id")
                .to_owned();

            let wait_result = sidecar
                .handle_inbound_acp_request(
                    &acp_session_id,
                    &JsonRpcRequest {
                        jsonrpc: String::from("2.0"),
                        id: JsonRpcId::Number(51),
                        method: String::from("terminal/wait_for_exit"),
                        params: Some(json!({ "terminalId": &short_terminal_id })),
                    },
                )
                .expect("wait terminal")
                .expect("terminal wait result");
            assert_eq!(wait_result, json!({ "exitCode": 0, "signal": Value::Null }));

            let output_result = sidecar
                .handle_inbound_acp_request(
                    &acp_session_id,
                    &JsonRpcRequest {
                        jsonrpc: String::from("2.0"),
                        id: JsonRpcId::Number(52),
                        method: String::from("terminal/output"),
                        params: Some(json!({ "terminalId": &short_terminal_id })),
                    },
                )
                .expect("terminal output")
                .expect("terminal output result");
            let output = output_result["output"]
                .as_str()
                .expect("terminal output string");
            assert!(output.contains("hello"));
            assert!(output.contains("oops"));
            assert_eq!(output_result["truncated"], Value::Bool(false));
            assert_eq!(output_result["exitStatus"]["exitCode"], json!(0));

            let release_result = sidecar
                .handle_inbound_acp_request(
                    &acp_session_id,
                    &JsonRpcRequest {
                        jsonrpc: String::from("2.0"),
                        id: JsonRpcId::Number(53),
                        method: String::from("terminal/release"),
                        params: Some(json!({ "terminalId": &short_terminal_id })),
                    },
                )
                .expect("release terminal")
                .expect("terminal release result");
            assert_eq!(release_result, Value::Null);
            assert!(matches!(
                sidecar.handle_inbound_acp_request(
                    &acp_session_id,
                    &JsonRpcRequest {
                        jsonrpc: String::from("2.0"),
                        id: JsonRpcId::Number(54),
                        method: String::from("terminal/output"),
                        params: Some(json!({ "terminalId": &short_terminal_id })),
                    },
                ),
                Err(SidecarError::InvalidState(message))
                    if message == format!("ACP terminal not found: {short_terminal_id}")
            ));

            let created = sidecar
                .handle_inbound_acp_request(
                    &acp_session_id,
                    &JsonRpcRequest {
                        jsonrpc: String::from("2.0"),
                        id: JsonRpcId::Number(55),
                        method: String::from("terminal/create"),
                        params: Some(json!({
                            "command": "node",
                            "args": [
                                "--eval",
                                "setInterval(() => {}, 1000);",
                            ],
                        })),
                    },
                )
                .expect("create long-lived terminal")
                .expect("long-lived terminal result");
            let long_terminal_id = created["terminalId"]
                .as_str()
                .expect("terminal id")
                .to_owned();

            let kill_result = sidecar
                .handle_inbound_acp_request(
                    &acp_session_id,
                    &JsonRpcRequest {
                        jsonrpc: String::from("2.0"),
                        id: JsonRpcId::Number(56),
                        method: String::from("terminal/kill"),
                        params: Some(json!({ "terminalId": &long_terminal_id })),
                    },
                )
                .expect("kill terminal")
                .expect("terminal kill result");
            assert_eq!(kill_result, Value::Null);

            let wait_result = sidecar
                .handle_inbound_acp_request(
                    &acp_session_id,
                    &JsonRpcRequest {
                        jsonrpc: String::from("2.0"),
                        id: JsonRpcId::Number(57),
                        method: String::from("terminal/wait_for_exit"),
                        params: Some(json!({ "terminalId": &long_terminal_id })),
                    },
                )
                .expect("wait killed terminal")
                .expect("killed terminal wait result");
            assert!(
                wait_result["exitCode"]
                    .as_i64()
                    .expect("exit code should be numeric")
                    > 0
            );

            let release_result = sidecar
                .handle_inbound_acp_request(
                    &acp_session_id,
                    &JsonRpcRequest {
                        jsonrpc: String::from("2.0"),
                        id: JsonRpcId::Number(58),
                        method: String::from("terminal/release"),
                        params: Some(json!({ "terminalId": &long_terminal_id })),
                    },
                )
                .expect("release killed terminal")
                .expect("release killed terminal result");
            assert_eq!(release_result, Value::Null);

            let event = sidecar
                .poll_event_blocking(
                    &OwnershipScope::session(&connection_id, &session_id),
                    Duration::from_millis(25),
                )
                .expect("poll session events");
            assert!(
                event.is_none(),
                "ACP terminal processes should stay internal"
            );
        }
        fn acp_inbound_requests_wait_for_forwarded_host_responses_before_method_not_found() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-acp-forward");
            let acp_session_id = create_acp_session_for_tests(&mut sidecar, &vm_id, &cwd);
            let ownership = OwnershipScope::vm(&connection_id, &session_id, &vm_id);

            sidecar.set_sidecar_request_handler({
                let acp_session_id = acp_session_id.clone();
                move |request| {
                    let SidecarRequestPayload::AcpRequest(payload) = request.payload else {
                        return Err(SidecarError::InvalidState(String::from(
                            "expected ACP sidecar request payload",
                        )));
                    };
                    assert_eq!(payload.session_id, acp_session_id);
                    let forwarded = serde_json::from_value::<JsonRpcRequest>(payload.request)
                        .expect("forwarded ACP request");
                    assert_eq!(forwarded.method, "host/echo");
                    thread::sleep(Duration::from_millis(25));
                    Ok(SidecarResponsePayload::AcpRequestResult(
                        SidecarAcpResultResponse {
                            response: Some(json!({
                                "jsonrpc": "2.0",
                                "id": forwarded.id,
                                "result": {
                                    "echo": forwarded.params,
                                },
                            })),
                            error: None,
                        },
                    ))
                }
            });

            let started_at = Instant::now();
            let response = sidecar.resolve_inbound_acp_request_with_timeout(
                &ownership,
                &acp_session_id,
                &JsonRpcRequest {
                    jsonrpc: String::from("2.0"),
                    id: JsonRpcId::Number(61),
                    method: String::from("host/echo"),
                    params: Some(json!({ "path": "/workspace/notes.txt" })),
                },
                Duration::from_millis(100),
            );

            assert!(started_at.elapsed() >= Duration::from_millis(20));
            assert_eq!(
                response,
                JsonRpcResponse::success(
                    JsonRpcId::Number(61),
                    json!({
                        "echo": {
                            "path": "/workspace/notes.txt",
                        },
                    }),
                )
            );
        }
        fn acp_inbound_requests_time_out_to_method_not_found_when_host_never_answers() {
            struct TimeoutAcpTransport;

            impl SidecarRequestTransport for TimeoutAcpTransport {
                fn send_request(
                    &self,
                    request: SidecarRequestFrame,
                    timeout: Duration,
                ) -> Result<SidecarResponseFrame, SidecarError> {
                    let SidecarRequestPayload::AcpRequest(payload) = request.payload else {
                        return Err(SidecarError::InvalidState(String::from(
                            "expected ACP sidecar request payload",
                        )));
                    };
                    let forwarded = serde_json::from_value::<JsonRpcRequest>(payload.request)
                        .expect("forwarded ACP request");
                    assert_eq!(forwarded.method, "host/missing");
                    Err(SidecarError::Io(format!(
                        "timed out waiting for sidecar response after {}s",
                        timeout.as_secs()
                    )))
                }
            }

            let mut sidecar = create_test_sidecar();
            sidecar.set_sidecar_request_transport(Arc::new(TimeoutAcpTransport));
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-acp-forward-timeout");
            let acp_session_id = create_acp_session_for_tests(&mut sidecar, &vm_id, &cwd);
            let ownership = OwnershipScope::vm(&connection_id, &session_id, &vm_id);

            let response = sidecar.resolve_inbound_acp_request_with_timeout(
                &ownership,
                &acp_session_id,
                &JsonRpcRequest {
                    jsonrpc: String::from("2.0"),
                    id: JsonRpcId::Number(62),
                    method: String::from("host/missing"),
                    params: None,
                },
                Duration::from_millis(5),
            );

            assert_eq!(
                response.error(),
                Some(&JsonRpcError {
                    code: -32601,
                    message: String::from("Method not found: host/missing"),
                    data: None,
                })
            );
        }

        fn acp_termination_sends_cancel_before_sigterm_and_closes_session_early() {
            assert_node_available();

            let mut sidecar = create_test_sidecar_with_config(NativeSidecarConfig {
                acp_termination_grace: Duration::from_millis(100),
                ..NativeSidecarConfig::default()
            });
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let (acp_session_id, child_pid) = create_mock_acp_session(
                &mut sidecar,
                &connection_id,
                &session_id,
                &vm_id,
                "graceful-agent.mjs",
                ACP_GRACEFUL_TERMINATION_AGENT,
            );
            let process_id = sidecar
                .acp_sessions
                .get(&acp_session_id)
                .expect("ACP session")
                .process_id
                .clone();
            let begin = sidecar
                .begin_acp_process_termination(&vm_id, &process_id)
                .expect("begin ACP termination");
            let Some((_session_ids, ownership)) = begin else {
                panic!("active ACP process should enter termination");
            };
            let session = sidecar
                .acp_sessions
                .get(&acp_session_id)
                .expect("ACP session after begin");
            assert!(
                session.closed,
                "session.closed should flip as soon as termination begins"
            );
            assert!(session.termination_requested);
            assert!(session
                .recent_activity
                .iter()
                .any(|entry| entry == "sent notification session/cancel"));
            assert!(sidecar
                .vms
                .get(&vm_id)
                .is_some_and(|vm| vm.active_processes.contains_key(&process_id)));

            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build local runtime for ACP termination test");
            let local = tokio::task::LocalSet::new();
            let graceful_vm_id = vm_id.clone();
            sidecar = runtime.block_on(local.run_until(async move {
                let cancel_deadline = Instant::now() + Duration::from_millis(200);
                while Instant::now() < cancel_deadline {
                    let _ = sidecar
                        .poll_event(&ownership, Duration::from_millis(20))
                        .await
                        .expect("pump cancel notification event");
                    let saw_cancel_file = sidecar
                        .vms
                        .get_mut(&graceful_vm_id)
                        .expect("VM during cancel pump")
                        .kernel
                        .read_file("/workspace/cancel.json")
                        .is_ok();
                    if saw_cancel_file {
                        break;
                    }
                }
                sidecar
                    .terminate_acp_process(&graceful_vm_id, &process_id)
                    .await
                    .expect("terminate ACP process");
                sidecar
            }));

            assert!(
                !runtime_child_is_alive(child_pid).expect("inspect terminated ACP child"),
                "graceful ACP agent should exit before SIGKILL fallback"
            );
            let session = sidecar
                .acp_sessions
                .get(&acp_session_id)
                .expect("ACP session after graceful termination");
            assert!(session.closed);
            assert!(session
                .recent_activity
                .iter()
                .any(|entry| entry == "sent signal SIGTERM"));
            assert!(
                !session
                    .recent_activity
                    .iter()
                    .any(|entry| entry == "sent signal SIGKILL"),
                "graceful ACP termination should not need SIGKILL"
            );
            assert!(
                sidecar
                    .vms
                    .get_mut(&vm_id)
                    .expect("VM after graceful termination")
                    .kernel
                    .read_file("/workspace/cancel.json")
                    .is_ok(),
                "expected the ACP agent to receive session/cancel before shutdown"
            );
            let sigterm_marker = sidecar
                .vms
                .get_mut(&vm_id)
                .expect("VM after graceful termination")
                .kernel
                .read_file("/workspace/sigterm.json")
                .expect("read graceful SIGTERM marker");
            let sigterm_marker: Value =
                serde_json::from_slice(&sigterm_marker).expect("parse graceful SIGTERM marker");
            assert_eq!(sigterm_marker["phase"], json!("sigterm"));
            assert_eq!(sigterm_marker["cancelSeen"], json!(true));
        }

        fn acp_termination_sigkills_after_grace_when_agent_ignores_sigterm() {
            assert_node_available();

            let grace = Duration::from_millis(75);
            let mut sidecar = create_test_sidecar_with_config(NativeSidecarConfig {
                acp_termination_grace: grace,
                ..NativeSidecarConfig::default()
            });
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let (acp_session_id, child_pid) = create_mock_acp_session(
                &mut sidecar,
                &connection_id,
                &session_id,
                &vm_id,
                "ignore-term-agent.mjs",
                ACP_IGNORE_TERM_AGENT,
            );
            let process_id = sidecar
                .acp_sessions
                .get(&acp_session_id)
                .expect("ACP session")
                .process_id
                .clone();

            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build local runtime for ACP timeout test");
            let local = tokio::task::LocalSet::new();
            let started = Instant::now();
            let forced_vm_id = vm_id.clone();
            sidecar = runtime.block_on(local.run_until(async move {
                sidecar
                    .terminate_acp_process(&forced_vm_id, &process_id)
                    .await
                    .expect("terminate unresponsive ACP process");
                sidecar
            }));
            let elapsed = started.elapsed();

            assert!(
                elapsed >= grace,
                "SIGKILL fallback should not fire before the SIGTERM grace window"
            );
            assert!(
                !runtime_child_is_alive(child_pid).expect("inspect SIGKILLed ACP child"),
                "unresponsive ACP agent should be reaped by SIGKILL fallback"
            );
            let session = sidecar
                .acp_sessions
                .get(&acp_session_id)
                .expect("ACP session after forced termination");
            assert!(session.closed);
            assert!(session.termination_requested);
            assert!(session
                .recent_activity
                .iter()
                .any(|entry| entry == "sent signal SIGKILL"));
            assert!(
                sidecar
                    .vms
                    .get_mut(&vm_id)
                    .expect("VM after forced termination")
                    .kernel
                    .read_file("/workspace/sigterm-ignored.json")
                    .is_ok(),
                "expected the ACP agent to observe SIGTERM before SIGKILL fallback"
            );
        }

        fn poll_http2_event(
            sidecar: &mut NativeSidecar<RecordingBridge>,
            vm_id: &str,
            process_id: &str,
            method: &str,
            id: u64,
            kind: &str,
        ) -> Value {
            for _ in 0..200 {
                let value = call_javascript_sync_rpc(
                    sidecar,
                    vm_id,
                    process_id,
                    JavascriptSyncRpcRequest {
                        id: 9_000,
                        method: String::from(method),
                        args: vec![json!(id), json!(25)],
                    },
                )
                .expect("poll http2 event");
                if value.is_null() {
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }
                let event: Value = serde_json::from_str(value.as_str().expect("event payload"))
                    .expect("parse http2 event");
                if event["kind"] == Value::String(String::from(kind)) {
                    return event;
                }
            }
            panic!("timed out waiting for {method} {kind}");
        }

        fn tls_test_certificates() -> Vec<rustls::pki_types::CertificateDer<'static>> {
            rustls_pemfile::certs(&mut BufReader::new(TLS_TEST_CERT_PEM.as_bytes()))
                .collect::<Result<Vec<_>, _>>()
                .expect("parse TLS test certificate")
        }

        fn tls_test_private_key() -> rustls::pki_types::PrivateKeyDer<'static> {
            rustls_pemfile::private_key(&mut BufReader::new(TLS_TEST_KEY_PEM.as_bytes()))
                .expect("parse TLS test private key")
                .expect("TLS test private key")
        }

        fn tls_test_server_config(alpn: &[&str]) -> Arc<ServerConfig> {
            let mut config =
                ServerConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
                    .with_safe_default_protocol_versions()
                    .expect("TLS server protocol versions")
                    .with_no_client_auth()
                    .with_single_cert(tls_test_certificates(), tls_test_private_key())
                    .expect("build TLS test server config");
            config.alpn_protocols = alpn
                .iter()
                .map(|protocol| protocol.as_bytes().to_vec())
                .collect();
            Arc::new(config)
        }

        #[derive(Debug)]
        struct TestInsecureTlsVerifier {
            supported_schemes: Vec<SignatureScheme>,
        }

        impl ServerCertVerifier for TestInsecureTlsVerifier {
            fn verify_server_cert(
                &self,
                _end_entity: &CertificateDer<'_>,
                _intermediates: &[CertificateDer<'_>],
                _server_name: &ServerName<'_>,
                _ocsp_response: &[u8],
                _now: rustls::pki_types::UnixTime,
            ) -> Result<ServerCertVerified, rustls::Error> {
                Ok(ServerCertVerified::assertion())
            }

            fn verify_tls12_signature(
                &self,
                _message: &[u8],
                _cert: &CertificateDer<'_>,
                _dss: &DigitallySignedStruct,
            ) -> Result<HandshakeSignatureValid, rustls::Error> {
                Ok(HandshakeSignatureValid::assertion())
            }

            fn verify_tls13_signature(
                &self,
                _message: &[u8],
                _cert: &CertificateDer<'_>,
                _dss: &DigitallySignedStruct,
            ) -> Result<HandshakeSignatureValid, rustls::Error> {
                Ok(HandshakeSignatureValid::assertion())
            }

            fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
                self.supported_schemes.clone()
            }
        }

        fn tls_test_client_config(trust_test_cert: bool, alpn: &[&str]) -> Arc<ClientConfig> {
            let provider = Arc::new(aws_lc_rs::default_provider());
            let builder = ClientConfig::builder_with_provider(provider.clone())
                .with_safe_default_protocol_versions()
                .expect("TLS client protocol versions");
            let mut config = if trust_test_cert {
                let mut roots = RootCertStore::empty();
                for certificate in tls_test_certificates() {
                    roots.add(certificate).expect("add TLS test certificate");
                }
                builder.with_root_certificates(roots).with_no_client_auth()
            } else {
                let verifier = Arc::new(TestInsecureTlsVerifier {
                    supported_schemes: provider
                        .signature_verification_algorithms
                        .supported_schemes(),
                });
                builder
                    .dangerous()
                    .with_custom_certificate_verifier(verifier)
                    .with_no_client_auth()
            };
            config.alpn_protocols = alpn
                .iter()
                .map(|protocol| protocol.as_bytes().to_vec())
                .collect();
            Arc::new(config)
        }

        fn loopback_tls_endpoints() -> (
            crate::state::LoopbackTlsEndpoint,
            crate::state::LoopbackTlsEndpoint,
        ) {
            let pair = Arc::new(crate::state::LoopbackTlsTransportPair {
                state: Mutex::new(crate::state::LoopbackTlsTransportPairState::default()),
                ready: std::sync::Condvar::new(),
            });
            (
                crate::state::LoopbackTlsEndpoint {
                    pair: Arc::clone(&pair),
                    is_lower_socket: true,
                },
                crate::state::LoopbackTlsEndpoint {
                    pair,
                    is_lower_socket: false,
                },
            )
        }

        fn with_panic_counter<T>(
            operation: impl FnOnce(Arc<AtomicUsize>) -> T + std::panic::UnwindSafe,
        ) -> T {
            static PANIC_HOOK_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

            let _hook_guard = PANIC_HOOK_LOCK
                .get_or_init(|| Mutex::new(()))
                .lock()
                .expect("panic hook lock");
            let panic_counter = Arc::new(AtomicUsize::new(0));
            let previous_hook = std::panic::take_hook();
            let hook_counter = Arc::clone(&panic_counter);
            std::panic::set_hook(Box::new(move |_| {
                hook_counter.fetch_add(1, Ordering::SeqCst);
            }));

            let result = std::panic::catch_unwind(|| operation(Arc::clone(&panic_counter)));
            std::panic::set_hook(previous_hook);
            match result {
                Ok(value) => value,
                Err(payload) => std::panic::resume_unwind(payload),
            }
        }

        fn tls_service_test_lock() -> std::sync::MutexGuard<'static, ()> {
            static TLS_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
            TLS_TEST_LOCK
                .get_or_init(|| Mutex::new(()))
                .lock()
                .expect("TLS service test lock")
        }

        fn complete_loopback_tls_handshake(start: Arc<Barrier>) {
            let (client_transport, server_transport) = loopback_tls_endpoints();
            let server_start = Arc::clone(&start);
            let server = thread::spawn(move || {
                server_start.wait();
                let mut stream = rustls::StreamOwned::new(
                    ServerConnection::new(tls_test_server_config(&["h2"]))
                        .expect("create loopback TLS server"),
                    server_transport,
                );
                while stream.conn.is_handshaking() {
                    match stream.conn.complete_io(&mut stream.sock) {
                        Ok(_) => {}
                        Err(error)
                            if {
                                let kind = error.kind();
                                kind == std::io::ErrorKind::WouldBlock
                                    || kind == std::io::ErrorKind::TimedOut
                            } =>
                        {
                            thread::yield_now()
                        }
                        Err(error) => panic!("complete loopback TLS server handshake: {error}"),
                    }
                }

                let mut payload = [0_u8; 4];
                stream
                    .read_exact(&mut payload)
                    .expect("read loopback TLS client payload");
                assert_eq!(&payload, b"ping");
                stream
                    .write_all(b"pong")
                    .expect("write loopback TLS server payload");
                stream.flush().expect("flush loopback TLS server payload");
            });

            let client = thread::spawn(move || {
                start.wait();
                let mut stream = rustls::StreamOwned::new(
                    ClientConnection::new(
                        tls_test_client_config(false, &["h2"]),
                        ServerName::try_from("localhost").expect("loopback TLS server name"),
                    )
                    .expect("create loopback TLS client"),
                    client_transport,
                );
                while stream.conn.is_handshaking() {
                    match stream.conn.complete_io(&mut stream.sock) {
                        Ok(_) => {}
                        Err(error)
                            if {
                                let kind = error.kind();
                                kind == std::io::ErrorKind::WouldBlock
                                    || kind == std::io::ErrorKind::TimedOut
                            } =>
                        {
                            thread::yield_now()
                        }
                        Err(error) => panic!("complete loopback TLS client handshake: {error}"),
                    }
                }

                stream
                    .write_all(b"ping")
                    .expect("write loopback TLS client payload");
                stream.flush().expect("flush loopback TLS client payload");
                let mut payload = [0_u8; 4];
                stream
                    .read_exact(&mut payload)
                    .expect("read loopback TLS server payload");
                assert_eq!(&payload, b"pong");
            });

            client.join().expect("join loopback TLS client");
            server.join().expect("join loopback TLS server");
        }
        fn loopback_tls_transport_survives_concurrent_handshakes_without_panicking() {
            let _tls_lock = tls_service_test_lock();
            with_panic_counter(|panic_counter| {
                let concurrency = 4;
                let start = Arc::new(Barrier::new(concurrency * 2));
                let workers = (0..concurrency)
                    .map(|_| {
                        let start = Arc::clone(&start);
                        thread::spawn(move || complete_loopback_tls_handshake(start))
                    })
                    .collect::<Vec<_>>();

                for worker in workers {
                    worker
                        .join()
                        .expect("join loopback TLS handshake stress worker");
                }

                assert_eq!(
                    panic_counter.load(Ordering::SeqCst),
                    0,
                    "loopback TLS handshake stress triggered a panic"
                );
            });
        }
        fn loopback_tls_endpoint_read_survives_competing_drain_and_peer_drop() {
            with_panic_counter(|panic_counter| {
                let (reader_endpoint, peer_endpoint) = loopback_tls_endpoints();
                {
                    let mut state = reader_endpoint
                        .pair
                        .state
                        .lock()
                        .expect("loopback TLS state");
                    state
                        .higher_to_lower
                        .extend((0..4096).map(|value| (value % 251) as u8));
                }

                let competing_reader = crate::state::LoopbackTlsEndpoint {
                    pair: Arc::clone(&reader_endpoint.pair),
                    is_lower_socket: reader_endpoint.is_lower_socket,
                };
                let start = Arc::new(Barrier::new(3));

                let primary_reader = {
                    let start = Arc::clone(&start);
                    thread::spawn(move || {
                        start.wait();
                        let mut endpoint = reader_endpoint;
                        let mut buffer = [0_u8; 64];
                        let mut total = 0;
                        loop {
                            match endpoint.read(&mut buffer) {
                                Ok(0) => return total,
                                Ok(read) => total += read,
                                Err(error)
                                    if {
                                        let kind = error.kind();
                                        kind == std::io::ErrorKind::WouldBlock
                                            || kind == std::io::ErrorKind::TimedOut
                                    } =>
                                {
                                    thread::yield_now()
                                }
                                Err(error) => panic!("primary loopback TLS read failed: {error}"),
                            }
                        }
                    })
                };

                let drain_racer = {
                    let start = Arc::clone(&start);
                    thread::spawn(move || {
                        start.wait();
                        let mut endpoint = competing_reader;
                        let mut buffer = [0_u8; 31];
                        loop {
                            match endpoint.read(&mut buffer) {
                                Ok(0) => return,
                                Ok(_) => thread::yield_now(),
                                Err(error)
                                    if {
                                        let kind = error.kind();
                                        kind == std::io::ErrorKind::WouldBlock
                                            || kind == std::io::ErrorKind::TimedOut
                                    } =>
                                {
                                    thread::yield_now()
                                }
                                Err(error) => {
                                    panic!("competing loopback TLS read failed: {error}")
                                }
                            }
                        }
                    })
                };

                let closer = {
                    let start = Arc::clone(&start);
                    thread::spawn(move || {
                        start.wait();
                        thread::sleep(std::time::Duration::from_millis(5));
                        drop(peer_endpoint);
                    })
                };

                primary_reader.join().expect("join primary loopback reader");
                drain_racer.join().expect("join competing loopback reader");
                closer.join().expect("join loopback peer closer");

                assert_eq!(
                    panic_counter.load(Ordering::SeqCst),
                    0,
                    "loopback TLS endpoint race triggered a panic"
                );
            });
        }
        fn javascript_net_socket_wait_connect_reports_tcp_socket_info() {
            assert_node_available();

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-js-net-wait-connect-cwd");
            write_fixture(&cwd.join("entry.mjs"), "setInterval(() => {}, 1000);");
            start_fake_javascript_process(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-net-wait-connect",
                "[\"net\"]",
            );

            let listen = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-net-wait-connect",
                JavascriptSyncRpcRequest {
                    id: 1,
                    method: String::from("net.listen"),
                    args: vec![json!({
                        "host": "127.0.0.1",
                        "port": 0,
                        "backlog": 1,
                    })],
                },
            )
            .expect("listen through sidecar net RPC");
            let server_id = listen["serverId"].as_str().expect("server id").to_string();
            let guest_port = listen["localPort"]
                .as_u64()
                .and_then(|value| u16::try_from(value).ok())
                .expect("guest listener port");

            let connect = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-net-wait-connect",
                JavascriptSyncRpcRequest {
                    id: 2,
                    method: String::from("net.connect"),
                    args: vec![json!({
                        "host": "127.0.0.1",
                        "port": guest_port,
                    })],
                },
            )
            .expect("connect to vm-owned listener");
            let socket_id = connect["socketId"].as_str().expect("socket id").to_string();

            let info = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-net-wait-connect",
                JavascriptSyncRpcRequest {
                    id: 3,
                    method: String::from("net.socket_wait_connect"),
                    args: vec![json!(socket_id.clone())],
                },
            )
            .expect("wait for connect");
            let parsed: Value = serde_json::from_str(info.as_str().expect("socket info string"))
                .expect("parse socket info");
            assert_eq!(parsed["remoteAddress"], Value::from("127.0.0.1"));
            assert_eq!(parsed["remotePort"], Value::from(guest_port));
            assert_eq!(parsed["remoteFamily"], Value::from("IPv4"));
            assert_eq!(parsed["localFamily"], Value::from("IPv4"));
            assert!(
                parsed["localPort"].as_u64().is_some_and(|port| port > 0),
                "socket info: {parsed}"
            );

            let accepted = (0..20)
                .find_map(|attempt| {
                    let value = call_javascript_sync_rpc(
                        &mut sidecar,
                        &vm_id,
                        "proc-js-net-wait-connect",
                        JavascriptSyncRpcRequest {
                            id: 4 + attempt,
                            method: String::from("net.server_accept"),
                            args: vec![json!(server_id.clone())],
                        },
                    )
                    .expect("accept connected client");
                    (value != Value::from("__secure_exec_net_timeout__")).then_some(value)
                })
                .expect("eventually accept connected client");
            let accepted: Value =
                serde_json::from_str(accepted.as_str().expect("accepted payload string"))
                    .expect("parse accepted payload");
            let accepted_socket_id = accepted["socketId"]
                .as_str()
                .expect("accepted socket id")
                .to_string();

            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-net-wait-connect",
                JavascriptSyncRpcRequest {
                    id: 50,
                    method: String::from("net.destroy"),
                    args: vec![json!(socket_id)],
                },
            )
            .expect("destroy connected socket");
            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-net-wait-connect",
                JavascriptSyncRpcRequest {
                    id: 51,
                    method: String::from("net.destroy"),
                    args: vec![json!(accepted_socket_id)],
                },
            )
            .expect("destroy accepted socket");
            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-net-wait-connect",
                JavascriptSyncRpcRequest {
                    id: 52,
                    method: String::from("net.server_close"),
                    args: vec![json!(server_id)],
                },
            )
            .expect("close listener");
        }
        fn javascript_net_socket_read_and_socket_options_work_for_tcp_sockets() {
            assert_node_available();

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-js-net-read-cwd");
            write_fixture(&cwd.join("entry.mjs"), "setInterval(() => {}, 1000);");
            start_fake_javascript_process(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-net-read",
                "[\"net\"]",
            );

            let listen = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-net-read",
                JavascriptSyncRpcRequest {
                    id: 1,
                    method: String::from("net.listen"),
                    args: vec![json!({
                        "host": "127.0.0.1",
                        "port": 0,
                        "backlog": 1,
                    })],
                },
            )
            .expect("listen through sidecar net RPC");
            let server_id = listen["serverId"].as_str().expect("server id").to_string();
            let guest_port = listen["localPort"]
                .as_u64()
                .and_then(|value| u16::try_from(value).ok())
                .expect("guest listener port");

            let connect = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-net-read",
                JavascriptSyncRpcRequest {
                    id: 2,
                    method: String::from("net.connect"),
                    args: vec![json!({
                        "host": "127.0.0.1",
                        "port": guest_port,
                    })],
                },
            )
            .expect("connect to vm-owned listener");
            let socket_id = connect["socketId"].as_str().expect("socket id").to_string();

            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-net-read",
                JavascriptSyncRpcRequest {
                    id: 3,
                    method: String::from("net.socket_set_no_delay"),
                    args: vec![json!(socket_id.clone()), Value::Bool(true)],
                },
            )
            .expect("enable TCP_NODELAY");
            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-net-read",
                JavascriptSyncRpcRequest {
                    id: 4,
                    method: String::from("net.socket_set_keep_alive"),
                    args: vec![json!(socket_id.clone()), Value::Bool(true), json!(1)],
                },
            )
            .expect("enable SO_KEEPALIVE");

            let mut accepted = None;
            for attempt in 0..20 {
                let value = call_javascript_sync_rpc(
                    &mut sidecar,
                    &vm_id,
                    "proc-js-net-read",
                    JavascriptSyncRpcRequest {
                        id: 5 + attempt,
                        method: String::from("net.server_accept"),
                        args: vec![json!(server_id.clone())],
                    },
                )
                .expect("accept connected client");
                if value != Value::from("__secure_exec_net_timeout__") {
                    accepted = Some(value);
                    break;
                }
                thread::sleep(std::time::Duration::from_millis(10));
            }
            let accepted = accepted.expect("eventually accept connected client");
            let accepted: Value =
                serde_json::from_str(accepted.as_str().expect("accepted payload string"))
                    .expect("parse accepted payload");
            let server_socket_id = accepted["socketId"]
                .as_str()
                .expect("accepted socket id")
                .to_string();

            {
                let vm = sidecar.vms.get(&vm_id).expect("javascript vm");
                let process = vm
                    .active_processes
                    .get("proc-js-net-read")
                    .expect("javascript process");
                let socket = process.tcp_sockets.get(&socket_id).expect("tcp socket");
                assert!(
                    socket.kernel_socket_id.is_some(),
                    "expected loopback net.connect to use kernel socket state"
                );
                assert!(socket.no_delay, "expected TCP_NODELAY flag to be tracked");
                assert!(
                    socket.keep_alive,
                    "expected SO_KEEPALIVE flag to be tracked"
                );
                assert_eq!(socket.keep_alive_initial_delay_secs, Some(1));
            }

            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-net-read",
                JavascriptSyncRpcRequest {
                    id: 60,
                    method: String::from("net.write"),
                    args: vec![
                        json!(server_socket_id.clone()),
                        json!({
                            "__agentOsType": "bytes",
                            "base64": base64::engine::general_purpose::STANDARD.encode("ping"),
                        }),
                    ],
                },
            )
            .expect("write server payload");
            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-net-read",
                JavascriptSyncRpcRequest {
                    id: 61,
                    method: String::from("net.shutdown"),
                    args: vec![json!(server_socket_id.clone())],
                },
            )
            .expect("shutdown server write half");

            let mut payload = None;
            for attempt in 0..20 {
                let value = call_javascript_sync_rpc(
                    &mut sidecar,
                    &vm_id,
                    "proc-js-net-read",
                    JavascriptSyncRpcRequest {
                        id: 10 + attempt,
                        method: String::from("net.socket_read"),
                        args: vec![json!(socket_id.clone())],
                    },
                )
                .expect("read bridged socket chunk");
                if value != Value::from("__secure_exec_net_timeout__") {
                    payload = Some(value);
                    break;
                }
                thread::sleep(std::time::Duration::from_millis(10));
            }
            let payload = payload.expect("eventually receive bridged socket data");
            assert_eq!(payload, Value::from("cGluZw=="));

            let mut end = None;
            for attempt in 0..20 {
                let value = call_javascript_sync_rpc(
                    &mut sidecar,
                    &vm_id,
                    "proc-js-net-read",
                    JavascriptSyncRpcRequest {
                        id: 40 + attempt,
                        method: String::from("net.socket_read"),
                        args: vec![json!(socket_id.clone())],
                    },
                )
                .expect("read bridged socket end");
                if value != Value::from("__secure_exec_net_timeout__") {
                    end = Some(value);
                    break;
                }
                thread::sleep(std::time::Duration::from_millis(10));
            }
            let end = end.expect("eventually receive bridged socket EOF");
            assert_eq!(end, Value::Null);

            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-net-read",
                JavascriptSyncRpcRequest {
                    id: 99,
                    method: String::from("net.destroy"),
                    args: vec![json!(socket_id)],
                },
            )
            .expect("destroy connected socket");
            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-net-read",
                JavascriptSyncRpcRequest {
                    id: 100,
                    method: String::from("net.destroy"),
                    args: vec![json!(server_socket_id)],
                },
            )
            .expect("destroy accepted socket");
            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-net-read",
                JavascriptSyncRpcRequest {
                    id: 101,
                    method: String::from("net.server_close"),
                    args: vec![json!(server_id)],
                },
            )
            .expect("close listener");
        }
        fn javascript_net_upgrade_socket_aliases_use_tcp_socket_state() {
            assert_node_available();

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-js-upgrade-socket-cwd");
            write_fixture(&cwd.join("entry.mjs"), "setInterval(() => {}, 1000);");
            start_fake_javascript_process(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-upgrade-socket",
                "[\"net\"]",
            );

            let listen = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-upgrade-socket",
                JavascriptSyncRpcRequest {
                    id: 1,
                    method: String::from("net.listen"),
                    args: vec![json!({
                        "host": "127.0.0.1",
                        "port": 0,
                        "backlog": 1,
                    })],
                },
            )
            .expect("listen through sidecar net RPC");
            let server_id = listen["serverId"].as_str().expect("server id").to_string();
            let guest_port = listen["localPort"]
                .as_u64()
                .and_then(|value| u16::try_from(value).ok())
                .expect("guest listener port");

            let connect = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-upgrade-socket",
                JavascriptSyncRpcRequest {
                    id: 2,
                    method: String::from("net.connect"),
                    args: vec![json!({
                        "host": "127.0.0.1",
                        "port": guest_port,
                    })],
                },
            )
            .expect("connect to vm-owned listener");
            let client_socket_id = connect["socketId"].as_str().expect("socket id").to_string();

            let accepted = (0..20)
                .find_map(|attempt| {
                    let value = call_javascript_sync_rpc(
                        &mut sidecar,
                        &vm_id,
                        "proc-js-upgrade-socket",
                        JavascriptSyncRpcRequest {
                            id: 10 + attempt,
                            method: String::from("net.server_accept"),
                            args: vec![json!(server_id.clone())],
                        },
                    )
                    .expect("accept connected client");
                    (value != Value::from("__secure_exec_net_timeout__")).then_some(value)
                })
                .expect("eventually accept connected client");
            let accepted: Value =
                serde_json::from_str(accepted.as_str().expect("accepted payload string"))
                    .expect("parse accepted payload");
            let server_socket_id = accepted["socketId"]
                .as_str()
                .expect("accepted socket id")
                .to_string();

            let written = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-upgrade-socket",
                JavascriptSyncRpcRequest {
                    id: 50,
                    method: String::from("net.upgrade_socket_write"),
                    args: vec![
                        json!(server_socket_id.clone()),
                        json!(base64::engine::general_purpose::STANDARD.encode("ping")),
                    ],
                },
            )
            .expect("write upgrade socket payload");
            assert_eq!(written, Value::from(4));

            let mut payload = None;
            for attempt in 0..20 {
                let value = call_javascript_sync_rpc(
                    &mut sidecar,
                    &vm_id,
                    "proc-js-upgrade-socket",
                    JavascriptSyncRpcRequest {
                        id: 60 + attempt,
                        method: String::from("net.socket_read"),
                        args: vec![json!(client_socket_id.clone())],
                    },
                )
                .expect("read upgrade socket payload");
                if value != Value::from("__secure_exec_net_timeout__") {
                    payload = Some(value);
                    break;
                }
                thread::sleep(std::time::Duration::from_millis(10));
            }
            let payload = payload.expect("eventually receive upgrade socket data");
            assert_eq!(payload, Value::from("cGluZw=="));

            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-upgrade-socket",
                JavascriptSyncRpcRequest {
                    id: 80,
                    method: String::from("net.upgrade_socket_end"),
                    args: vec![json!(server_socket_id.clone())],
                },
            )
            .expect("end upgrade socket");

            let mut end = None;
            for attempt in 0..20 {
                let value = call_javascript_sync_rpc(
                    &mut sidecar,
                    &vm_id,
                    "proc-js-upgrade-socket",
                    JavascriptSyncRpcRequest {
                        id: 90 + attempt,
                        method: String::from("net.socket_read"),
                        args: vec![json!(client_socket_id.clone())],
                    },
                )
                .expect("read upgrade socket EOF");
                if value != Value::from("__secure_exec_net_timeout__") {
                    end = Some(value);
                    break;
                }
                thread::sleep(std::time::Duration::from_millis(10));
            }
            let end = end.expect("eventually receive upgrade socket EOF");
            assert_eq!(end, Value::Null);

            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-upgrade-socket",
                JavascriptSyncRpcRequest {
                    id: 120,
                    method: String::from("net.upgrade_socket_destroy"),
                    args: vec![json!(client_socket_id)],
                },
            )
            .expect("destroy client upgrade socket");
            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-upgrade-socket",
                JavascriptSyncRpcRequest {
                    id: 121,
                    method: String::from("net.upgrade_socket_destroy"),
                    args: vec![json!(server_socket_id)],
                },
            )
            .expect("destroy accepted upgrade socket");
            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-upgrade-socket",
                JavascriptSyncRpcRequest {
                    id: 122,
                    method: String::from("net.server_close"),
                    args: vec![json!(server_id)],
                },
            )
            .expect("close listener");
        }
        fn javascript_dgram_address_and_buffer_size_sync_rpcs_work() {
            assert_node_available();

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-js-dgram-options-cwd");
            write_fixture(&cwd.join("entry.mjs"), "setInterval(() => {}, 1000);");
            start_fake_javascript_process(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-dgram-options",
                "[\"dgram\"]",
            );

            let socket = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-dgram-options",
                JavascriptSyncRpcRequest {
                    id: 1,
                    method: String::from("dgram.createSocket"),
                    args: vec![json!({ "type": "udp4" })],
                },
            )
            .expect("create udp socket");
            let socket_id = socket["socketId"]
                .as_str()
                .expect("udp socket id")
                .to_string();

            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-dgram-options",
                JavascriptSyncRpcRequest {
                    id: 2,
                    method: String::from("dgram.bind"),
                    args: vec![
                        json!(socket_id.clone()),
                        json!({
                            "address": "127.0.0.1",
                            "port": 0,
                        }),
                    ],
                },
            )
            .expect("bind udp socket");

            let address = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-dgram-options",
                JavascriptSyncRpcRequest {
                    id: 3,
                    method: String::from("dgram.address"),
                    args: vec![json!(socket_id.clone())],
                },
            )
            .expect("get udp socket address");
            let address: Value =
                serde_json::from_str(address.as_str().expect("address payload string"))
                    .expect("parse address payload");
            assert_eq!(address["address"], Value::from("127.0.0.1"));
            assert_eq!(address["family"], Value::from("IPv4"));
            assert!(
                address["port"].as_u64().is_some_and(|port| port > 0),
                "socket address: {address}"
            );

            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-dgram-options",
                JavascriptSyncRpcRequest {
                    id: 4,
                    method: String::from("dgram.setBufferSize"),
                    args: vec![json!(socket_id.clone()), json!("recv"), json!(4096)],
                },
            )
            .expect("set recv buffer size");
            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-dgram-options",
                JavascriptSyncRpcRequest {
                    id: 5,
                    method: String::from("dgram.setBufferSize"),
                    args: vec![json!(socket_id.clone()), json!("send"), json!(2048)],
                },
            )
            .expect("set send buffer size");

            let recv_size = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-dgram-options",
                JavascriptSyncRpcRequest {
                    id: 6,
                    method: String::from("dgram.getBufferSize"),
                    args: vec![json!(socket_id.clone()), json!("recv")],
                },
            )
            .expect("get recv buffer size");
            assert!(
                recv_size.as_u64().is_some_and(|size| size >= 4096),
                "recv buffer size: {recv_size}"
            );

            let send_size = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-dgram-options",
                JavascriptSyncRpcRequest {
                    id: 7,
                    method: String::from("dgram.getBufferSize"),
                    args: vec![json!(socket_id.clone()), json!("send")],
                },
            )
            .expect("get send buffer size");
            assert!(
                send_size.as_u64().is_some_and(|size| size >= 2048),
                "send buffer size: {send_size}"
            );

            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-dgram-options",
                JavascriptSyncRpcRequest {
                    id: 8,
                    method: String::from("dgram.close"),
                    args: vec![json!(socket_id)],
                },
            )
            .expect("close udp socket");
        }
        fn javascript_tls_client_upgrade_query_and_cipher_list_work() {
            let _tls_lock = tls_service_test_lock();
            assert_node_available();

            let listener = TcpListener::bind("127.0.0.1:0").expect("bind TLS listener");
            let port = listener.local_addr().expect("listener address").port();
            let server = thread::spawn(move || {
                let config = tls_test_server_config(&["http/1.1"]);
                let (stream, _) = listener.accept().expect("accept TLS client");
                let mut stream = rustls::StreamOwned::new(
                    ServerConnection::new(config).expect("create TLS server connection"),
                    stream,
                );
                while stream.conn.is_handshaking() {
                    stream
                        .conn
                        .complete_io(&mut stream.sock)
                        .expect("complete TLS server handshake");
                }
                assert_eq!(stream.conn.alpn_protocol(), Some(b"http/1.1".as_slice()));

                let mut payload = [0_u8; 4];
                stream
                    .read_exact(&mut payload)
                    .expect("read client payload");
                assert_eq!(&payload, b"ping");
                stream
                    .write_all(b"pong")
                    .expect("write TLS server response");
                stream.flush().expect("flush TLS server response");
            });

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm_with_metadata(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
                BTreeMap::from([(
                    format!("env.{LOOPBACK_EXEMPT_PORTS_ENV}"),
                    serde_json::to_string(&vec![port.to_string()]).expect("serialize exempt ports"),
                )]),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-js-tls-client-rpc-cwd");
            write_fixture(&cwd.join("entry.mjs"), "setInterval(() => {}, 1000);");
            start_fake_javascript_process(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-tls-client",
                "[\"net\",\"tls\"]",
            );

            let ciphers = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-tls-client",
                JavascriptSyncRpcRequest {
                    id: 1,
                    method: String::from("tls.get_ciphers"),
                    args: Vec::new(),
                },
            )
            .expect("list TLS ciphers");
            let ciphers: Value = serde_json::from_str(ciphers.as_str().expect("cipher JSON"))
                .expect("parse ciphers");
            assert!(
                ciphers
                    .as_array()
                    .is_some_and(|entries| !entries.is_empty()),
                "ciphers: {ciphers}"
            );

            let connect = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-tls-client",
                JavascriptSyncRpcRequest {
                    id: 2,
                    method: String::from("net.connect"),
                    args: vec![json!({
                        "host": "127.0.0.1",
                        "port": port,
                    })],
                },
            )
            .expect("connect to host TLS server");
            let socket_id = connect["socketId"].as_str().expect("socket id").to_string();

            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-tls-client",
                JavascriptSyncRpcRequest {
                    id: 3,
                    method: String::from("net.socket_upgrade_tls"),
                    args: vec![
                        json!(socket_id.clone()),
                        json!(serde_json::to_string(&json!({
                            "isServer": false,
                            "servername": "localhost",
                            "rejectUnauthorized": false,
                            "ALPNProtocols": ["http/1.1"],
                        }))
                        .expect("serialize client TLS options")),
                    ],
                },
            )
            .expect("upgrade client socket to TLS");

            let protocol = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-tls-client",
                JavascriptSyncRpcRequest {
                    id: 4,
                    method: String::from("net.socket_tls_query"),
                    args: vec![json!(socket_id.clone()), json!("getProtocol")],
                },
            )
            .expect("query TLS protocol");
            let protocol: Value =
                serde_json::from_str(protocol.as_str().expect("TLS protocol query JSON"))
                    .expect("parse TLS protocol");
            assert!(
                protocol == Value::String(String::from("TLSv1.3"))
                    || protocol == Value::String(String::from("TLSv1.2")),
                "protocol: {protocol}"
            );

            let cipher = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-tls-client",
                JavascriptSyncRpcRequest {
                    id: 5,
                    method: String::from("net.socket_tls_query"),
                    args: vec![json!(socket_id.clone()), json!("getCipher")],
                },
            )
            .expect("query TLS cipher");
            let cipher: Value =
                serde_json::from_str(cipher.as_str().expect("TLS cipher query JSON"))
                    .expect("parse TLS cipher");
            assert_eq!(cipher["type"], Value::from("object"));

            let peer_certificate = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-tls-client",
                JavascriptSyncRpcRequest {
                    id: 6,
                    method: String::from("net.socket_tls_query"),
                    args: vec![
                        json!(socket_id.clone()),
                        json!("getPeerCertificate"),
                        Value::Bool(true),
                    ],
                },
            )
            .expect("query TLS peer certificate");
            let peer_certificate: Value = serde_json::from_str(
                peer_certificate
                    .as_str()
                    .expect("TLS peer certificate query JSON"),
            )
            .expect("parse TLS peer certificate");
            assert_eq!(peer_certificate["type"], Value::from("object"));

            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-tls-client",
                JavascriptSyncRpcRequest {
                    id: 7,
                    method: String::from("net.write"),
                    args: vec![
                        json!(socket_id.clone()),
                        json!({
                            "__agentOsType": "bytes",
                            "base64": base64::engine::general_purpose::STANDARD.encode("ping"),
                        }),
                    ],
                },
            )
            .expect("write TLS client payload");

            let payload = (0..30)
                .find_map(|attempt| {
                    let value = call_javascript_sync_rpc(
                        &mut sidecar,
                        &vm_id,
                        "proc-js-tls-client",
                        JavascriptSyncRpcRequest {
                            id: 20 + attempt,
                            method: String::from("net.socket_read"),
                            args: vec![json!(socket_id.clone())],
                        },
                    )
                    .expect("read TLS client payload");
                    if value == Value::from("__secure_exec_net_timeout__") {
                        thread::sleep(Duration::from_millis(10));
                        None
                    } else {
                        Some(value)
                    }
                })
                .expect("eventually receive TLS response");
            assert_eq!(payload, Value::from("cG9uZw=="));

            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-tls-client",
                JavascriptSyncRpcRequest {
                    id: 99,
                    method: String::from("net.destroy"),
                    args: vec![json!(socket_id)],
                },
            )
            .expect("destroy TLS client socket");

            server.join().expect("join TLS server");
        }
        fn javascript_tls_server_client_hello_and_server_upgrade_work() {
            let _tls_lock = tls_service_test_lock();
            assert_node_available();

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-js-tls-server-rpc-cwd");
            write_fixture(&cwd.join("entry.mjs"), "setInterval(() => {}, 1000);");
            start_fake_javascript_process(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-tls-server",
                "[\"net\",\"tls\"]",
            );

            let listen = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-tls-server",
                JavascriptSyncRpcRequest {
                    id: 1,
                    method: String::from("net.listen"),
                    args: vec![json!({
                        "host": "127.0.0.1",
                        "port": 0,
                        "backlog": 1,
                    })],
                },
            )
            .expect("listen through sidecar net RPC");
            let server_id = listen["serverId"].as_str().expect("server id").to_string();
            let guest_port = listen["localPort"]
                .as_u64()
                .and_then(|value| u16::try_from(value).ok())
                .expect("guest listener port");
            let client_connect = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-tls-server",
                JavascriptSyncRpcRequest {
                    id: 2,
                    method: String::from("net.connect"),
                    args: vec![json!({
                        "host": "127.0.0.1",
                        "port": guest_port,
                    })],
                },
            )
            .expect("connect guest TLS client");
            let client_socket_id = client_connect["socketId"]
                .as_str()
                .expect("client socket id")
                .to_string();

            let accepted = (0..30)
                .find_map(|attempt| {
                    let value = call_javascript_sync_rpc(
                        &mut sidecar,
                        &vm_id,
                        "proc-js-tls-server",
                        JavascriptSyncRpcRequest {
                            id: 10 + attempt,
                            method: String::from("net.server_accept"),
                            args: vec![json!(server_id.clone())],
                        },
                    )
                    .expect("accept TLS client");
                    if value == Value::from("__secure_exec_net_timeout__") {
                        thread::sleep(Duration::from_millis(10));
                        None
                    } else {
                        Some(value)
                    }
                })
                .expect("eventually accept TLS client");
            let accepted: Value =
                serde_json::from_str(accepted.as_str().expect("accepted payload string"))
                    .expect("parse accepted payload");
            let socket_id = accepted["socketId"]
                .as_str()
                .expect("accepted socket id")
                .to_string();

            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-tls-server",
                JavascriptSyncRpcRequest {
                    id: 40,
                    method: String::from("net.socket_upgrade_tls"),
                    args: vec![
                        json!(client_socket_id.clone()),
                        json!(serde_json::to_string(&json!({
                            "isServer": false,
                            "servername": "localhost",
                            "rejectUnauthorized": false,
                            "ALPNProtocols": ["h2", "http/1.1"],
                        }))
                        .expect("serialize client TLS options")),
                    ],
                },
            )
            .expect("upgrade guest TLS client socket");

            let client_hello = (0..30)
                .find_map(|attempt| {
                    let value = call_javascript_sync_rpc(
                        &mut sidecar,
                        &vm_id,
                        "proc-js-tls-server",
                        JavascriptSyncRpcRequest {
                            id: 50 + attempt,
                            method: String::from("net.socket_get_tls_client_hello"),
                            args: vec![json!(socket_id.clone())],
                        },
                    )
                    .expect("get TLS client hello");
                    let parsed: Value =
                        serde_json::from_str(value.as_str().expect("TLS client hello JSON"))
                            .expect("parse TLS client hello");
                    if parsed["servername"] == Value::from("localhost") {
                        Some(parsed)
                    } else {
                        thread::sleep(Duration::from_millis(10));
                        None
                    }
                })
                .expect("eventually parse TLS client hello");
            assert_eq!(client_hello["servername"], Value::from("localhost"));
            assert!(
                client_hello["ALPNProtocols"]
                    .as_array()
                    .is_some_and(|protocols| protocols.contains(&Value::from("h2"))),
                "client hello: {client_hello}"
            );

            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-tls-server",
                JavascriptSyncRpcRequest {
                    id: 80,
                    method: String::from("net.socket_upgrade_tls"),
                    args: vec![
                        json!(socket_id.clone()),
                        json!(serde_json::to_string(&json!({
                            "isServer": true,
                            "key": { "kind": "string", "data": TLS_TEST_KEY_PEM },
                            "cert": { "kind": "string", "data": TLS_TEST_CERT_PEM },
                            "ALPNProtocols": ["h2"],
                        }))
                        .expect("serialize server TLS options")),
                    ],
                },
            )
            .expect("upgrade accepted socket to TLS");

            let certificate = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-tls-server",
                JavascriptSyncRpcRequest {
                    id: 81,
                    method: String::from("net.socket_tls_query"),
                    args: vec![json!(socket_id.clone()), json!("getCertificate")],
                },
            )
            .expect("query local TLS certificate");
            let certificate: Value =
                serde_json::from_str(certificate.as_str().expect("TLS certificate JSON"))
                    .expect("parse TLS certificate");
            assert_eq!(certificate["type"], Value::from("object"));

            (0..30)
                .find_map(|attempt| {
                    let server_protocol = call_javascript_sync_rpc(
                        &mut sidecar,
                        &vm_id,
                        "proc-js-tls-server",
                        JavascriptSyncRpcRequest {
                            id: 82 + attempt * 2,
                            method: String::from("net.socket_tls_query"),
                            args: vec![json!(socket_id.clone()), json!("getProtocol")],
                        },
                    )
                    .expect("query server TLS protocol");
                    let server_protocol: Value = serde_json::from_str(
                        server_protocol.as_str().expect("server protocol JSON"),
                    )
                    .expect("parse server protocol");

                    let client_protocol = call_javascript_sync_rpc(
                        &mut sidecar,
                        &vm_id,
                        "proc-js-tls-server",
                        JavascriptSyncRpcRequest {
                            id: 83 + attempt * 2,
                            method: String::from("net.socket_tls_query"),
                            args: vec![json!(client_socket_id.clone()), json!("getProtocol")],
                        },
                    )
                    .expect("query client TLS protocol");
                    let client_protocol: Value = serde_json::from_str(
                        client_protocol.as_str().expect("client protocol JSON"),
                    )
                    .expect("parse client protocol");

                    if server_protocol.is_null() || client_protocol.is_null() {
                        thread::sleep(Duration::from_millis(10));
                        None
                    } else {
                        Some(())
                    }
                })
                .expect("eventually complete guest TLS handshake");

            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-tls-server",
                JavascriptSyncRpcRequest {
                    id: 145,
                    method: String::from("net.write"),
                    args: vec![
                        json!(client_socket_id.clone()),
                        json!({
                            "__agentOsType": "bytes",
                            "base64": base64::engine::general_purpose::STANDARD.encode("ping"),
                        }),
                    ],
                },
            )
            .expect("write guest TLS client payload");

            let payload = (0..30)
                .find_map(|attempt| {
                    let value = call_javascript_sync_rpc(
                        &mut sidecar,
                        &vm_id,
                        "proc-js-tls-server",
                        JavascriptSyncRpcRequest {
                            id: 150 + attempt,
                            method: String::from("net.socket_read"),
                            args: vec![json!(socket_id.clone())],
                        },
                    )
                    .expect("read TLS server payload");
                    if value == Value::from("__secure_exec_net_timeout__") {
                        thread::sleep(Duration::from_millis(10));
                        None
                    } else {
                        Some(value)
                    }
                })
                .expect("eventually receive TLS client payload");
            assert_eq!(payload, Value::from("cGluZw=="));

            let protocol = (0..30)
                .find_map(|attempt| {
                    let value = call_javascript_sync_rpc(
                        &mut sidecar,
                        &vm_id,
                        "proc-js-tls-server",
                        JavascriptSyncRpcRequest {
                            id: 190 + attempt,
                            method: String::from("net.socket_tls_query"),
                            args: vec![json!(socket_id.clone()), json!("getProtocol")],
                        },
                    )
                    .expect("query TLS protocol");
                    let parsed: Value =
                        serde_json::from_str(value.as_str().expect("TLS protocol JSON"))
                            .expect("parse TLS protocol");
                    if parsed.is_null() {
                        thread::sleep(Duration::from_millis(10));
                        None
                    } else {
                        Some(parsed)
                    }
                })
                .expect("eventually negotiate TLS protocol");
            assert!(
                protocol == Value::String(String::from("TLSv1.3"))
                    || protocol == Value::String(String::from("TLSv1.2")),
                "protocol: {protocol}"
            );

            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-tls-server",
                JavascriptSyncRpcRequest {
                    id: 120,
                    method: String::from("net.write"),
                    args: vec![
                        json!(socket_id.clone()),
                        json!({
                            "__agentOsType": "bytes",
                            "base64": base64::engine::general_purpose::STANDARD.encode("pong"),
                        }),
                    ],
                },
            )
            .expect("write TLS server payload");

            let client_payload = (0..30)
                .find_map(|attempt| {
                    let value = call_javascript_sync_rpc(
                        &mut sidecar,
                        &vm_id,
                        "proc-js-tls-server",
                        JavascriptSyncRpcRequest {
                            id: 220 + attempt,
                            method: String::from("net.socket_read"),
                            args: vec![json!(client_socket_id.clone())],
                        },
                    )
                    .expect("read guest TLS client payload");
                    if value == Value::from("__secure_exec_net_timeout__") {
                        thread::sleep(Duration::from_millis(10));
                        None
                    } else {
                        Some(value)
                    }
                })
                .expect("eventually receive TLS server payload");
            assert_eq!(client_payload, Value::from("cG9uZw=="));

            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-tls-server",
                JavascriptSyncRpcRequest {
                    id: 121,
                    method: String::from("net.destroy"),
                    args: vec![json!(socket_id)],
                },
            )
            .expect("destroy accepted TLS socket");
            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-tls-server",
                JavascriptSyncRpcRequest {
                    id: 122,
                    method: String::from("net.destroy"),
                    args: vec![json!(client_socket_id)],
                },
            )
            .expect("destroy guest TLS client socket");
            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-tls-server",
                JavascriptSyncRpcRequest {
                    id: 123,
                    method: String::from("net.server_close"),
                    args: vec![json!(server_id)],
                },
            )
            .expect("close TLS listener");
        }
        fn javascript_net_server_accept_returns_timeout_then_pending_connection() {
            assert_node_available();

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-js-server-accept-cwd");
            write_fixture(&cwd.join("entry.mjs"), "setInterval(() => {}, 1000);");
            start_fake_javascript_process(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-server-accept",
                "[\"net\"]",
            );

            let listen = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-server-accept",
                JavascriptSyncRpcRequest {
                    id: 1,
                    method: String::from("net.listen"),
                    args: vec![json!({
                        "host": "127.0.0.1",
                        "port": 0,
                        "backlog": 1,
                    })],
                },
            )
            .expect("listen through sidecar net RPC");
            let server_id = listen["serverId"].as_str().expect("server id").to_string();
            let guest_port = listen["localPort"]
                .as_u64()
                .and_then(|value| u16::try_from(value).ok())
                .expect("guest listener port");
            let timeout = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-server-accept",
                JavascriptSyncRpcRequest {
                    id: 2,
                    method: String::from("net.server_accept"),
                    args: vec![json!(server_id.clone())],
                },
            )
            .expect("accept timeout sentinel");
            assert_eq!(timeout, Value::from("__secure_exec_net_timeout__"));

            let connect = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-server-accept",
                JavascriptSyncRpcRequest {
                    id: 3,
                    method: String::from("net.connect"),
                    args: vec![json!({
                        "host": "127.0.0.1",
                        "port": guest_port,
                    })],
                },
            )
            .expect("connect to vm-owned listener");
            let client_socket_id = connect["socketId"]
                .as_str()
                .expect("client socket id")
                .to_string();

            let mut accepted = None;
            for attempt in 0..20 {
                let value = call_javascript_sync_rpc(
                    &mut sidecar,
                    &vm_id,
                    "proc-js-server-accept",
                    JavascriptSyncRpcRequest {
                        id: 10 + attempt,
                        method: String::from("net.server_accept"),
                        args: vec![json!(server_id.clone())],
                    },
                )
                .expect("accept pending connection");
                if value != Value::from("__secure_exec_net_timeout__") {
                    accepted = Some(value);
                    break;
                }
                thread::sleep(std::time::Duration::from_millis(10));
            }
            let accepted = accepted.expect("eventually accept pending TCP connection");
            let parsed: Value =
                serde_json::from_str(accepted.as_str().expect("accepted payload string"))
                    .expect("parse accepted payload");
            assert!(
                parsed["socketId"].as_str().is_some(),
                "accepted payload: {parsed}"
            );
            assert_eq!(parsed["info"]["localAddress"], Value::from("127.0.0.1"));
            assert_eq!(parsed["info"]["localPort"], Value::from(guest_port));
            assert_eq!(parsed["info"]["localFamily"], Value::from("IPv4"));
            assert_eq!(parsed["info"]["remoteFamily"], Value::from("IPv4"));
            assert!(
                parsed["info"]["remotePort"]
                    .as_u64()
                    .is_some_and(|port| port > 0),
                "accepted payload: {parsed}"
            );

            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-server-accept",
                JavascriptSyncRpcRequest {
                    id: 40,
                    method: String::from("net.destroy"),
                    args: vec![json!(client_socket_id)],
                },
            )
            .expect("destroy client socket");
            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-server-accept",
                JavascriptSyncRpcRequest {
                    id: 41,
                    method: String::from("net.destroy"),
                    args: vec![json!(parsed["socketId"]
                        .as_str()
                        .expect("accepted socket id"))],
                },
            )
            .expect("destroy accepted socket");
        }
        fn javascript_kernel_stdin_reads_buffered_input_and_reports_timeout_and_eof() {
            assert_node_available();

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-js-kernel-stdin-cwd");
            write_fixture(&cwd.join("entry.mjs"), "setInterval(() => {}, 1000);");
            let context =
                sidecar
                    .javascript_engine
                    .create_context(CreateJavascriptContextRequest {
                        vm_id: vm_id.clone(),
                        bootstrap_module: None,
                        compile_cache_root: None,
                    });
            let execution = sidecar
                .javascript_engine
                .start_execution(StartJavascriptExecutionRequest {
                    vm_id: vm_id.clone(),
                    context_id: context.context_id,
                    argv: vec![String::from("./entry.mjs")],
                    env: BTreeMap::from([(
                        String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
                        String::from(
                            "[\"assert\",\"buffer\",\"console\",\"events\",\"fs\",\"path\",\"readline\",\"stream\",\"string_decoder\",\"timers\",\"util\"]",
                        ),
                    )]),
                    cwd: cwd.clone(),
                    inline_code: None,
                })
                .expect("start fake javascript execution");
            let kernel_handle = {
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                vm.kernel
                    .spawn_process(
                        JAVASCRIPT_COMMAND,
                        vec![String::from("./entry.mjs")],
                        SpawnOptions {
                            requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                            cwd: Some(String::from("/")),
                            ..SpawnOptions::default()
                        },
                    )
                    .expect("spawn kernel javascript process")
            };
            let kernel_stdin_writer_fd = {
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                let (read_fd, write_fd) = vm
                    .kernel
                    .open_pipe(EXECUTION_DRIVER_NAME, kernel_handle.pid())
                    .expect("open kernel stdin pipe");
                vm.kernel
                    .fd_dup2(EXECUTION_DRIVER_NAME, kernel_handle.pid(), read_fd, 0)
                    .expect("dup kernel stdin pipe onto fd 0");
                vm.kernel
                    .fd_close(EXECUTION_DRIVER_NAME, kernel_handle.pid(), read_fd)
                    .expect("close extra kernel stdin read fd");
                write_fd
            };
            {
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                vm.active_processes.insert(
                    String::from("proc-js-stdin"),
                    ActiveProcess::new(
                        kernel_handle.pid(),
                        kernel_handle,
                        GuestRuntimeKind::JavaScript,
                        ActiveExecution::Javascript(execution),
                    )
                    .with_kernel_stdin_writer_fd(kernel_stdin_writer_fd)
                    .with_host_cwd(cwd.clone()),
                );
            }

            let initial = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-stdin",
                JavascriptSyncRpcRequest {
                    id: 1,
                    method: String::from("__kernel_stdin_read"),
                    args: vec![json!(1024), json!(10)],
                },
            )
            .expect("poll empty kernel stdin");
            assert_eq!(initial, Value::Null);

            let write = sidecar
                .dispatch_blocking(request(
                    11,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::WriteStdin(WriteStdinRequest {
                        process_id: String::from("proc-js-stdin"),
                        chunk: String::from("hello from stdin"),
                    }),
                ))
                .expect("write stdin");
            match write.response.payload {
                ResponsePayload::StdinWritten(response) => {
                    assert_eq!(response.process_id, "proc-js-stdin");
                    assert_eq!(response.accepted_bytes, "hello from stdin".len() as u64);
                }
                other => panic!("unexpected stdin_written response: {other:?}"),
            }

            let next = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-stdin",
                JavascriptSyncRpcRequest {
                    id: 2,
                    method: String::from("__kernel_stdin_read"),
                    args: vec![json!(1024), json!(10)],
                },
            )
            .expect("read kernel stdin payload");
            assert_eq!(
                next,
                json!({
                    "dataBase64": base64::engine::general_purpose::STANDARD
                        .encode("hello from stdin"),
                })
            );

            let close = sidecar
                .dispatch_blocking(request(
                    12,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::CloseStdin(CloseStdinRequest {
                        process_id: String::from("proc-js-stdin"),
                    }),
                ))
                .expect("close stdin");
            match close.response.payload {
                ResponsePayload::StdinClosed(response) => {
                    assert_eq!(response.process_id, "proc-js-stdin");
                }
                other => panic!("unexpected stdin_closed response: {other:?}"),
            }

            let eof = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-stdin",
                JavascriptSyncRpcRequest {
                    id: 3,
                    method: String::from("__kernel_stdin_read"),
                    args: vec![json!(1024), json!(10)],
                },
            )
            .expect("read kernel stdin eof");
            assert_eq!(eof, json!({ "done": true }));

            sidecar
                .kill_process_internal(&vm_id, "proc-js-stdin", "SIGKILL")
                .expect("kill javascript stdin process");
        }
        fn javascript_sync_rpc_pty_set_raw_mode_toggles_kernel_tty_discipline() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-js-pty-raw-mode");
            write_fixture(&cwd.join("entry.mjs"), "export {};\n");

            let context =
                sidecar
                    .javascript_engine
                    .create_context(CreateJavascriptContextRequest {
                        vm_id: vm_id.clone(),
                        bootstrap_module: None,
                        compile_cache_root: None,
                    });
            let execution = sidecar
                .javascript_engine
                .start_execution(StartJavascriptExecutionRequest {
                    vm_id: vm_id.clone(),
                    context_id: context.context_id,
                    argv: vec![String::from("./entry.mjs")],
                    env: BTreeMap::new(),
                    cwd: cwd.clone(),
                    inline_code: None,
                })
                .expect("start fake javascript execution");
            let kernel_handle = {
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                vm.kernel
                    .spawn_process(
                        JAVASCRIPT_COMMAND,
                        vec![String::from("./entry.mjs")],
                        SpawnOptions {
                            requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                            cwd: Some(String::from("/")),
                            ..SpawnOptions::default()
                        },
                    )
                    .expect("spawn kernel javascript process")
            };
            {
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                let (_master_fd, slave_fd, _pty_path) = vm
                    .kernel
                    .open_pty(EXECUTION_DRIVER_NAME, kernel_handle.pid())
                    .expect("open kernel pty");
                vm.kernel
                    .fd_dup2(EXECUTION_DRIVER_NAME, kernel_handle.pid(), slave_fd, 0)
                    .expect("dup kernel pty slave onto fd 0");
                vm.kernel
                    .fd_close(EXECUTION_DRIVER_NAME, kernel_handle.pid(), slave_fd)
                    .expect("close extra kernel pty slave fd");
                vm.active_processes.insert(
                    String::from("proc-js-pty"),
                    ActiveProcess::new(
                        kernel_handle.pid(),
                        kernel_handle,
                        GuestRuntimeKind::JavaScript,
                        ActiveExecution::Javascript(execution),
                    )
                    .with_host_cwd(cwd.clone()),
                );
            }

            {
                let vm = sidecar.vms.get(&vm_id).expect("javascript vm");
                let kernel_pid = vm.active_processes["proc-js-pty"].kernel_pid;
                let termios = vm
                    .kernel
                    .tcgetattr(EXECUTION_DRIVER_NAME, kernel_pid, 0)
                    .expect("read cooked termios");
                assert!(termios.icanon);
                assert!(termios.echo);
                assert!(termios.isig);
            }

            let raw = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-pty",
                JavascriptSyncRpcRequest {
                    id: 1,
                    method: String::from("__pty_set_raw_mode"),
                    args: vec![json!(true)],
                },
            )
            .expect("enable raw mode");
            assert_eq!(raw, Value::Null);

            {
                let vm = sidecar.vms.get(&vm_id).expect("javascript vm");
                let kernel_pid = vm.active_processes["proc-js-pty"].kernel_pid;
                let termios = vm
                    .kernel
                    .tcgetattr(EXECUTION_DRIVER_NAME, kernel_pid, 0)
                    .expect("read raw termios");
                assert!(!termios.icanon);
                assert!(!termios.echo);
                assert!(!termios.isig);
            }

            let cooked = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-pty",
                JavascriptSyncRpcRequest {
                    id: 2,
                    method: String::from("__pty_set_raw_mode"),
                    args: vec![json!(false)],
                },
            )
            .expect("disable raw mode");
            assert_eq!(cooked, Value::Null);

            {
                let vm = sidecar.vms.get(&vm_id).expect("javascript vm");
                let kernel_pid = vm.active_processes["proc-js-pty"].kernel_pid;
                let termios = vm
                    .kernel
                    .tcgetattr(EXECUTION_DRIVER_NAME, kernel_pid, 0)
                    .expect("read restored cooked termios");
                assert!(termios.icanon);
                assert!(termios.echo);
                assert!(termios.isig);
            }

            sidecar
                .kill_process_internal(&vm_id, "proc-js-pty", "SIGKILL")
                .expect("kill javascript pty process");
        }
        fn dispose_vm_removes_per_vm_javascript_import_cache_directory() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_a = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm a");
            let vm_b = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm b");

            let cache_path_a = sidecar
                .javascript_engine
                .materialize_import_cache_for_vm(&vm_a)
                .expect("materialize vm a import cache")
                .to_path_buf();
            let cache_path_b = sidecar
                .javascript_engine
                .materialize_import_cache_for_vm(&vm_b)
                .expect("materialize vm b import cache")
                .to_path_buf();
            let cache_root_a = cache_path_a
                .parent()
                .expect("vm a cache parent")
                .to_path_buf();
            let cache_root_b = cache_path_b
                .parent()
                .expect("vm b cache parent")
                .to_path_buf();

            assert_ne!(cache_root_a, cache_root_b);
            assert!(cache_root_a.exists(), "vm a cache root should exist");
            assert!(cache_root_b.exists(), "vm b cache root should exist");

            sidecar
                .dispose_vm_internal_blocking(
                    &connection_id,
                    &session_id,
                    &vm_a,
                    DisposeReason::Requested,
                )
                .expect("dispose vm a");

            assert!(
                !cache_root_a.exists(),
                "vm a cache root should be removed on dispose"
            );
            assert!(
                cache_root_b.exists(),
                "vm b cache root should remain until that VM is disposed"
            );
            assert!(
                sidecar
                    .javascript_engine
                    .import_cache_path_for_vm(&vm_a)
                    .is_none(),
                "vm a cache entry should be removed from the engine"
            );
            assert_eq!(
                sidecar.javascript_engine.import_cache_path_for_vm(&vm_b),
                Some(cache_path_b.as_path())
            );

            sidecar
                .dispose_vm_internal_blocking(
                    &connection_id,
                    &session_id,
                    &vm_b,
                    DisposeReason::Requested,
                )
                .expect("dispose vm b");
            assert!(
                !cache_root_b.exists(),
                "vm b cache root should be removed on dispose"
            );
        }
        fn execution_dispose_vm_race_skips_stale_process_events_without_panicking() {
            with_panic_counter(|panic_counter| {
                let mut sidecar = create_test_sidecar();
                let (connection_id, session_id) = authenticate_and_open_session(&mut sidecar)
                    .expect("authenticate and open session");

                for _iteration in 0..16 {
                    let vm_id = create_vm(
                        &mut sidecar,
                        &connection_id,
                        &session_id,
                        PermissionsPolicy::allow_all(),
                    )
                    .expect("create vm");

                    sidecar
                        .dispose_vm_internal_blocking(
                            &connection_id,
                            &session_id,
                            &vm_id,
                            DisposeReason::Requested,
                        )
                        .expect("dispose vm");

                    assert!(sidecar
                        .handle_execution_event(
                            &vm_id,
                            "proc-js-race",
                            crate::state::ActiveExecutionEvent::Exited(0),
                        )
                        .expect("handle stale exited event")
                        .is_none());
                    assert!(sidecar
                        .handle_execution_event(
                            &vm_id,
                            "proc-js-race",
                            crate::state::ActiveExecutionEvent::Stdout(b"stale stdout".to_vec(),),
                        )
                        .expect("handle stale stdout event")
                        .is_none());
                    assert_eq!(
                        panic_counter.load(Ordering::SeqCst),
                        0,
                        "stale VM/process events should not panic after dispose"
                    );

                    let live_vm_id = create_vm(
                        &mut sidecar,
                        &connection_id,
                        &session_id,
                        PermissionsPolicy::allow_all(),
                    )
                    .expect("create live vm");
                    let vm = sidecar.vms.get_mut(&live_vm_id).expect("live vm");
                    vm.active_processes.remove("proc-js-race");
                    assert!(sidecar
                        .handle_execution_event(
                            &live_vm_id,
                            "proc-js-race",
                            crate::state::ActiveExecutionEvent::Exited(0),
                        )
                        .expect("handle stale process event")
                        .is_none());
                    sidecar
                        .dispose_vm_internal_blocking(
                            &connection_id,
                            &session_id,
                            &live_vm_id,
                            DisposeReason::Requested,
                        )
                        .expect("dispose live vm");
                }
            });
        }
        fn execution_javascript_sync_rpc_handler_ignores_stale_vm_and_process_races() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let request = JavascriptSyncRpcRequest {
                id: 1,
                method: String::from("process.kill"),
                args: vec![json!(999_999u32), json!("SIGTERM")],
            };

            let disposed_vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create disposed vm");
            sidecar
                .dispose_vm_internal_blocking(
                    &connection_id,
                    &session_id,
                    &disposed_vm_id,
                    DisposeReason::Requested,
                )
                .expect("dispose vm");
            sidecar
                .handle_javascript_sync_rpc_request(
                    &disposed_vm_id,
                    "proc-js-race",
                    request.clone(),
                )
                .expect("ignore stale vm javascript sync rpc");

            let live_vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create live vm");
            sidecar
                .handle_javascript_sync_rpc_request(&live_vm_id, "proc-js-race", request)
                .expect("ignore stale process javascript sync rpc");
        }
        fn execution_poll_event_smoke_skips_queued_stale_process_envelopes_after_dispose() {
            with_panic_counter(|panic_counter| {
                let mut sidecar = create_test_sidecar();
                let (connection_id, session_id) = authenticate_and_open_session(&mut sidecar)
                    .expect("authenticate and open session");

                for _iteration in 0..16 {
                    let vm_id = create_vm(
                        &mut sidecar,
                        &connection_id,
                        &session_id,
                        PermissionsPolicy::allow_all(),
                    )
                    .expect("create vm");
                    let ownership = OwnershipScope::vm(&connection_id, &session_id, &vm_id);

                    sidecar
                        .process_event_sender
                        .send(crate::state::ProcessEventEnvelope {
                            connection_id: connection_id.clone(),
                            session_id: session_id.clone(),
                            vm_id: vm_id.clone(),
                            process_id: String::from("proc-js-race"),
                            event: crate::state::ActiveExecutionEvent::Stdout(
                                b"stale stdout".to_vec(),
                            ),
                        })
                        .expect("queue stale stdout envelope");
                    sidecar
                        .process_event_sender
                        .send(crate::state::ProcessEventEnvelope {
                            connection_id: connection_id.clone(),
                            session_id: session_id.clone(),
                            vm_id: vm_id.clone(),
                            process_id: String::from("proc-js-race"),
                            event: crate::state::ActiveExecutionEvent::Exited(0),
                        })
                        .expect("queue stale exited envelope");

                    sidecar
                        .dispose_vm_internal_blocking(
                            &connection_id,
                            &session_id,
                            &vm_id,
                            DisposeReason::Requested,
                        )
                        .expect("dispose vm");

                    assert!(sidecar
                        .poll_event_blocking(&ownership, Duration::ZERO)
                        .expect("poll stale envelopes")
                        .is_none());
                    assert_eq!(
                        panic_counter.load(Ordering::SeqCst),
                        0,
                        "queued stale process envelopes should not panic after dispose"
                    );
                }
            });
        }
        fn execution_poll_event_concurrent_dispose_logs_stale_process_event() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");

            for _iteration in 0..16 {
                let vm_id = create_vm(
                    &mut sidecar,
                    &connection_id,
                    &session_id,
                    PermissionsPolicy::allow_all(),
                )
                .expect("create vm");
                let ownership = OwnershipScope::vm(&connection_id, &session_id, &vm_id);
                let initial_log_count = sidecar
                    .with_bridge_mut(|bridge| bridge.log_events.len())
                    .expect("read initial log count");
                let barrier = Arc::new(Barrier::new(2));
                let sender = sidecar.process_event_sender.clone();
                let sender_barrier = Arc::clone(&barrier);
                let sender_connection_id = connection_id.clone();
                let sender_session_id = session_id.clone();
                let sender_vm_id = vm_id.clone();

                let send_thread = thread::spawn(move || {
                    sender_barrier.wait();
                    sender
                        .send(crate::state::ProcessEventEnvelope {
                            connection_id: sender_connection_id,
                            session_id: sender_session_id,
                            vm_id: sender_vm_id,
                            process_id: String::from("proc-js-race"),
                            event: crate::state::ActiveExecutionEvent::Stdout(
                                b"stale stdout".to_vec(),
                            ),
                        })
                        .expect("queue concurrent stale stdout envelope");
                });

                barrier.wait();
                sidecar
                    .dispose_vm_internal_blocking(
                        &connection_id,
                        &session_id,
                        &vm_id,
                        DisposeReason::Requested,
                    )
                    .expect("dispose vm");
                send_thread.join().expect("join sender thread");

                assert!(sidecar
                    .poll_event_blocking(&ownership, Duration::ZERO)
                    .expect("poll concurrent stale envelope")
                    .is_none());

                let stale_logs = sidecar
                    .with_bridge_mut(|bridge| {
                        bridge.log_events[initial_log_count..]
                            .iter()
                            .filter(|log| {
                                log.vm_id == vm_id
                                    && log.message.contains(
                                        "Ignoring stale process event during execution event dispatch",
                                    )
                                    && log.message.contains("proc-js-race")
                            })
                            .map(|log| log.message.clone())
                            .collect::<Vec<_>>()
                    })
                    .expect("read stale log events");
                assert!(
                    !stale_logs.is_empty(),
                    "expected stale process event log after concurrent dispose race"
                );
            }
        }
        fn filesystem_requests_ignore_stale_vm_and_process_races() {
            with_panic_counter(|panic_counter| {
                let mut sidecar = create_test_sidecar();
                let (connection_id, session_id) = authenticate_and_open_session(&mut sidecar)
                    .expect("authenticate and open session");

                let disposed_vm_id = create_vm(
                    &mut sidecar,
                    &connection_id,
                    &session_id,
                    PermissionsPolicy::allow_all(),
                )
                .expect("create disposed vm");
                let disposed_ownership =
                    OwnershipScope::vm(&connection_id, &session_id, &disposed_vm_id);

                sidecar
                    .dispose_vm_internal_blocking(
                        &connection_id,
                        &session_id,
                        &disposed_vm_id,
                        DisposeReason::Requested,
                    )
                    .expect("dispose vm");

                let stale_guest_request = sidecar
                    .dispatch_blocking(request(
                        4,
                        disposed_ownership,
                        RequestPayload::GuestFilesystemCall(GuestFilesystemCallRequest {
                            operation: GuestFilesystemOperation::WriteFile,
                            path: String::from("/stale.txt"),
                            destination_path: None,
                            target: None,
                            content: Some(String::from("stale")),
                            encoding: Some(RootFilesystemEntryEncoding::Utf8),
                            recursive: false,
                            mode: None,
                            uid: None,
                            gid: None,
                            atime_ms: None,
                            mtime_ms: None,
                            len: None,
                        }),
                    ))
                    .expect("dispatch stale guest filesystem request");
                match stale_guest_request.response.payload {
                    ResponsePayload::Rejected(rejected) => {
                        assert_eq!(rejected.code, "invalid_state");
                        assert!(
                            rejected.message.contains("unknown sidecar VM"),
                            "unexpected stale guest filesystem rejection: {rejected:?}"
                        );
                    }
                    other => panic!("unexpected stale guest filesystem response: {other:?}"),
                }

                let live_vm_id = create_vm(
                    &mut sidecar,
                    &connection_id,
                    &session_id,
                    PermissionsPolicy::allow_all(),
                )
                .expect("create live vm");

                {
                    let vm = sidecar.vms.get(&live_vm_id).expect("live vm");
                    assert!(
                        !vm.kernel
                            .exists("/workspace")
                            .expect("check missing workspace before stale python rpc"),
                        "stale python request precondition failed"
                    );
                }

                sidecar
                    .handle_python_vfs_rpc_request(
                        &live_vm_id,
                        "proc-stale-python",
                        PythonVfsRpcRequest {
                            id: 1,
                            method: PythonVfsRpcMethod::Mkdir,
                            path: String::from("/workspace"),
                            content_base64: None,
                            recursive: false,
                            url: None,
                            http_method: None,
                            headers: BTreeMap::new(),
                            body_base64: None,
                            hostname: None,
                            family: None,
                            command: None,
                            args: Vec::new(),
                            cwd: None,
                            env: BTreeMap::new(),
                            shell: false,
                            max_buffer: None,
                        },
                    )
                    .expect("ignore stale python vfs process");

                {
                    let vm = sidecar.vms.get(&live_vm_id).expect("live vm");
                    assert!(
                        !vm.kernel
                            .exists("/workspace")
                            .expect("check stale python rpc did not mutate kernel"),
                        "stale python VFS request should not mutate the kernel"
                    );
                }

                sidecar
                    .handle_python_vfs_rpc_request(
                        &disposed_vm_id,
                        "proc-stale-python",
                        PythonVfsRpcRequest {
                            id: 2,
                            method: PythonVfsRpcMethod::Mkdir,
                            path: String::from("/workspace"),
                            content_base64: None,
                            recursive: false,
                            url: None,
                            http_method: None,
                            headers: BTreeMap::new(),
                            body_base64: None,
                            hostname: None,
                            family: None,
                            command: None,
                            args: Vec::new(),
                            cwd: None,
                            env: BTreeMap::new(),
                            shell: false,
                            max_buffer: None,
                        },
                    )
                    .expect("ignore stale python vfs vm");

                let write_response = sidecar
                    .dispatch_blocking(request(
                        5,
                        OwnershipScope::vm(&connection_id, &session_id, &live_vm_id),
                        RequestPayload::GuestFilesystemCall(GuestFilesystemCallRequest {
                            operation: GuestFilesystemOperation::WriteFile,
                            path: String::from("/note.txt"),
                            destination_path: None,
                            target: None,
                            content: Some(String::from("hello from live vm")),
                            encoding: Some(RootFilesystemEntryEncoding::Utf8),
                            recursive: false,
                            mode: None,
                            uid: None,
                            gid: None,
                            atime_ms: None,
                            mtime_ms: None,
                            len: None,
                        }),
                    ))
                    .expect("dispatch live guest filesystem write");
                match write_response.response.payload {
                    ResponsePayload::GuestFilesystemResult(response) => {
                        assert_eq!(response.operation, GuestFilesystemOperation::WriteFile);
                        assert_eq!(response.path, "/note.txt");
                    }
                    other => panic!("unexpected live guest filesystem write response: {other:?}"),
                }

                let read_response = sidecar
                    .dispatch_blocking(request(
                        6,
                        OwnershipScope::vm(&connection_id, &session_id, &live_vm_id),
                        RequestPayload::GuestFilesystemCall(GuestFilesystemCallRequest {
                            operation: GuestFilesystemOperation::ReadFile,
                            path: String::from("/note.txt"),
                            destination_path: None,
                            target: None,
                            content: None,
                            encoding: None,
                            recursive: false,
                            mode: None,
                            uid: None,
                            gid: None,
                            atime_ms: None,
                            mtime_ms: None,
                            len: None,
                        }),
                    ))
                    .expect("dispatch live guest filesystem read");
                match read_response.response.payload {
                    ResponsePayload::GuestFilesystemResult(response) => {
                        assert_eq!(response.operation, GuestFilesystemOperation::ReadFile);
                        assert_eq!(response.path, "/note.txt");
                        assert_eq!(response.content.as_deref(), Some("hello from live vm"));
                        assert_eq!(response.encoding, Some(RootFilesystemEntryEncoding::Utf8));
                    }
                    other => panic!("unexpected live guest filesystem read response: {other:?}"),
                }

                assert_eq!(
                    panic_counter.load(Ordering::SeqCst),
                    0,
                    "stale filesystem races should not panic"
                );
            });
        }
        fn get_zombie_timer_count_reports_kernel_state_before_and_after_waitpid() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");

            let zombie_pid = {
                let vm = sidecar.vms.get_mut(&vm_id).expect("configured vm");
                vm.kernel
                    .register_driver(CommandDriver::new("test-driver", ["test-zombie"]))
                    .expect("register test driver");
                let process = vm
                    .kernel
                    .spawn_process(
                        "test-zombie",
                        Vec::new(),
                        SpawnOptions {
                            requester_driver: Some(String::from("test-driver")),
                            ..SpawnOptions::default()
                        },
                    )
                    .expect("spawn test process");
                process.finish(17);
                assert_eq!(vm.kernel.zombie_timer_count(), 1);
                process.pid()
            };

            let zombie_count = sidecar
                .dispatch_blocking(request(
                    4,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::GetZombieTimerCount(GetZombieTimerCountRequest::default()),
                ))
                .expect("query zombie count");
            match zombie_count.response.payload {
                ResponsePayload::ZombieTimerCount(response) => assert_eq!(response.count, 1),
                other => panic!("unexpected zombie count response: {other:?}"),
            }

            {
                let vm = sidecar.vms.get_mut(&vm_id).expect("configured vm");
                let waited = vm.kernel.waitpid(zombie_pid).expect("waitpid");
                assert_eq!(waited.pid, zombie_pid);
                assert_eq!(waited.status, 17);
                assert_eq!(vm.kernel.zombie_timer_count(), 0);
            }

            let reaped_count = sidecar
                .dispatch_blocking(request(
                    5,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::GetZombieTimerCount(GetZombieTimerCountRequest::default()),
                ))
                .expect("query reaped zombie count");
            match reaped_count.response.payload {
                ResponsePayload::ZombieTimerCount(response) => assert_eq!(response.count, 0),
                other => panic!("unexpected zombie count response: {other:?}"),
            }
        }
        fn parse_signal_accepts_full_guest_signal_table() {
            assert_eq!(parse_signal("SIGINT").expect("parse SIGINT"), libc::SIGINT);
            assert_eq!(parse_signal("kill").expect("parse SIGKILL"), SIGKILL);
            assert_eq!(parse_signal("15").expect("parse numeric SIGTERM"), SIGTERM);
            assert_eq!(
                parse_signal("SIGCONT").expect("parse SIGCONT"),
                libc::SIGCONT
            );
            assert_eq!(
                parse_signal("SIGSTOP").expect("parse SIGSTOP"),
                libc::SIGSTOP
            );
            assert_eq!(parse_signal("0").expect("parse signal 0"), 0);
            assert_eq!(
                parse_signal("SIGUSR1").expect("parse SIGUSR1"),
                libc::SIGUSR1
            );
            assert_eq!(parse_signal("SIGIOT").expect("parse SIGIOT"), libc::SIGABRT);
            assert_eq!(parse_signal("SIGPOLL").expect("parse SIGPOLL"), libc::SIGIO);
            assert!(parse_signal("32").is_err());
        }
        fn runtime_child_liveness_only_tracks_owned_children() {
            assert!(
                !runtime_child_is_alive(std::process::id()).expect("current pid is not a child"),
                "current process should not be treated as a guest runtime child"
            );

            let mut child = Command::new("sh")
                .arg("-c")
                .arg("sleep 10")
                .spawn()
                .expect("spawn child process");
            let child_pid = child.id();

            assert!(
                runtime_child_is_alive(child_pid).expect("inspect running child"),
                "running child should be considered alive"
            );

            signal_runtime_process(child_pid, SIGTERM).expect("signal running child");
            child.wait().expect("wait for signaled child");

            assert!(
                !runtime_child_is_alive(child_pid).expect("inspect reaped child"),
                "reaped child should no longer be considered alive"
            );
            signal_runtime_process(child_pid, SIGTERM).expect("ignore reaped child");
        }
        fn authenticated_connection_id_returns_error_for_unexpected_response() {
            let error = authenticated_connection_id(DispatchResult {
                response: ResponseFrame::new(
                    1,
                    OwnershipScope::connection("conn-1"),
                    ResponsePayload::SessionOpened(SessionOpenedResponse {
                        session_id: String::from("session-1"),
                        owner_connection_id: String::from("conn-1"),
                    }),
                ),
                events: Vec::new(),
            })
            .expect_err("unexpected auth payload should return an error");

            match error {
                SidecarError::InvalidState(message) => {
                    assert!(message.contains("expected authenticated response"));
                    assert!(message.contains("SessionOpened"));
                }
                other => panic!("expected invalid_state error, got {other:?}"),
            }
        }
        fn opened_session_id_returns_error_for_unexpected_response() {
            let error = opened_session_id(DispatchResult {
                response: ResponseFrame::new(
                    2,
                    OwnershipScope::connection("conn-1"),
                    ResponsePayload::VmCreated(VmCreatedResponse {
                        vm_id: String::from("vm-1"),
                    }),
                ),
                events: Vec::new(),
            })
            .expect_err("unexpected session payload should return an error");

            match error {
                SidecarError::InvalidState(message) => {
                    assert!(message.contains("expected session_opened response"));
                    assert!(message.contains("VmCreated"));
                }
                other => panic!("expected invalid_state error, got {other:?}"),
            }
        }
        fn created_vm_id_returns_error_for_unexpected_response() {
            let error = created_vm_id(DispatchResult {
                response: ResponseFrame::new(
                    3,
                    OwnershipScope::session("conn-1", "session-1"),
                    ResponsePayload::Rejected(RejectedResponse {
                        code: String::from("invalid_state"),
                        message: String::from("not owned"),
                    }),
                ),
                events: Vec::new(),
            })
            .expect_err("unexpected vm payload should return an error");

            match error {
                SidecarError::InvalidState(message) => {
                    assert!(message.contains("expected vm_created response"));
                    assert!(message.contains("Rejected"));
                }
                other => panic!("expected invalid_state error, got {other:?}"),
            }
        }
        fn configure_vm_instantiates_memory_mounts_through_the_plugin_registry() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");

            sidecar
                .dispatch_blocking(request(
                    4,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::BootstrapRootFilesystem(BootstrapRootFilesystemRequest {
                        entries: vec![
                            RootFilesystemEntry {
                                path: String::from("/workspace"),
                                kind: RootFilesystemEntryKind::Directory,
                                ..Default::default()
                            },
                            RootFilesystemEntry {
                                path: String::from("/workspace/root-only.txt"),
                                kind: RootFilesystemEntryKind::File,
                                content: Some(String::from("root bootstrap file")),
                                ..Default::default()
                            },
                        ],
                    }),
                ))
                .expect("bootstrap root workspace");

            sidecar
                .dispatch_blocking(request(
                    5,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::ConfigureVm(ConfigureVmRequest {
                        mounts: vec![MountDescriptor {
                            guest_path: String::from("/workspace"),
                            read_only: false,
                            plugin: MountPluginDescriptor {
                                id: String::from("memory"),
                                config: json!({}),
                            },
                        }],
                        software: Vec::new(),
                        permissions: None,
                        module_access_cwd: None,
                        instructions: Vec::new(),
                        projected_modules: Vec::new(),
                        command_permissions: BTreeMap::new(),
                        allowed_node_builtins: Vec::new(),
                        loopback_exempt_ports: Vec::new(),
                    }),
                ))
                .expect("configure mounts");

            let vm = sidecar.vms.get_mut(&vm_id).expect("configured vm");
            let hidden = vm
                .kernel
                .filesystem_mut()
                .read_file("/workspace/root-only.txt")
                .expect_err("mounted filesystem should hide root-backed file");
            assert_eq!(hidden.code(), "ENOENT");

            vm.kernel
                .filesystem_mut()
                .write_file("/workspace/from-mount.txt", b"native mount".to_vec())
                .expect("write mounted file");
            assert_eq!(
                vm.kernel
                    .filesystem_mut()
                    .read_file("/workspace/from-mount.txt")
                    .expect("read mounted file"),
                b"native mount".to_vec()
            );
            assert_eq!(
                vm.kernel.mounted_filesystems(),
                vec![
                    MountEntry {
                        path: String::from("/workspace"),
                        plugin_id: String::from("memory"),
                        read_only: false,
                    },
                    MountEntry {
                        path: String::from("/"),
                        plugin_id: String::from("root"),
                        read_only: false,
                    },
                ]
            );
        }
        fn configure_vm_applies_read_only_mount_wrappers() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");

            sidecar
                .dispatch_blocking(request(
                    4,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::ConfigureVm(ConfigureVmRequest {
                        mounts: vec![MountDescriptor {
                            guest_path: String::from("/readonly"),
                            read_only: true,
                            plugin: MountPluginDescriptor {
                                id: String::from("memory"),
                                config: json!({}),
                            },
                        }],
                        software: Vec::new(),
                        permissions: None,
                        module_access_cwd: None,
                        instructions: Vec::new(),
                        projected_modules: Vec::new(),
                        command_permissions: BTreeMap::new(),
                        allowed_node_builtins: Vec::new(),
                        loopback_exempt_ports: Vec::new(),
                    }),
                ))
                .expect("configure readonly mount");

            let vm = sidecar.vms.get_mut(&vm_id).expect("configured vm");
            let error = vm
                .kernel
                .filesystem_mut()
                .write_file("/readonly/blocked.txt", b"nope".to_vec())
                .expect_err("readonly mount should reject writes");
            assert_eq!(error.code(), "EROFS");
        }
        fn configure_vm_instantiates_host_dir_mounts_through_the_plugin_registry() {
            let host_dir = temp_dir("agent-os-sidecar-host-dir");
            fs::write(host_dir.join("hello.txt"), "hello from host").expect("seed host dir");

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");

            sidecar
                .dispatch_blocking(request(
                    4,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::BootstrapRootFilesystem(BootstrapRootFilesystemRequest {
                        entries: vec![
                            RootFilesystemEntry {
                                path: String::from("/workspace"),
                                kind: RootFilesystemEntryKind::Directory,
                                ..Default::default()
                            },
                            RootFilesystemEntry {
                                path: String::from("/workspace/root-only.txt"),
                                kind: RootFilesystemEntryKind::File,
                                content: Some(String::from("root bootstrap file")),
                                ..Default::default()
                            },
                        ],
                    }),
                ))
                .expect("bootstrap root workspace");

            sidecar
                .dispatch_blocking(request(
                    5,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::ConfigureVm(ConfigureVmRequest {
                        mounts: vec![MountDescriptor {
                            guest_path: String::from("/workspace"),
                            read_only: false,
                            plugin: MountPluginDescriptor {
                                id: String::from("host_dir"),
                                config: json!({
                                    "hostPath": host_dir,
                                    "readOnly": false,
                                }),
                            },
                        }],
                        software: Vec::new(),
                        permissions: None,
                        module_access_cwd: None,
                        instructions: Vec::new(),
                        projected_modules: Vec::new(),
                        command_permissions: BTreeMap::new(),
                        allowed_node_builtins: Vec::new(),
                        loopback_exempt_ports: Vec::new(),
                    }),
                ))
                .expect("configure host_dir mount");

            let vm = sidecar.vms.get_mut(&vm_id).expect("configured vm");
            let hidden = vm
                .kernel
                .filesystem_mut()
                .read_file("/workspace/root-only.txt")
                .expect_err("mounted host dir should hide root-backed file");
            assert_eq!(hidden.code(), "ENOENT");
            assert_eq!(
                vm.kernel
                    .filesystem_mut()
                    .read_file("/workspace/hello.txt")
                    .expect("read mounted host file"),
                b"hello from host".to_vec()
            );

            vm.kernel
                .filesystem_mut()
                .write_file("/workspace/from-vm.txt", b"native host dir".to_vec())
                .expect("write host dir file");
            assert_eq!(
                fs::read_to_string(host_dir.join("from-vm.txt")).expect("read host output"),
                "native host dir"
            );

            fs::remove_dir_all(host_dir).expect("remove temp dir");
        }
        fn configure_vm_js_bridge_mount_dispatches_filesystem_calls_via_sidecar_requests() {
            let mut sidecar = create_test_sidecar();
            let (filesystem, calls) = install_memory_js_bridge_handler(&mut sidecar);
            filesystem
                .lock()
                .expect("lock js bridge fs")
                .write_file("/original.txt", b"hello world".to_vec())
                .expect("seed js bridge fs");

            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");

            sidecar
                .dispatch_blocking(request(
                    4,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::ConfigureVm(ConfigureVmRequest {
                        mounts: vec![MountDescriptor {
                            guest_path: String::from("/workspace"),
                            read_only: false,
                            plugin: MountPluginDescriptor {
                                id: String::from("js_bridge"),
                                config: json!({ "mountId": "mount-1" }),
                            },
                        }],
                        software: Vec::new(),
                        permissions: None,
                        module_access_cwd: None,
                        instructions: Vec::new(),
                        projected_modules: Vec::new(),
                        command_permissions: BTreeMap::new(),
                        allowed_node_builtins: Vec::new(),
                        loopback_exempt_ports: Vec::new(),
                    }),
                ))
                .expect("configure js_bridge mount");

            let vm = sidecar.vms.get_mut(&vm_id).expect("configured vm");
            vm.kernel
                .filesystem_mut()
                .link("/workspace/original.txt", "/workspace/linked.txt")
                .expect("create js bridge hard link");
            vm.kernel
                .filesystem_mut()
                .write_file("/workspace/linked.txt", b"updated".to_vec())
                .expect("write through linked file");
            vm.kernel
                .filesystem_mut()
                .chown("/workspace/original.txt", 2000, 3000)
                .expect("update ownership");
            vm.kernel
                .filesystem_mut()
                .utimes(
                    "/workspace/linked.txt",
                    1_700_000_000_000,
                    1_710_000_000_000,
                )
                .expect("update timestamps");

            let original = vm
                .kernel
                .filesystem_mut()
                .stat("/workspace/original.txt")
                .expect("stat original");
            let linked = vm
                .kernel
                .filesystem_mut()
                .stat("/workspace/linked.txt")
                .expect("stat linked");
            assert_eq!(original.ino, linked.ino);
            assert_eq!(original.nlink, 2);
            assert_eq!(linked.nlink, 2);
            assert_eq!(original.uid, 2000);
            assert_eq!(original.gid, 3000);
            assert_eq!(linked.uid, 2000);
            assert_eq!(linked.gid, 3000);
            assert_eq!(original.atime_ms, 1_700_000_000_000);
            assert_eq!(original.mtime_ms, 1_710_000_000_000);
            assert_eq!(
                vm.kernel
                    .filesystem_mut()
                    .read_file("/workspace/original.txt")
                    .expect("read original through js bridge"),
                b"updated".to_vec()
            );

            let calls = calls.lock().expect("lock js bridge calls");
            assert!(calls.iter().any(|call| {
                call.mount_id == "mount-1"
                    && call.operation == "link"
                    && call.path.is_none()
                    && call.ownership == OwnershipScope::vm(&connection_id, &session_id, &vm_id)
            }));
            assert!(calls.iter().any(|call| {
                call.mount_id == "mount-1"
                    && call.operation == "writeFile"
                    && call.path.as_deref() == Some("/linked.txt")
            }));
            assert!(calls.iter().any(|call| {
                call.mount_id == "mount-1"
                    && call.operation == "stat"
                    && call.path.as_deref() == Some("/original.txt")
            }));
        }
        fn configure_vm_js_bridge_mount_maps_callback_errors_to_errno_codes() {
            let mut sidecar = create_test_sidecar();
            sidecar.set_sidecar_request_handler(|request| {
                let SidecarRequestPayload::JsBridgeCall(call) = &request.payload else {
                    return Err(SidecarError::InvalidState(String::from(
                        "expected js_bridge_call payload",
                    )));
                };
                let path = call.args.get("path").and_then(Value::as_str);
                if path == Some("/") {
                    return match call.operation.as_str() {
                        "exists" => js_bridge_result(request, Some(Value::Bool(true)), None),
                        "stat" | "lstat" => js_bridge_result(
                            request,
                            Some(stat_json(VirtualStat {
                                mode: 0o755,
                                size: 0,
                                blocks: 0,
                                dev: 1,
                                rdev: 0,
                                is_directory: true,
                                is_symbolic_link: false,
                                atime_ms: 0,
                                atime_nsec: 0,
                                mtime_ms: 0,
                                mtime_nsec: 0,
                                ctime_ms: 0,
                                ctime_nsec: 0,
                                birthtime_ms: 0,
                                ino: 1,
                                nlink: 1,
                                uid: 0,
                                gid: 0,
                            })),
                            None,
                        ),
                        "readDir" => js_bridge_result(request, Some(json!([])), None),
                        "readDirWithTypes" => {
                            js_bridge_result(request, Some(Value::Array(Vec::new())), None)
                        }
                        "realpath" => js_bridge_result(request, Some(json!("/")), None),
                        _ => js_bridge_result(request, None, None),
                    };
                }

                let error = match (call.operation.as_str(), path) {
                    ("realpath", Some("/missing.txt")) | ("readFile", Some("/missing.txt")) => {
                        "not found"
                    }
                    ("writeFile", Some("/output.txt")) => "permission denied",
                    ("rename", _) => "already exists",
                    ("stat", Some("/anything.txt")) => "unexpected js bridge failure",
                    _ => return js_bridge_result(request, None, None),
                };
                js_bridge_result(request, None, Some(error))
            });

            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");

            sidecar
                .dispatch_blocking(request(
                    4,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::ConfigureVm(ConfigureVmRequest {
                        mounts: vec![MountDescriptor {
                            guest_path: String::from("/workspace"),
                            read_only: false,
                            plugin: MountPluginDescriptor {
                                id: String::from("js_bridge"),
                                config: json!({ "mountId": "mount-errors" }),
                            },
                        }],
                        software: Vec::new(),
                        permissions: None,
                        module_access_cwd: None,
                        instructions: Vec::new(),
                        projected_modules: Vec::new(),
                        command_permissions: BTreeMap::new(),
                        allowed_node_builtins: Vec::new(),
                        loopback_exempt_ports: Vec::new(),
                    }),
                ))
                .expect("configure js_bridge mount");

            let vm = sidecar.vms.get_mut(&vm_id).expect("configured vm");
            let read_error = vm
                .kernel
                .filesystem_mut()
                .read_file("/workspace/missing.txt")
                .expect_err("read should fail");
            assert_eq!(read_error.code(), "ENOENT");

            let write_error = vm
                .kernel
                .filesystem_mut()
                .write_file("/workspace/output.txt", b"blocked".to_vec())
                .expect_err("write should fail");
            assert_eq!(write_error.code(), "EACCES");

            let rename_error = vm
                .kernel
                .filesystem_mut()
                .rename("/workspace/a.txt", "/workspace/b.txt")
                .expect_err("rename should fail");
            assert_eq!(rename_error.code(), "EEXIST");

            let stat_error = vm
                .kernel
                .filesystem_mut()
                .stat("/workspace/anything.txt")
                .expect_err("stat should fail");
            assert_eq!(stat_error.code(), "EIO");
        }
        fn configure_vm_instantiates_sandbox_agent_mounts_through_the_plugin_registry() {
            let server = MockSandboxAgentServer::start("agent-os-sidecar-sandbox", None);
            fs::write(server.root().join("hello.txt"), "hello from sandbox")
                .expect("seed sandbox file");

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");

            sidecar
                .dispatch_blocking(request(
                    4,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::BootstrapRootFilesystem(BootstrapRootFilesystemRequest {
                        entries: vec![
                            RootFilesystemEntry {
                                path: String::from("/sandbox"),
                                kind: RootFilesystemEntryKind::Directory,
                                ..Default::default()
                            },
                            RootFilesystemEntry {
                                path: String::from("/sandbox/root-only.txt"),
                                kind: RootFilesystemEntryKind::File,
                                content: Some(String::from("root bootstrap file")),
                                ..Default::default()
                            },
                        ],
                    }),
                ))
                .expect("bootstrap root sandbox dir");

            sidecar
                .dispatch_blocking(request(
                    5,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::ConfigureVm(ConfigureVmRequest {
                        mounts: vec![MountDescriptor {
                            guest_path: String::from("/sandbox"),
                            read_only: false,
                            plugin: MountPluginDescriptor {
                                id: String::from("sandbox_agent"),
                                config: json!({
                                    "baseUrl": server.base_url(),
                                }),
                            },
                        }],
                        software: Vec::new(),
                        permissions: None,
                        module_access_cwd: None,
                        instructions: Vec::new(),
                        projected_modules: Vec::new(),
                        command_permissions: BTreeMap::new(),
                        allowed_node_builtins: Vec::new(),
                        loopback_exempt_ports: Vec::new(),
                    }),
                ))
                .expect("configure sandbox_agent mount");

            let vm = sidecar.vms.get_mut(&vm_id).expect("configured vm");
            let hidden = vm
                .kernel
                .filesystem_mut()
                .read_file("/sandbox/root-only.txt")
                .expect_err("mounted sandbox should hide root-backed file");
            assert_eq!(hidden.code(), "ENOENT");
            assert_eq!(
                vm.kernel
                    .filesystem_mut()
                    .read_file("/sandbox/hello.txt")
                    .expect("read mounted sandbox file"),
                b"hello from sandbox".to_vec()
            );

            vm.kernel
                .filesystem_mut()
                .write_file("/sandbox/from-vm.txt", b"native sandbox mount".to_vec())
                .expect("write sandbox file");
            assert_eq!(
                fs::read_to_string(server.root().join("from-vm.txt")).expect("read sandbox output"),
                "native sandbox mount"
            );
        }
        fn configure_vm_instantiates_s3_mounts_through_the_plugin_registry() {
            let server = MockS3Server::start();

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");

            sidecar
                .dispatch_blocking(request(
                    4,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::BootstrapRootFilesystem(BootstrapRootFilesystemRequest {
                        entries: vec![
                            RootFilesystemEntry {
                                path: String::from("/data"),
                                kind: RootFilesystemEntryKind::Directory,
                                ..Default::default()
                            },
                            RootFilesystemEntry {
                                path: String::from("/data/root-only.txt"),
                                kind: RootFilesystemEntryKind::File,
                                content: Some(String::from("root bootstrap file")),
                                ..Default::default()
                            },
                        ],
                    }),
                ))
                .expect("bootstrap root s3 dir");

            sidecar
                .dispatch_blocking(request(
                    5,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::ConfigureVm(ConfigureVmRequest {
                        mounts: vec![MountDescriptor {
                            guest_path: String::from("/data"),
                            read_only: false,
                            plugin: MountPluginDescriptor {
                                id: String::from("s3"),
                                config: json!({
                                    "bucket": "test-bucket",
                                    "prefix": "service-test",
                                    "region": "us-east-1",
                                    "endpoint": server.base_url(),
                                    "credentials": {
                                        "accessKeyId": "minioadmin",
                                        "secretAccessKey": "minioadmin",
                                    },
                                    "chunkSize": 8,
                                    "inlineThreshold": 4,
                                }),
                            },
                        }],
                        software: Vec::new(),
                        permissions: None,
                        module_access_cwd: None,
                        instructions: Vec::new(),
                        projected_modules: Vec::new(),
                        command_permissions: BTreeMap::new(),
                        allowed_node_builtins: Vec::new(),
                        loopback_exempt_ports: Vec::new(),
                    }),
                ))
                .expect("configure s3 mount");

            let vm = sidecar.vms.get_mut(&vm_id).expect("configured vm");
            let hidden = vm
                .kernel
                .filesystem_mut()
                .read_file("/data/root-only.txt")
                .expect_err("mounted s3 fs should hide root-backed file");
            assert_eq!(hidden.code(), "ENOENT");

            vm.kernel
                .filesystem_mut()
                .write_file("/data/from-vm.txt", b"native s3 mount".to_vec())
                .expect("write s3-backed file");
            assert_eq!(
                vm.kernel
                    .filesystem_mut()
                    .read_file("/data/from-vm.txt")
                    .expect("read s3-backed file"),
                b"native s3 mount".to_vec()
            );
            drop(sidecar);

            let requests = server.requests();
            assert!(
                requests.iter().any(|request| request.method == "PUT"),
                "expected the native plugin to persist data back to S3"
            );
            assert!(
                requests
                    .iter()
                    .any(|request| request.path.contains("filesystem-manifest.json")),
                "expected the native plugin to store a manifest object"
            );
        }
        fn bridge_permissions_map_symlink_operations_to_symlink_access() {
            let bridge = SharedBridge::new(RecordingBridge::default());
            let permissions = bridge_permissions(bridge.clone(), "vm-symlink");
            let check = permissions
                .filesystem
                .as_ref()
                .expect("filesystem permission callback");

            let decision = check(&FsAccessRequest {
                vm_id: String::from("ignored-by-bridge"),
                op: FsOperation::Symlink,
                path: String::from("/workspace/link.txt"),
            });
            assert!(decision.allow);

            let recorded = bridge
                .inspect(|bridge| bridge.filesystem_permission_requests.clone())
                .expect("inspect bridge");
            assert_eq!(
                recorded,
                vec![FilesystemPermissionRequest {
                    vm_id: String::from("vm-symlink"),
                    path: String::from("/workspace/link.txt"),
                    access: FilesystemAccess::Symlink,
                }]
            );
        }
        fn parse_resource_limits_reads_filesystem_limits() {
            let metadata = BTreeMap::from([
                (String::from("resource.max_sockets"), String::from("8")),
                (String::from("resource.max_connections"), String::from("4")),
                (
                    String::from("resource.max_filesystem_bytes"),
                    String::from("4096"),
                ),
                (
                    String::from("resource.max_inode_count"),
                    String::from("128"),
                ),
                (
                    String::from("resource.max_blocking_read_ms"),
                    String::from("250"),
                ),
                (
                    String::from("resource.max_pread_bytes"),
                    String::from("8192"),
                ),
                (
                    String::from("resource.max_fd_write_bytes"),
                    String::from("4096"),
                ),
                (
                    String::from("resource.max_process_argv_bytes"),
                    String::from("2048"),
                ),
                (
                    String::from("resource.max_process_env_bytes"),
                    String::from("1024"),
                ),
                (
                    String::from("resource.max_readdir_entries"),
                    String::from("32"),
                ),
                (String::from("resource.max_wasm_fuel"), String::from("5000")),
                (
                    String::from("resource.max_wasm_memory_bytes"),
                    String::from("131072"),
                ),
                (
                    String::from("resource.max_wasm_stack_bytes"),
                    String::from("262144"),
                ),
            ]);

            let limits =
                crate::vm::parse_resource_limits(&metadata).expect("parse resource limits");
            assert_eq!(limits.max_sockets, Some(8));
            assert_eq!(limits.max_connections, Some(4));
            assert_eq!(limits.max_filesystem_bytes, Some(4096));
            assert_eq!(limits.max_inode_count, Some(128));
            assert_eq!(limits.max_blocking_read_ms, Some(250));
            assert_eq!(limits.max_pread_bytes, Some(8192));
            assert_eq!(limits.max_fd_write_bytes, Some(4096));
            assert_eq!(limits.max_process_argv_bytes, Some(2048));
            assert_eq!(limits.max_process_env_bytes, Some(1024));
            assert_eq!(limits.max_readdir_entries, Some(32));
            assert_eq!(limits.max_wasm_fuel, Some(5000));
            assert_eq!(limits.max_wasm_memory_bytes, Some(131072));
            assert_eq!(limits.max_wasm_stack_bytes, Some(262144));
        }
        fn create_vm_applies_filesystem_permission_descriptors_to_kernel_access() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                capability_permissions(&[
                    ("fs", PermissionMode::Allow),
                    ("fs.read", PermissionMode::Deny),
                ]),
            )
            .expect("create vm");

            let vm = sidecar.vms.get_mut(&vm_id).expect("configured vm");
            vm.kernel
                .filesystem_mut()
                .write_file("/blocked.txt", b"nope".to_vec())
                .expect("write should be allowed");

            let read_error = vm
                .kernel
                .filesystem_mut()
                .read_file("/blocked.txt")
                .expect_err("read should be denied");
            assert_eq!(read_error.code(), "EACCES");
        }
        fn create_vm_without_permissions_defaults_to_static_deny_all() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let response = sidecar
                .dispatch_blocking(request(
                    3,
                    OwnershipScope::session(&connection_id, &session_id),
                    RequestPayload::CreateVm(CreateVmRequest {
                        runtime: GuestRuntimeKind::JavaScript,
                        metadata: BTreeMap::new(),
                        root_filesystem: Default::default(),
                        permissions: None,
                    }),
                ))
                .expect("create vm");
            let vm_id = created_vm_id(response).expect("vm created");
            let permission_check_count_before_write = sidecar
                .with_bridge_mut(|bridge| bridge.permission_checks.len())
                .expect("read bootstrap permission checks");

            let write_error = sidecar
                .vms
                .get_mut(&vm_id)
                .expect("configured vm")
                .kernel
                .filesystem_mut()
                .write_file("/blocked.txt", b"nope".to_vec())
                .expect_err("write should be denied");
            assert_eq!(write_error.code(), "EACCES");

            let permission_check_count_after_write = sidecar
                .with_bridge_mut(|bridge| bridge.permission_checks.len())
                .expect("read bridge permission checks");
            assert_eq!(
                permission_check_count_after_write, permission_check_count_before_write,
                "guest writes under default-deny should not fall through to bridge callbacks"
            );
        }
        fn configure_vm_rollback_restore_failure_falls_back_to_static_deny_all() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            sidecar
                .bridge
                .queue_set_vm_permissions_result(Ok(()))
                .expect("queue allow-all bootstrap permission set");
            sidecar
                .bridge
                .queue_set_vm_permissions_result(Err(SidecarError::Bridge(String::from(
                    "injected restore failure",
                ))))
                .expect("queue restore failure");

            let response = sidecar
                .dispatch_blocking(request(
                    4,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::ConfigureVm(ConfigureVmRequest {
                        mounts: vec![MountDescriptor {
                            guest_path: String::from("/workspace"),
                            read_only: false,
                            plugin: MountPluginDescriptor {
                                id: String::from("host_dir"),
                                config: json!({
                                    "readOnly": false,
                                }),
                            },
                        }],
                        software: Vec::new(),
                        permissions: None,
                        module_access_cwd: None,
                        instructions: Vec::new(),
                        projected_modules: Vec::new(),
                        command_permissions: BTreeMap::new(),
                        allowed_node_builtins: Vec::new(),
                        loopback_exempt_ports: Vec::new(),
                    }),
                ))
                .expect("dispatch configure_vm failure");

            match response.response.payload {
                ResponsePayload::Rejected(rejected) => {
                    assert_eq!(rejected.code, "invalid_state");
                    let message = rejected.message;
                    assert!(message.contains("configure_vm rollback failed"));
                    assert!(message.contains("injected restore failure"));
                    assert!(message.contains("applied deny-all fallback"));
                }
                other => panic!("expected rejected response, got {other:?}"),
            }

            let stored_permissions = sidecar
                .bridge
                .permissions
                .lock()
                .expect("read stored permissions")
                .get(&vm_id)
                .cloned()
                .expect("vm permissions tracked");
            assert_eq!(stored_permissions, PermissionsPolicy::deny_all());
            assert_eq!(
                sidecar
                    .vms
                    .get(&vm_id)
                    .expect("configured vm")
                    .configuration
                    .permissions,
                PermissionsPolicy::deny_all()
            );

            let permission_check_count_before_write = sidecar
                .with_bridge_mut(|bridge| bridge.permission_checks.len())
                .expect("read bridge permission checks");
            let write_error = sidecar
                .vms
                .get_mut(&vm_id)
                .expect("configured vm")
                .kernel
                .filesystem_mut()
                .write_file("/blocked.txt", b"nope".to_vec())
                .expect_err("write should be denied after failed rollback");
            assert_eq!(write_error.code(), "EACCES");
            let permission_check_count_after_write = sidecar
                .with_bridge_mut(|bridge| bridge.permission_checks.len())
                .expect("read bridge permission checks");
            assert_eq!(
                permission_check_count_after_write, permission_check_count_before_write,
                "guest writes under deny-all fallback should not fall through to bridge callbacks"
            );
        }
        fn toolkit_registration_rollback_restore_failure_keeps_registry_consistent() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");

            let original_toolkit =
                test_toolkit_payload("browser", "Browser automation", "screenshot");
            sidecar
                .dispatch_blocking(request(
                    4,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::RegisterToolkit(original_toolkit.clone()),
                ))
                .expect("register original toolkit");

            let (toolkits_before, command_paths_before) = {
                let vm = sidecar.vms.get(&vm_id).expect("configured vm");
                (vm.toolkits.clone(), vm.command_guest_paths.clone())
            };

            sidecar
                .bridge
                .queue_set_vm_permissions_result(Ok(()))
                .expect("queue allow-all toolkit refresh");
            sidecar
                .bridge
                .queue_set_vm_permissions_result(Err(SidecarError::Bridge(String::from(
                    "injected restore failure",
                ))))
                .expect("queue toolkit restore failure");

            let response = sidecar
                .dispatch_blocking(request(
                    5,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::RegisterToolkit(test_toolkit_payload(
                        "browser",
                        "Replacement browser toolkit",
                        "click",
                    )),
                ))
                .expect("dispatch toolkit registration failure");

            match response.response.payload {
                ResponsePayload::Rejected(rejected) => {
                    assert_eq!(rejected.code, "invalid_state");
                    let message = rejected.message;
                    assert!(message.contains("toolkit registration rollback failed"));
                    assert!(message.contains("injected restore failure"));
                    assert!(message.contains("applied deny-all fallback"));
                }
                other => panic!("expected rejected response, got {other:?}"),
            }

            let stored_permissions = sidecar
                .bridge
                .permissions
                .lock()
                .expect("read stored permissions")
                .get(&vm_id)
                .cloned()
                .expect("vm permissions tracked");
            assert_eq!(stored_permissions, PermissionsPolicy::deny_all());

            let vm = sidecar.vms.get(&vm_id).expect("configured vm");
            assert_eq!(vm.configuration.permissions, PermissionsPolicy::deny_all());
            assert_eq!(vm.toolkits, toolkits_before);
            assert_eq!(vm.command_guest_paths, command_paths_before);
        }
        fn create_vm_rejects_permission_rules_with_empty_operations() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let response = sidecar
                .dispatch_blocking(request(
                    3,
                    OwnershipScope::session(&connection_id, &session_id),
                    RequestPayload::CreateVm(CreateVmRequest {
                        runtime: GuestRuntimeKind::JavaScript,
                        metadata: BTreeMap::new(),
                        root_filesystem: Default::default(),
                        permissions: Some(PermissionsPolicy {
                            fs: Some(FsPermissionScope::Rules(FsPermissionRuleSet {
                                default: Some(PermissionMode::Deny),
                                rules: vec![FsPermissionRule {
                                    mode: PermissionMode::Allow,
                                    operations: Vec::new(),
                                    paths: vec![String::from("*")],
                                }],
                            })),
                            network: None,
                            child_process: None,
                            process: None,
                            env: None,
                            tool: None,
                        }),
                    }),
                ))
                .expect("dispatch create vm");

            match response.response.payload {
                ResponsePayload::Rejected(rejected) => {
                    assert_eq!(rejected.code, "invalid_state");
                    assert!(
                        rejected
                            .message
                            .contains("fs.rules[0].operations must not be empty"),
                        "unexpected rejection: {rejected:?}"
                    );
                }
                other => panic!("expected rejected response, got {other:?}"),
            }
        }
        fn configure_vm_rejects_permission_rules_with_empty_paths_or_patterns() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");

            let fs_response = sidecar
                .dispatch_blocking(request(
                    4,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::ConfigureVm(ConfigureVmRequest {
                        mounts: Vec::new(),
                        software: Vec::new(),
                        permissions: Some(PermissionsPolicy {
                            fs: Some(FsPermissionScope::Rules(FsPermissionRuleSet {
                                default: Some(PermissionMode::Deny),
                                rules: vec![FsPermissionRule {
                                    mode: PermissionMode::Allow,
                                    operations: vec![String::from("read")],
                                    paths: Vec::new(),
                                }],
                            })),
                            network: None,
                            child_process: None,
                            process: None,
                            env: None,
                            tool: None,
                        }),
                        module_access_cwd: None,
                        instructions: Vec::new(),
                        projected_modules: Vec::new(),
                        command_permissions: BTreeMap::new(),
                        allowed_node_builtins: Vec::new(),
                        loopback_exempt_ports: Vec::new(),
                    }),
                ))
                .expect("dispatch fs configure vm");

            match fs_response.response.payload {
                ResponsePayload::Rejected(rejected) => {
                    assert_eq!(rejected.code, "invalid_state");
                    assert!(
                        rejected
                            .message
                            .contains("fs.rules[0].paths must not be empty"),
                        "unexpected rejection: {rejected:?}"
                    );
                }
                other => panic!("expected rejected response, got {other:?}"),
            }

            let network_response = sidecar
                .dispatch_blocking(request(
                    5,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::ConfigureVm(ConfigureVmRequest {
                        mounts: Vec::new(),
                        software: Vec::new(),
                        permissions: Some(PermissionsPolicy {
                            fs: None,
                            network: Some(PatternPermissionScope::Rules(
                                PatternPermissionRuleSet {
                                    default: Some(PermissionMode::Deny),
                                    rules: vec![PatternPermissionRule {
                                        mode: PermissionMode::Allow,
                                        operations: vec![String::from("dns")],
                                        patterns: Vec::new(),
                                    }],
                                },
                            )),
                            child_process: None,
                            process: None,
                            env: None,
                            tool: None,
                        }),
                        module_access_cwd: None,
                        instructions: Vec::new(),
                        projected_modules: Vec::new(),
                        command_permissions: BTreeMap::new(),
                        allowed_node_builtins: Vec::new(),
                        loopback_exempt_ports: Vec::new(),
                    }),
                ))
                .expect("dispatch network configure vm");

            match network_response.response.payload {
                ResponsePayload::Rejected(rejected) => {
                    assert_eq!(rejected.code, "invalid_state");
                    assert!(
                        rejected
                            .message
                            .contains("network.rules[0].patterns must not be empty"),
                        "unexpected rejection: {rejected:?}"
                    );
                }
                other => panic!("expected rejected response, got {other:?}"),
            }
        }
        fn configure_vm_mounts_bypass_guest_fs_write_policy() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            sidecar
                .bridge
                .set_vm_permissions(
                    &vm_id,
                    &capability_permissions(&[("fs.write", PermissionMode::Deny)]),
                )
                .expect("set vm permissions");

            let result = sidecar
                .dispatch_blocking(request(
                    4,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::ConfigureVm(ConfigureVmRequest {
                        mounts: vec![MountDescriptor {
                            guest_path: String::from("/workspace"),
                            read_only: false,
                            plugin: MountPluginDescriptor {
                                id: String::from("memory"),
                                config: json!({}),
                            },
                        }],
                        software: Vec::new(),
                        permissions: None,
                        module_access_cwd: None,
                        instructions: Vec::new(),
                        projected_modules: Vec::new(),
                        command_permissions: BTreeMap::new(),
                        allowed_node_builtins: Vec::new(),
                        loopback_exempt_ports: Vec::new(),
                    }),
                ))
                .expect("dispatch configure vm");

            match result.response.payload {
                ResponsePayload::VmConfigured(response) => {
                    assert_eq!(response.applied_mounts, 1);
                }
                other => panic!("expected configured response, got {other:?}"),
            }
        }
        fn guest_filesystem_link_and_truncate_preserve_hard_link_semantics() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");

            for (request_id, payload) in [
                (
                    4,
                    GuestFilesystemCallRequest {
                        operation: GuestFilesystemOperation::Mkdir,
                        path: String::from("/workspace"),
                        destination_path: None,
                        target: None,
                        content: None,
                        encoding: None,
                        recursive: true,
                        mode: None,
                        uid: None,
                        gid: None,
                        atime_ms: None,
                        mtime_ms: None,
                        len: None,
                    },
                ),
                (
                    5,
                    GuestFilesystemCallRequest {
                        operation: GuestFilesystemOperation::WriteFile,
                        path: String::from("/workspace/note.txt"),
                        destination_path: None,
                        target: None,
                        content: Some(String::from("stdio-sidecar-fs")),
                        encoding: None,
                        recursive: false,
                        mode: None,
                        uid: None,
                        gid: None,
                        atime_ms: None,
                        mtime_ms: None,
                        len: None,
                    },
                ),
                (
                    6,
                    GuestFilesystemCallRequest {
                        operation: GuestFilesystemOperation::Link,
                        path: String::from("/workspace/note.txt"),
                        destination_path: Some(String::from("/workspace/hard.txt")),
                        target: None,
                        content: None,
                        encoding: None,
                        recursive: false,
                        mode: None,
                        uid: None,
                        gid: None,
                        atime_ms: None,
                        mtime_ms: None,
                        len: None,
                    },
                ),
                (
                    7,
                    GuestFilesystemCallRequest {
                        operation: GuestFilesystemOperation::Truncate,
                        path: String::from("/workspace/hard.txt"),
                        destination_path: None,
                        target: None,
                        content: None,
                        encoding: None,
                        recursive: false,
                        mode: None,
                        uid: None,
                        gid: None,
                        atime_ms: None,
                        mtime_ms: None,
                        len: Some(5),
                    },
                ),
                (
                    8,
                    GuestFilesystemCallRequest {
                        operation: GuestFilesystemOperation::Utimes,
                        path: String::from("/workspace/note.txt"),
                        destination_path: None,
                        target: None,
                        content: None,
                        encoding: None,
                        recursive: false,
                        mode: None,
                        uid: None,
                        gid: None,
                        atime_ms: Some(1_700_000_000_000),
                        mtime_ms: Some(1_710_000_000_000),
                        len: None,
                    },
                ),
            ] {
                sidecar
                    .dispatch_blocking(request(
                        request_id,
                        OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                        RequestPayload::GuestFilesystemCall(payload),
                    ))
                    .expect("dispatch guest filesystem request");
            }

            let vm = sidecar.vms.get_mut(&vm_id).expect("configured vm");
            let note_stat = vm
                .kernel
                .stat("/workspace/note.txt")
                .expect("stat source after truncate");
            let hard_stat = vm
                .kernel
                .stat("/workspace/hard.txt")
                .expect("stat hard link after truncate");
            let note = vm
                .kernel
                .read_file("/workspace/note.txt")
                .expect("read source after truncate");
            let hard = vm
                .kernel
                .read_file("/workspace/hard.txt")
                .expect("read hard link after truncate");

            assert_eq!(note, b"stdio".to_vec());
            assert_eq!(hard, b"stdio".to_vec());
            assert_eq!(note_stat.size, 5);
            assert_eq!(hard_stat.size, 5);
            assert_eq!(note_stat.ino, hard_stat.ino);
            assert_eq!(note_stat.nlink, 2);
            assert_eq!(hard_stat.nlink, 2);
            assert_eq!(note_stat.mtime_ms, 1_710_000_000_000);
            assert_eq!(hard_stat.mtime_ms, 1_710_000_000_000);
        }
        fn configure_vm_sensitive_mounts_bypass_guest_fs_mount_sensitive_policy() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            sidecar
                .bridge
                .set_vm_permissions(
                    &vm_id,
                    &capability_permissions(&[
                        ("fs.write", PermissionMode::Allow),
                        ("fs.mount_sensitive", PermissionMode::Deny),
                    ]),
                )
                .expect("set vm permissions");

            let result = sidecar
                .dispatch_blocking(request(
                    4,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::ConfigureVm(ConfigureVmRequest {
                        mounts: vec![MountDescriptor {
                            guest_path: String::from("/etc"),
                            read_only: false,
                            plugin: MountPluginDescriptor {
                                id: String::from("memory"),
                                config: json!({}),
                            },
                        }],
                        software: Vec::new(),
                        permissions: None,
                        module_access_cwd: None,
                        instructions: Vec::new(),
                        projected_modules: Vec::new(),
                        command_permissions: BTreeMap::new(),
                        allowed_node_builtins: Vec::new(),
                        loopback_exempt_ports: Vec::new(),
                    }),
                ))
                .expect("dispatch configure vm");

            match result.response.payload {
                ResponsePayload::VmConfigured(response) => {
                    assert_eq!(response.applied_mounts, 1);
                }
                other => panic!("expected configured response, got {other:?}"),
            }
        }
        fn guest_mount_request_default_deny_rejects_without_changing_operator_mounts() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let response = sidecar
                .dispatch_blocking(request(
                    3,
                    OwnershipScope::session(&connection_id, &session_id),
                    RequestPayload::CreateVm(CreateVmRequest {
                        runtime: GuestRuntimeKind::JavaScript,
                        metadata: BTreeMap::new(),
                        root_filesystem: Default::default(),
                        permissions: None,
                    }),
                ))
                .expect("create vm");
            let vm_id = created_vm_id(response).expect("vm created");

            sidecar
                .dispatch_blocking(request(
                    4,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::BootstrapRootFilesystem(BootstrapRootFilesystemRequest {
                        entries: vec![RootFilesystemEntry {
                            path: String::from("/guest-mount"),
                            kind: RootFilesystemEntryKind::Directory,
                            ..Default::default()
                        }],
                    }),
                ))
                .expect("bootstrap guest mount directory");

            let configure_response = sidecar
                .dispatch_blocking(request(
                    5,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::ConfigureVm(ConfigureVmRequest {
                        mounts: vec![MountDescriptor {
                            guest_path: String::from("/workspace"),
                            read_only: false,
                            plugin: MountPluginDescriptor {
                                id: String::from("memory"),
                                config: json!({}),
                            },
                        }],
                        software: Vec::new(),
                        permissions: None,
                        module_access_cwd: None,
                        instructions: Vec::new(),
                        projected_modules: Vec::new(),
                        command_permissions: BTreeMap::new(),
                        allowed_node_builtins: Vec::new(),
                        loopback_exempt_ports: Vec::new(),
                    }),
                ))
                .expect("configure operator mount");

            match configure_response.response.payload {
                ResponsePayload::VmConfigured(configured) => {
                    assert_eq!(configured.applied_mounts, 1);
                }
                other => panic!("expected configured response, got {other:?}"),
            }

            let operator_mounts = sidecar
                .vms
                .get(&vm_id)
                .expect("configured vm")
                .kernel
                .mounted_filesystems();
            assert_eq!(operator_mounts.len(), 2, "root + operator-applied mount");

            let mount_error = sidecar
                .vms
                .get_mut(&vm_id)
                .expect("configured vm")
                .kernel
                .mount_filesystem(
                    "/guest-mount",
                    MemoryFileSystem::new(),
                    MountOptions::new("memory"),
                )
                .expect_err("guest mount under default-deny should be rejected");
            assert_eq!(mount_error.code(), "EACCES");

            let mounts_after_guest_request = sidecar
                .vms
                .get(&vm_id)
                .expect("configured vm")
                .kernel
                .mounted_filesystems();
            assert_eq!(mounts_after_guest_request, operator_mounts);
        }
        fn scoped_host_filesystem_unscoped_target_requires_exact_guest_root_prefix() {
            let filesystem = ScopedHostFilesystem::new(
                HostFilesystem::new(SharedBridge::new(RecordingBridge::default()), "vm-1"),
                "/data",
            );

            assert_eq!(
                filesystem.unscoped_target(String::from("/database")),
                "/database"
            );
            assert_eq!(
                filesystem.unscoped_target(String::from("/data/nested.txt")),
                "/nested.txt"
            );
            assert_eq!(filesystem.unscoped_target(String::from("/data")), "/");
        }
        fn scoped_host_filesystem_realpath_preserves_paths_outside_guest_root() {
            let bridge = SharedBridge::new(RecordingBridge::default());
            bridge
                .inspect(|bridge| {
                    agent_os_bridge::FilesystemBridge::symlink(
                        bridge,
                        SymlinkRequest {
                            vm_id: String::from("vm-1"),
                            target_path: String::from("/database"),
                            link_path: String::from("/data/alias"),
                        },
                    )
                    .expect("seed alias symlink");
                })
                .expect("inspect bridge");

            let filesystem =
                ScopedHostFilesystem::new(HostFilesystem::new(bridge, "vm-1"), "/data");

            assert_eq!(
                filesystem.realpath("/alias").expect("resolve alias"),
                "/database"
            );
        }
        fn host_filesystem_realpath_fails_closed_on_circular_symlinks() {
            let bridge = SharedBridge::new(RecordingBridge::default());
            bridge
                .inspect(|bridge| {
                    agent_os_bridge::FilesystemBridge::symlink(
                        bridge,
                        SymlinkRequest {
                            vm_id: String::from("vm-1"),
                            target_path: String::from("/loop-b.txt"),
                            link_path: String::from("/loop-a.txt"),
                        },
                    )
                    .expect("seed loop-a symlink");
                    agent_os_bridge::FilesystemBridge::symlink(
                        bridge,
                        SymlinkRequest {
                            vm_id: String::from("vm-1"),
                            target_path: String::from("/loop-a.txt"),
                            link_path: String::from("/loop-b.txt"),
                        },
                    )
                    .expect("seed loop-b symlink");
                })
                .expect("inspect bridge");

            let filesystem = HostFilesystem::new(bridge, "vm-1");
            let error = filesystem
                .realpath("/loop-a.txt")
                .expect_err("circular symlink chain should fail closed");
            assert_eq!(error.code(), "ELOOP");
        }
        fn configure_vm_host_dir_plugin_fails_closed_for_escape_symlinks() {
            let host_dir = temp_dir("agent-os-sidecar-host-dir-escape");
            std::os::unix::fs::symlink("/etc", host_dir.join("escape"))
                .expect("seed escape symlink");

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");

            sidecar
                .dispatch_blocking(request(
                    4,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::ConfigureVm(ConfigureVmRequest {
                        mounts: vec![MountDescriptor {
                            guest_path: String::from("/workspace"),
                            read_only: false,
                            plugin: MountPluginDescriptor {
                                id: String::from("host_dir"),
                                config: json!({
                                    "hostPath": host_dir,
                                    "readOnly": false,
                                }),
                            },
                        }],
                        software: Vec::new(),
                        permissions: None,
                        module_access_cwd: None,
                        instructions: Vec::new(),
                        projected_modules: Vec::new(),
                        command_permissions: BTreeMap::new(),
                        allowed_node_builtins: Vec::new(),
                        loopback_exempt_ports: Vec::new(),
                    }),
                ))
                .expect("configure host_dir mount");

            let vm = sidecar.vms.get_mut(&vm_id).expect("configured vm");
            let error = vm
                .kernel
                .filesystem_mut()
                .read_file("/workspace/escape/hostname")
                .expect_err("escape symlink should fail closed");
            assert_eq!(error.code(), "EACCES");

            fs::remove_dir_all(host_dir).expect("remove temp dir");
        }
        fn execute_starts_python_runtime_instead_of_rejecting_it() {
            assert_node_available();

            let cache_root = temp_dir("agent-os-sidecar-python-cache");

            acquire_sidecar_runtime_test_lock();
            let mut sidecar = NativeSidecar::with_config(
                RecordingBridge::default(),
                NativeSidecarConfig {
                    sidecar_id: String::from("sidecar-python-test"),
                    compile_cache_root: Some(cache_root),
                    expected_auth_token: Some(String::from(TEST_AUTH_TOKEN)),
                    ..NativeSidecarConfig::default()
                },
            )
            .expect("create sidecar");
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");

            let result = sidecar
                .dispatch_blocking(request(
                    4,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::Execute(crate::protocol::ExecuteRequest {
                        process_id: String::from("proc-python"),
                        command: None,
                        runtime: Some(GuestRuntimeKind::Python),
                        entrypoint: Some(String::from("print('hello from python')")),
                        args: Vec::new(),
                        env: BTreeMap::new(),
                        cwd: None,
                        wasm_permission_tier: None,
                    }),
                ))
                .expect("dispatch python execute");

            match result.response.payload {
                ResponsePayload::ProcessStarted(response) => {
                    assert_eq!(response.process_id, "proc-python");
                    assert!(
                        response.pid.is_some(),
                        "python runtime should expose a child pid"
                    );
                }
                other => panic!("unexpected execute response: {other:?}"),
            }

            let vm = sidecar.vms.get(&vm_id).expect("python vm");
            let process = vm
                .active_processes
                .get("proc-python")
                .expect("python process should be tracked");
            assert_eq!(process.runtime, GuestRuntimeKind::Python);
            match &process.execution {
                ActiveExecution::Python(_) => {}
                other => panic!("unexpected active execution variant: {other:?}"),
            }
        }
        fn command_resolution_executes_wasm_command_from_sidecar_path() {
            let command_root = temp_dir("agent-os-sidecar-command-resolution-wasm");
            write_fixture(
                &command_root.join("hello"),
                wat::parse_str(
                    r#"
(module
  (type $fd_write_t (func (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (type $fd_write_t)))
  (memory (export "memory") 1)
  (data (i32.const 16) "wasm:ready\n")
  (func $_start (export "_start")
    (i32.store (i32.const 0) (i32.const 16))
    (i32.store (i32.const 4) (i32.const 11))
    (drop
      (call $fd_write
        (i32.const 1)
        (i32.const 0)
        (i32.const 1)
        (i32.const 32)
      )
    )
  )
)
"#,
                )
                .expect("compile wasm fixture"),
            );

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");

            sidecar
                .dispatch_blocking(request(
                    4,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::ConfigureVm(ConfigureVmRequest {
                        mounts: vec![MountDescriptor {
                            guest_path: String::from("/__agentos/commands/0"),
                            read_only: true,
                            plugin: MountPluginDescriptor {
                                id: String::from("host_dir"),
                                config: json!({
                                    "hostPath": command_root,
                                    "readOnly": true,
                                }),
                            },
                        }],
                        software: Vec::new(),
                        permissions: None,
                        module_access_cwd: None,
                        instructions: Vec::new(),
                        projected_modules: Vec::new(),
                        command_permissions: BTreeMap::new(),
                        allowed_node_builtins: Vec::new(),
                        loopback_exempt_ports: Vec::new(),
                    }),
                ))
                .expect("configure command mount");

            let response = sidecar
                .dispatch_blocking(request(
                    5,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::Execute(crate::protocol::ExecuteRequest {
                        process_id: String::from("proc-command-wasm"),
                        command: Some(String::from("hello")),
                        runtime: None,
                        entrypoint: None,
                        args: Vec::new(),
                        env: BTreeMap::new(),
                        cwd: None,
                        wasm_permission_tier: None,
                    }),
                ))
                .expect("dispatch wasm command execute");

            match response.response.payload {
                ResponsePayload::ProcessStarted(response) => {
                    assert_eq!(response.process_id, "proc-command-wasm");
                }
                other => panic!("unexpected execute response: {other:?}"),
            }

            let (stdout, stderr, exit_code) =
                drain_process_output(&mut sidecar, &vm_id, "proc-command-wasm");

            assert_eq!(exit_code, Some(0), "stderr: {stderr}");
            assert!(stdout.contains("wasm:ready"), "stdout: {stdout}");
        }
        fn wasm_fd_write_sync_rpc_keeps_stdout_isolated_per_vm() {
            let cwd_a = temp_dir("agent-os-sidecar-wasm-stdio-vm-a");
            let cwd_b = temp_dir("agent-os-sidecar-wasm-stdio-vm-b");
            write_fixture(&cwd_a.join("guest.wasm"), wasm_stdout_module("VM_A_MARKER"));
            write_fixture(&cwd_b.join("guest.wasm"), wasm_stdout_module("VM_B_MARKER"));

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_a = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm A");
            let vm_b = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm B");

            for (request_id, vm_id, process_id, entrypoint) in [
                (6, &vm_a, "proc-wasm-a", cwd_a.join("guest.wasm")),
                (7, &vm_b, "proc-wasm-b", cwd_b.join("guest.wasm")),
            ] {
                let response = sidecar
                    .dispatch_blocking(request(
                        request_id,
                        OwnershipScope::vm(&connection_id, &session_id, vm_id),
                        RequestPayload::Execute(crate::protocol::ExecuteRequest {
                            process_id: String::from(process_id),
                            command: None,
                            runtime: Some(GuestRuntimeKind::WebAssembly),
                            entrypoint: Some(entrypoint.to_string_lossy().into_owned()),
                            args: Vec::new(),
                            env: BTreeMap::new(),
                            cwd: None,
                            wasm_permission_tier: None,
                        }),
                    ))
                    .expect("dispatch wasm execute");

                match response.response.payload {
                    ResponsePayload::ProcessStarted(response) => {
                        assert_eq!(response.process_id, process_id);
                    }
                    other => panic!("unexpected execute response: {other:?}"),
                }
            }

            let (stdout_a, stderr_a, exit_a) =
                drain_process_output(&mut sidecar, &vm_a, "proc-wasm-a");
            let (stdout_b, stderr_b, exit_b) =
                drain_process_output(&mut sidecar, &vm_b, "proc-wasm-b");

            assert_eq!(exit_a, Some(0), "stderr A: {stderr_a}");
            assert_eq!(exit_b, Some(0), "stderr B: {stderr_b}");
            assert!(stderr_a.is_empty(), "unexpected stderr A: {stderr_a}");
            assert!(stderr_b.is_empty(), "unexpected stderr B: {stderr_b}");
            assert!(
                stdout_a.contains("VM_A_MARKER"),
                "stdout A missing marker: {stdout_a:?}"
            );
            assert!(
                !stdout_a.contains("VM_B_MARKER"),
                "stdout A leaked B marker: {stdout_a:?}"
            );
            assert!(
                stdout_b.contains("VM_B_MARKER"),
                "stdout B missing marker: {stdout_b:?}"
            );
            assert!(
                !stdout_b.contains("VM_A_MARKER"),
                "stdout B leaked A marker: {stdout_b:?}"
            );
        }
        fn wasm_fd_write_sync_rpc_routes_stdout_into_kernel_pty() {
            let cwd = temp_dir("agent-os-sidecar-wasm-stdio-pty");
            write_fixture(&cwd.join("guest.wasm"), wasm_stdout_module("PTY_MARKER"));

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");

            let master_fd =
                start_fake_wasm_process(&mut sidecar, &vm_id, &cwd, "proc-wasm-pty", true)
                    .expect("attach stdout pty");

            let mut pty_text = None;
            let mut stderr = String::new();
            let mut exit_code = None;

            for _ in 0..64 {
                let next_event = {
                    let vm = sidecar.vms.get_mut(&vm_id).expect("active vm");
                    vm.active_processes
                        .get_mut("proc-wasm-pty")
                        .map(|process| {
                            if let Some(event) = process.pending_execution_events.pop_front() {
                                Some(event)
                            } else {
                                process
                                    .execution
                                    .poll_event_blocking(Duration::from_secs(5))
                                    .expect("poll wasm pty process event")
                            }
                        })
                        .flatten()
                };
                let Some(event) = next_event else {
                    break;
                };

                if let ActiveExecutionEvent::Stderr(chunk) = &event {
                    stderr.push_str(&String::from_utf8_lossy(chunk));
                }
                if let ActiveExecutionEvent::Exited(code) = &event {
                    exit_code = Some(*code);
                }

                sidecar
                    .handle_execution_event(&vm_id, "proc-wasm-pty", event)
                    .expect("handle wasm pty process event");

                if pty_text.is_none() {
                    let maybe_pty = {
                        let vm = sidecar.vms.get_mut(&vm_id).expect("wasm vm");
                        let kernel_pid = vm
                            .active_processes
                            .get("proc-wasm-pty")
                            .map(|process| process.kernel_pid)
                            .unwrap_or_else(|| {
                                panic!("proc-wasm-pty should stay active until exit is handled")
                            });
                        let ready = vm
                            .kernel
                            .poll_targets(
                                EXECUTION_DRIVER_NAME,
                                kernel_pid,
                                vec![PollTargetEntry::fd(master_fd, POLLIN)],
                                0,
                            )
                            .expect("poll pty master");
                        if ready.ready_count == 0 {
                            None
                        } else {
                            Some(
                                String::from_utf8(
                                    vm.kernel
                                        .fd_read(EXECUTION_DRIVER_NAME, kernel_pid, master_fd, 64)
                                        .expect("read pty master"),
                                )
                                .expect("pty output utf8"),
                            )
                        }
                    };
                    if maybe_pty.is_some() {
                        pty_text = maybe_pty;
                    }
                }

                if exit_code.is_some() && pty_text.is_some() {
                    break;
                }
            }

            let pty_text = pty_text.expect("pty master should receive stdout");
            assert!(
                pty_text.replace("\r\n", "\n").contains("PTY_MARKER\n"),
                "pty output should contain routed marker: {pty_text:?}"
            );
            assert_eq!(exit_code, Some(0), "stderr: {stderr}");
            assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
        }
        fn javascript_child_process_searches_path_for_mounted_wasm_commands() {
            let command_root = temp_dir("agent-os-sidecar-command-path-root");
            for command in ["sh", "ls", "cat", "grep", "echo", "sed"] {
                write_fixture(&command_root.join(command), b"placeholder");
            }

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");

            sidecar
                .dispatch_blocking(request(
                    4,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::ConfigureVm(ConfigureVmRequest {
                        mounts: vec![MountDescriptor {
                            guest_path: String::from("/__agentos/commands/0"),
                            read_only: true,
                            plugin: MountPluginDescriptor {
                                id: String::from("host_dir"),
                                config: json!({
                                    "hostPath": command_root,
                                    "readOnly": true,
                                }),
                            },
                        }],
                        software: Vec::new(),
                        permissions: None,
                        module_access_cwd: None,
                        instructions: Vec::new(),
                        projected_modules: Vec::new(),
                        command_permissions: BTreeMap::new(),
                        allowed_node_builtins: vec![String::from("child_process")],
                        loopback_exempt_ports: Vec::new(),
                    }),
                ))
                .expect("configure command-path mounts");

            let vm = sidecar.vms.get(&vm_id).expect("configured vm");
            let path = vm
                .guest_env
                .get("PATH")
                .expect("configured PATH should exist");
            let path_entries = path.split(':').collect::<Vec<_>>();
            assert!(
                path_entries
                    .first()
                    .is_some_and(|entry| *entry == "/__agentos/commands/0"),
                "PATH should prioritize mounted command root: {path}"
            );
            assert!(
                path_entries
                    .iter()
                    .any(|entry| *entry == "/__agentos/commands/0"),
                "PATH should include mounted command root: {path}"
            );

            for (command, request, expected_process_args) in [
                (
                    "sh",
                    crate::protocol::JavascriptChildProcessSpawnRequest {
                        command: String::from("sh"),
                        args: vec![String::from("-c"), String::from("echo hello")],
                        options: crate::protocol::JavascriptChildProcessSpawnOptions::default(),
                    },
                    vec![
                        String::from("sh"),
                        String::from("-c"),
                        String::from("cd '/home/user' && echo hello"),
                    ],
                ),
                (
                    "ls",
                    crate::protocol::JavascriptChildProcessSpawnRequest {
                        command: String::from("ls"),
                        args: vec![String::from("/")],
                        options: crate::protocol::JavascriptChildProcessSpawnOptions::default(),
                    },
                    vec![String::from("ls"), String::from("/")],
                ),
                (
                    "cat",
                    crate::protocol::JavascriptChildProcessSpawnRequest {
                        command: String::from("cat"),
                        args: vec![String::from("/tmp/file")],
                        options: crate::protocol::JavascriptChildProcessSpawnOptions::default(),
                    },
                    vec![String::from("cat"), String::from("/tmp/file")],
                ),
                (
                    "grep",
                    crate::protocol::JavascriptChildProcessSpawnRequest {
                        command: String::from("grep"),
                        args: vec![String::from("pattern"), String::from("/tmp/file")],
                        options: crate::protocol::JavascriptChildProcessSpawnOptions::default(),
                    },
                    vec![
                        String::from("grep"),
                        String::from("pattern"),
                        String::from("/tmp/file"),
                    ],
                ),
                (
                    "echo",
                    crate::protocol::JavascriptChildProcessSpawnRequest {
                        command: String::from("echo"),
                        args: vec![String::from("hello")],
                        options: crate::protocol::JavascriptChildProcessSpawnOptions::default(),
                    },
                    vec![String::from("echo"), String::from("hello")],
                ),
                (
                    "sed",
                    crate::protocol::JavascriptChildProcessSpawnRequest {
                        command: String::from("sed"),
                        args: vec![String::from("s/a/b/"), String::from("/tmp/file")],
                        options: crate::protocol::JavascriptChildProcessSpawnOptions::default(),
                    },
                    vec![
                        String::from("sed"),
                        String::from("s/a/b/"),
                        String::from("/tmp/file"),
                    ],
                ),
            ] {
                let resolved = sidecar
                    .resolve_javascript_child_process_execution(
                        vm,
                        &vm.guest_env,
                        &vm.guest_cwd,
                        &vm.host_cwd,
                        &request,
                    )
                    .unwrap_or_else(|error| panic!("failed to resolve {command}: {error}"));
                assert_eq!(
                    resolved.runtime,
                    GuestRuntimeKind::WebAssembly,
                    "{command} should resolve as a WASM command"
                );
                assert_eq!(
                    resolved.process_args, expected_process_args,
                    "{command} process args mismatch: {resolved:?}"
                );
                assert!(
                    resolved.entrypoint.ends_with(&format!("/{command}")),
                    "{command} entrypoint should end with /{command}: {}",
                    resolved.entrypoint
                );
            }

            let missing = sidecar.resolve_javascript_child_process_execution(
                vm,
                &vm.guest_env,
                &vm.guest_cwd,
                &vm.host_cwd,
                &crate::protocol::JavascriptChildProcessSpawnRequest {
                    command: String::from("definitely-not-a-command"),
                    args: Vec::new(),
                    options: crate::protocol::JavascriptChildProcessSpawnOptions::default(),
                },
            );
            let error = missing.expect_err("missing command should fail");
            assert!(
                error
                    .to_string()
                    .contains("command not found: definitely-not-a-command"),
                "missing command error should mention the command: {error}"
            );
        }
        fn javascript_child_process_spawns_path_resolved_tool_commands() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");

            sidecar
                .dispatch_blocking(request(
                    5,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::RegisterToolkit(test_toolkit_payload(
                        "math",
                        "Math utilities",
                        "add",
                    )),
                ))
                .expect("register math toolkit");

            let cwd = temp_dir("agent-os-sidecar-tool-command-child-process");
            write_fixture(&cwd.join("entry.mjs"), "setInterval(() => {}, 1000);");
            start_fake_javascript_process(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-tool-child",
                "[\"child_process\"]",
            );

            let spawned = sidecar
                .spawn_javascript_child_process(
                    &vm_id,
                    "proc-js-tool-child",
                    crate::protocol::JavascriptChildProcessSpawnRequest {
                        command: String::from("/usr/local/bin/agentos-math"),
                        args: vec![
                            String::from("add"),
                            String::from("--a"),
                            String::from("2"),
                            String::from("--b"),
                            String::from("3"),
                        ],
                        options: crate::protocol::JavascriptChildProcessSpawnOptions::default(),
                    },
                )
                .expect("spawn toolkit child process");

            assert_eq!(
                spawned["command"],
                Value::String(String::from("agentos-math"))
            );
            assert_eq!(
                spawned["args"],
                json!(["agentos-math", "add", "--a", "2", "--b", "3"])
            );
        }
        fn javascript_child_process_resolves_path_resolved_tool_commands_as_tools() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");

            sidecar
                .dispatch_blocking(request(
                    6,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::RegisterToolkit(test_toolkit_payload(
                        "math",
                        "Math utilities",
                        "add",
                    )),
                ))
                .expect("register math toolkit");

            let vm = sidecar.vms.get(&vm_id).expect("configured vm");
            let resolved = sidecar
                .resolve_javascript_child_process_execution(
                    vm,
                    &vm.guest_env,
                    &vm.guest_cwd,
                    &vm.host_cwd,
                    &crate::protocol::JavascriptChildProcessSpawnRequest {
                        command: String::from("/usr/local/bin/agentos-math"),
                        args: vec![
                            String::from("add"),
                            String::from("--a"),
                            String::from("2"),
                            String::from("--b"),
                            String::from("3"),
                        ],
                        options: crate::protocol::JavascriptChildProcessSpawnOptions::default(),
                    },
                )
                .expect("resolve toolkit child process");

            assert!(
                resolved.tool_command,
                "tool command should stay on the tool path"
            );
            assert_eq!(resolved.command, "agentos-math");
            assert_eq!(
                resolved.process_args,
                vec![
                    String::from("agentos-math"),
                    String::from("add"),
                    String::from("--a"),
                    String::from("2"),
                    String::from("--b"),
                    String::from("3"),
                ]
            );
        }
        fn javascript_child_process_spawns_internal_tool_command_paths() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");

            sidecar
                .dispatch_blocking(request(
                    7,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::RegisterToolkit(test_toolkit_payload(
                        "math",
                        "Math utilities",
                        "add",
                    )),
                ))
                .expect("register math toolkit");

            let cwd = temp_dir("agent-os-sidecar-tool-command-sync-rpc");
            write_fixture(&cwd.join("entry.mjs"), "setInterval(() => {}, 1000);");
            start_fake_javascript_process(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-tool-rpc",
                "[\"child_process\"]",
            );

            let spawned = sidecar
                .spawn_javascript_child_process(
                    &vm_id,
                    "proc-js-tool-rpc",
                    crate::protocol::JavascriptChildProcessSpawnRequest {
                        command: String::from("/__agentos/commands/0/agentos-math"),
                        args: vec![
                            String::from("add"),
                            String::from("--a"),
                            String::from("2"),
                            String::from("--b"),
                            String::from("3"),
                        ],
                        options: crate::protocol::JavascriptChildProcessSpawnOptions::default(),
                    },
                )
                .expect("spawn toolkit child process over internal command path");

            assert_eq!(
                spawned["command"],
                Value::String(String::from("agentos-math"))
            );
            assert_eq!(
                spawned["args"],
                json!(["agentos-math", "add", "--a", "2", "--b", "3"])
            );
        }
        fn javascript_child_process_resolves_internal_tool_command_paths_as_tools() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");

            sidecar
                .dispatch_blocking(request(
                    8,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::RegisterToolkit(test_toolkit_payload(
                        "math",
                        "Math utilities",
                        "add",
                    )),
                ))
                .expect("register math toolkit");

            let vm = sidecar.vms.get(&vm_id).expect("configured vm");
            let resolved = sidecar
                .resolve_javascript_child_process_execution(
                    vm,
                    &vm.guest_env,
                    &vm.guest_cwd,
                    &vm.host_cwd,
                    &crate::protocol::JavascriptChildProcessSpawnRequest {
                        command: String::from("/__agentos/commands/0/agentos-math"),
                        args: vec![
                            String::from("add"),
                            String::from("--a"),
                            String::from("2"),
                            String::from("--b"),
                            String::from("3"),
                        ],
                        options: crate::protocol::JavascriptChildProcessSpawnOptions::default(),
                    },
                )
                .expect("resolve toolkit child process");

            assert!(
                resolved.tool_command,
                "tool command should stay on the tool path"
            );
            assert_eq!(resolved.command, "agentos-math");
            assert_eq!(
                resolved.process_args,
                vec![
                    String::from("agentos-math"),
                    String::from("add"),
                    String::from("--a"),
                    String::from("2"),
                    String::from("--b"),
                    String::from("3"),
                ]
            );
        }
        fn tools_register_toolkit_rejects_duplicate_names_without_replacing_existing_toolkit() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");

            let original_toolkit = test_toolkit_payload("math", "Math utilities", "add");
            sidecar
                .dispatch_blocking(request(
                    9,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::RegisterToolkit(original_toolkit.clone()),
                ))
                .expect("register original toolkit");

            let duplicate_response = sidecar
                .dispatch_blocking(request(
                    10,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::RegisterToolkit(test_toolkit_payload(
                        "math",
                        "Replacement math toolkit",
                        "subtract",
                    )),
                ))
                .expect("dispatch duplicate toolkit registration");

            match duplicate_response.response.payload {
                ResponsePayload::Rejected(rejected) => {
                    assert_eq!(rejected.code, "conflict");
                    assert!(
                        rejected
                            .message
                            .contains("toolkit already registered: math"),
                        "unexpected rejection: {rejected:?}"
                    );
                }
                other => panic!("expected rejected response, got {other:?}"),
            }

            let vm = sidecar.vms.get(&vm_id).expect("configured vm");
            assert_eq!(vm.toolkits.get("math"), Some(&original_toolkit));
        }
        fn tools_javascript_child_process_denies_tool_invocation_without_permission() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy {
                    fs: Some(FsPermissionScope::Mode(PermissionMode::Allow)),
                    network: None,
                    child_process: Some(PatternPermissionScope::Mode(PermissionMode::Allow)),
                    process: None,
                    env: None,
                    tool: Some(PatternPermissionScope::Mode(PermissionMode::Deny)),
                },
            )
            .expect("create vm");

            sidecar
                .dispatch_blocking(request(
                    11,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::RegisterToolkit(test_toolkit_payload(
                        "math",
                        "Math utilities",
                        "add",
                    )),
                ))
                .expect("register math toolkit");

            let cwd = temp_dir("agent-os-sidecar-tool-command-denied");
            insert_fake_javascript_parent_process(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-tool-denied",
            );

            let result = sidecar
                .spawn_javascript_child_process_sync(
                    &vm_id,
                    "proc-js-tool-denied",
                    crate::protocol::JavascriptChildProcessSpawnRequest {
                        command: String::from("/usr/local/bin/agentos-math"),
                        args: vec![String::from("add")],
                        options: crate::protocol::JavascriptChildProcessSpawnOptions::default(),
                    },
                    None,
                )
                .expect("spawn denied tool command");

            assert_eq!(result["code"], json!(1));
            assert_eq!(result["stdout"], json!(""));
            let stderr = result["stderr"]
                .as_str()
                .expect("stderr should be captured as a string");
            assert!(
                stderr.contains("blocked by tool.invoke policy for math:add"),
                "unexpected denied stderr: {stderr:?}"
            );
        }
        fn tools_javascript_child_process_invokes_tool_with_matching_permission() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let permissions = PermissionsPolicy {
                fs: Some(FsPermissionScope::Mode(PermissionMode::Allow)),
                network: None,
                child_process: Some(PatternPermissionScope::Mode(PermissionMode::Allow)),
                process: None,
                env: None,
                tool: Some(PatternPermissionScope::Rules(PatternPermissionRuleSet {
                    default: Some(PermissionMode::Deny),
                    rules: vec![PatternPermissionRule {
                        mode: PermissionMode::Allow,
                        operations: vec![String::from("invoke")],
                        patterns: vec![String::from("math:add")],
                    }],
                })),
            };
            let vm_id = create_vm(&mut sidecar, &connection_id, &session_id, permissions)
                .expect("create vm");

            sidecar
                .dispatch_blocking(request(
                    12,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::RegisterToolkit(test_toolkit_payload(
                        "math",
                        "Math utilities",
                        "add",
                    )),
                ))
                .expect("register math toolkit");

            sidecar.set_sidecar_request_handler(|request| match request.payload {
                SidecarRequestPayload::ToolInvocation(invocation) => {
                    assert_eq!(invocation.tool_key, "math:add");
                    assert_eq!(invocation.input, json!({}));
                    Ok(SidecarResponsePayload::ToolInvocationResult(
                        ToolInvocationResultResponse {
                            invocation_id: invocation.invocation_id,
                            result: Some(json!({ "sum": 5 })),
                            error: None,
                        },
                    ))
                }
                other => panic!("unexpected sidecar request payload: {other:?}"),
            });

            let cwd = temp_dir("agent-os-sidecar-tool-command-allowed");
            insert_fake_javascript_parent_process(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-tool-allowed",
            );

            let result = sidecar
                .spawn_javascript_child_process_sync(
                    &vm_id,
                    "proc-js-tool-allowed",
                    crate::protocol::JavascriptChildProcessSpawnRequest {
                        command: String::from("/usr/local/bin/agentos-math"),
                        args: vec![String::from("add")],
                        options: crate::protocol::JavascriptChildProcessSpawnOptions::default(),
                    },
                    None,
                )
                .expect("spawn allowed tool command");

            assert_eq!(result["code"], json!(0));
            assert_eq!(result["stderr"], json!(""));
            let stdout = result["stdout"]
                .as_str()
                .expect("stdout should be captured as a string");
            let payload: Value =
                serde_json::from_str(stdout).expect("parse successful tool invocation payload");
            assert_eq!(
                payload,
                json!({
                    "ok": true,
                    "result": { "sum": 5 },
                })
            );
        }
        fn tools_javascript_child_process_rejects_invalid_json_file_input_before_dispatch() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let permissions = PermissionsPolicy {
                fs: Some(FsPermissionScope::Mode(PermissionMode::Allow)),
                network: None,
                child_process: Some(PatternPermissionScope::Mode(PermissionMode::Allow)),
                process: None,
                env: None,
                tool: Some(PatternPermissionScope::Rules(PatternPermissionRuleSet {
                    default: Some(PermissionMode::Deny),
                    rules: vec![PatternPermissionRule {
                        mode: PermissionMode::Allow,
                        operations: vec![String::from("invoke")],
                        patterns: vec![String::from("math:add")],
                    }],
                })),
            };
            let vm_id = create_vm(&mut sidecar, &connection_id, &session_id, permissions)
                .expect("create vm");

            sidecar
                .dispatch_blocking(request(
                    13,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::RegisterToolkit(test_toolkit_payload_with_schema(
                        "math",
                        "Math utilities",
                        "add",
                        json!({
                            "type": "object",
                            "properties": {
                                "count": { "type": "integer", "minimum": 0 },
                                "label": { "type": "string" }
                            },
                            "required": ["count", "label"],
                            "additionalProperties": false,
                        }),
                    )),
                ))
                .expect("register math toolkit");

            {
                let vm = sidecar.vms.get_mut(&vm_id).expect("configured vm");
                vm.kernel
                    .write_file(
                        "/workspace/invalid-tool-input.json",
                        br#"{"count":"oops","label":4}"#.to_vec(),
                    )
                    .expect("write invalid tool input");
            }

            let invocation_count = Arc::new(AtomicUsize::new(0));
            let seen_invocation_count = Arc::clone(&invocation_count);
            sidecar.set_sidecar_request_handler(move |request| match request.payload {
                SidecarRequestPayload::ToolInvocation(_) => {
                    seen_invocation_count.fetch_add(1, Ordering::SeqCst);
                    Err(SidecarError::InvalidState(String::from(
                        "tool invocation should not run for invalid JSON-file input",
                    )))
                }
                other => panic!("unexpected sidecar request payload: {other:?}"),
            });

            let cwd = temp_dir("agent-os-sidecar-tool-command-invalid-json-file");
            insert_fake_javascript_parent_process(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-tool-invalid-json-file",
            );

            let result = sidecar
                .spawn_javascript_child_process_sync(
                    &vm_id,
                    "proc-js-tool-invalid-json-file",
                    crate::protocol::JavascriptChildProcessSpawnRequest {
                        command: String::from("/usr/local/bin/agentos-math"),
                        args: vec![
                            String::from("add"),
                            String::from("--json-file"),
                            String::from("/workspace/invalid-tool-input.json"),
                        ],
                        options: crate::protocol::JavascriptChildProcessSpawnOptions::default(),
                    },
                    None,
                )
                .expect("spawn invalid json-file tool command");

            assert_eq!(result["code"], json!(1));
            assert_eq!(result["stdout"], json!(""));
            let stderr = result["stderr"]
                .as_str()
                .expect("stderr should be captured as a string");
            assert!(
                stderr.contains("ToolInputSchemaViolation at $.count"),
                "unexpected schema violation stderr: {stderr:?}"
            );
            assert!(
                stderr.contains("expected integer"),
                "unexpected schema violation stderr: {stderr:?}"
            );
            assert_eq!(invocation_count.load(Ordering::SeqCst), 0);
        }
        fn tools_javascript_child_process_accepts_valid_json_input() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let permissions = PermissionsPolicy {
                fs: Some(FsPermissionScope::Mode(PermissionMode::Allow)),
                network: None,
                child_process: Some(PatternPermissionScope::Mode(PermissionMode::Allow)),
                process: None,
                env: None,
                tool: Some(PatternPermissionScope::Rules(PatternPermissionRuleSet {
                    default: Some(PermissionMode::Deny),
                    rules: vec![PatternPermissionRule {
                        mode: PermissionMode::Allow,
                        operations: vec![String::from("invoke")],
                        patterns: vec![String::from("math:add")],
                    }],
                })),
            };
            let vm_id = create_vm(&mut sidecar, &connection_id, &session_id, permissions)
                .expect("create vm");

            sidecar
                .dispatch_blocking(request(
                    14,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::RegisterToolkit(test_toolkit_payload_with_schema(
                        "math",
                        "Math utilities",
                        "add",
                        json!({
                            "type": "object",
                            "properties": {
                                "count": { "type": "integer", "minimum": 0 },
                                "label": { "type": "string" }
                            },
                            "required": ["count", "label"],
                            "additionalProperties": false,
                        }),
                    )),
                ))
                .expect("register math toolkit");

            let invocation_count = Arc::new(AtomicUsize::new(0));
            let seen_invocation_count = Arc::clone(&invocation_count);
            sidecar.set_sidecar_request_handler(move |request| match request.payload {
                SidecarRequestPayload::ToolInvocation(invocation) => {
                    seen_invocation_count.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(invocation.tool_key, "math:add");
                    assert_eq!(invocation.input, json!({ "count": 2, "label": "ok" }));
                    Ok(SidecarResponsePayload::ToolInvocationResult(
                        ToolInvocationResultResponse {
                            invocation_id: invocation.invocation_id,
                            result: Some(json!({ "sum": 2 })),
                            error: None,
                        },
                    ))
                }
                other => panic!("unexpected sidecar request payload: {other:?}"),
            });

            let cwd = temp_dir("agent-os-sidecar-tool-command-valid-json");
            insert_fake_javascript_parent_process(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-tool-valid-json",
            );

            let result = sidecar
                .spawn_javascript_child_process_sync(
                    &vm_id,
                    "proc-js-tool-valid-json",
                    crate::protocol::JavascriptChildProcessSpawnRequest {
                        command: String::from("/usr/local/bin/agentos-math"),
                        args: vec![
                            String::from("add"),
                            String::from("--json"),
                            String::from(r#"{"count":2,"label":"ok"}"#),
                        ],
                        options: crate::protocol::JavascriptChildProcessSpawnOptions::default(),
                    },
                    None,
                )
                .expect("spawn valid json tool command");

            assert_eq!(result["code"], json!(0));
            assert_eq!(result["stderr"], json!(""));
            let stdout = result["stdout"]
                .as_str()
                .expect("stdout should be captured as a string");
            let payload: Value =
                serde_json::from_str(stdout).expect("parse successful tool invocation payload");
            assert_eq!(
                payload,
                json!({
                    "ok": true,
                    "result": { "sum": 2 },
                })
            );
            assert_eq!(invocation_count.load(Ordering::SeqCst), 1);
        }
        fn command_resolution_executes_javascript_path_command_with_sidecar_mappings() {
            let workspace = temp_dir("agent-os-sidecar-command-resolution-js");
            write_fixture(
                &workspace.join("entry.js"),
                r#"
const { message } = require("./message.js");

process.stdout.write(`${JSON.stringify({
  message,
})}\n`);
"#,
            );
            write_fixture(
                &workspace.join("message.js"),
                r#"module.exports = { message: "resolved-from-mounted-workspace" };"#,
            );

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");

            sidecar
                .dispatch_blocking(request(
                    4,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::ConfigureVm(ConfigureVmRequest {
                        mounts: vec![MountDescriptor {
                            guest_path: String::from("/workspace"),
                            read_only: false,
                            plugin: MountPluginDescriptor {
                                id: String::from("host_dir"),
                                config: json!({
                                    "hostPath": workspace,
                                    "readOnly": false,
                                }),
                            },
                        }],
                        software: Vec::new(),
                        permissions: None,
                        module_access_cwd: None,
                        instructions: Vec::new(),
                        projected_modules: Vec::new(),
                        command_permissions: BTreeMap::new(),
                        allowed_node_builtins: vec![
                            String::from("fs"),
                            String::from("path"),
                            String::from("path"),
                        ],
                        loopback_exempt_ports: vec![4312],
                    }),
                ))
                .expect("configure workspace mount");

            let response = sidecar
                .dispatch_blocking(request(
                    5,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::Execute(crate::protocol::ExecuteRequest {
                        process_id: String::from("proc-command-js"),
                        command: Some(String::from("./entry.js")),
                        runtime: None,
                        entrypoint: None,
                        args: Vec::new(),
                        env: BTreeMap::new(),
                        cwd: Some(String::from("/workspace")),
                        wasm_permission_tier: None,
                    }),
                ))
                .expect("dispatch javascript command execute");

            match response.response.payload {
                ResponsePayload::ProcessStarted(response) => {
                    assert_eq!(response.process_id, "proc-command-js");
                }
                other => panic!("unexpected execute response: {other:?}"),
            }

            let (stdout, stderr, exit_code) =
                drain_process_output(&mut sidecar, &vm_id, "proc-command-js");

            assert_eq!(exit_code, Some(0), "stderr: {stderr}");
            let payload: Value =
                serde_json::from_str(stdout.trim()).expect("parse javascript command JSON");
            assert_eq!(
                payload["message"],
                Value::String(String::from("resolved-from-mounted-workspace"))
            );
        }
        fn command_resolution_executes_node_eval_command() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");

            let response = sidecar
                .dispatch_blocking(request(
                    4,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::Execute(crate::protocol::ExecuteRequest {
                        process_id: String::from("proc-command-node-eval"),
                        command: Some(String::from("node")),
                        runtime: None,
                        entrypoint: None,
                        args: vec![
                            String::from("-e"),
                            String::from("process.stdout.write('node-eval-ok\\n')"),
                        ],
                        env: BTreeMap::new(),
                        cwd: None,
                        wasm_permission_tier: None,
                    }),
                ))
                .expect("dispatch node eval execute");

            match response.response.payload {
                ResponsePayload::ProcessStarted(response) => {
                    assert_eq!(response.process_id, "proc-command-node-eval");
                }
                other => panic!("unexpected execute response: {other:?}"),
            }

            let (stdout, stderr, exit_code) =
                drain_process_output(&mut sidecar, &vm_id, "proc-command-node-eval");

            assert_eq!(exit_code, Some(0), "stderr: {stderr}");
            assert!(stdout.contains("node-eval-ok"), "stdout: {stdout}");
        }
        fn command_resolution_rejects_unknown_command() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");

            let response = sidecar
                .dispatch_blocking(request(
                    4,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::Execute(crate::protocol::ExecuteRequest {
                        process_id: String::from("proc-command-missing"),
                        command: Some(String::from("definitely-not-a-command")),
                        runtime: None,
                        entrypoint: None,
                        args: Vec::new(),
                        env: BTreeMap::new(),
                        cwd: None,
                        wasm_permission_tier: None,
                    }),
                ))
                .expect("dispatch missing command execute");

            match response.response.payload {
                ResponsePayload::Rejected(rejected) => {
                    assert_eq!(rejected.code, "invalid_state");
                    assert!(
                        rejected
                            .message
                            .contains("command not found on native sidecar path"),
                        "unexpected rejection: {rejected:?}"
                    );
                }
                other => panic!("unexpected execute response: {other:?}"),
            }
        }
        fn python_vfs_rpc_requests_proxy_into_the_vm_kernel_filesystem() {
            assert_node_available();

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-python-vfs-rpc-cwd");
            let pyodide_dir = temp_dir("agent-os-sidecar-python-vfs-rpc-pyodide");
            write_fixture(
                &pyodide_dir.join("pyodide.mjs"),
                r#"
export async function loadPyodide() {
  return {
    setStdin(_stdin) {},
    async runPythonAsync(_code) {
      await new Promise(() => {
        setInterval(() => {}, 1_000);
      });
    },
  };
}
"#,
            );
            write_fixture(
                &pyodide_dir.join("pyodide-lock.json"),
                "{\"packages\":[]}\n",
            );
            write_fixture(&pyodide_dir.join("python_stdlib.zip"), "");
            write_fixture(&pyodide_dir.join("pyodide.asm.js"), "");
            write_fixture(&pyodide_dir.join("pyodide.asm.wasm"), "");

            let context = sidecar
                .python_engine
                .create_context(CreatePythonContextRequest {
                    vm_id: vm_id.clone(),
                    pyodide_dist_path: pyodide_dir,
                });
            let execution = sidecar
                .python_engine
                .start_execution(StartPythonExecutionRequest {
                    vm_id: vm_id.clone(),
                    context_id: context.context_id,
                    code: String::from("print('hold-open')"),
                    file_path: None,
                    env: BTreeMap::new(),
                    cwd: cwd.clone(),
                })
                .expect("start fake python execution");

            let kernel_handle = {
                let vm = sidecar.vms.get_mut(&vm_id).expect("python vm");
                vm.kernel
                    .spawn_process(
                        PYTHON_COMMAND,
                        vec![String::from("print('hold-open')")],
                        SpawnOptions {
                            requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                            cwd: Some(String::from("/")),
                            ..SpawnOptions::default()
                        },
                    )
                    .expect("spawn kernel python process")
            };

            {
                let vm = sidecar.vms.get_mut(&vm_id).expect("python vm");
                vm.active_processes.insert(
                    String::from("proc-python-vfs"),
                    ActiveProcess::new(
                        kernel_handle.pid(),
                        kernel_handle,
                        GuestRuntimeKind::Python,
                        ActiveExecution::Python(execution),
                    ),
                );
            }

            for _ in 0..16 {
                let event = {
                    let vm = sidecar.vms.get_mut(&vm_id).expect("python vm");
                    let process = vm
                        .active_processes
                        .get_mut("proc-python-vfs")
                        .expect("python process should be tracked");
                    process
                        .execution
                        .poll_event_blocking(Duration::from_millis(100))
                        .expect("poll python bootstrap event")
                };
                let Some(event) = event else {
                    break;
                };
                if let ActiveExecutionEvent::Exited(code) = &event {
                    panic!("python bootstrap exited unexpectedly with status {code}");
                }
                sidecar
                    .handle_execution_event(&vm_id, "proc-python-vfs", event)
                    .expect("handle python bootstrap event");
            }

            allow_synthetic_python_vfs_reply_drop(
                sidecar.handle_python_vfs_rpc_request(
                    &vm_id,
                    "proc-python-vfs",
                    PythonVfsRpcRequest {
                        id: 1,
                        method: PythonVfsRpcMethod::Mkdir,
                        path: String::from("/workspace"),
                        content_base64: None,
                        recursive: false,
                        url: None,
                        http_method: None,
                        headers: BTreeMap::new(),
                        body_base64: None,
                        hostname: None,
                        family: None,
                        command: None,
                        args: Vec::new(),
                        cwd: None,
                        env: BTreeMap::new(),
                        shell: false,
                        max_buffer: None,
                    },
                ),
                "handle python mkdir rpc",
            );
            allow_synthetic_python_vfs_reply_drop(
                sidecar.handle_python_vfs_rpc_request(
                    &vm_id,
                    "proc-python-vfs",
                    PythonVfsRpcRequest {
                        id: 2,
                        method: PythonVfsRpcMethod::Write,
                        path: String::from("/workspace/note.txt"),
                        content_base64: Some(String::from("aGVsbG8gZnJvbSBzaWRlY2FyIHJwYw==")),
                        recursive: false,
                        url: None,
                        http_method: None,
                        headers: BTreeMap::new(),
                        body_base64: None,
                        hostname: None,
                        family: None,
                        command: None,
                        args: Vec::new(),
                        cwd: None,
                        env: BTreeMap::new(),
                        shell: false,
                        max_buffer: None,
                    },
                ),
                "handle python write rpc",
            );

            let content = {
                let vm = sidecar.vms.get_mut(&vm_id).expect("python vm");
                String::from_utf8(
                    vm.kernel
                        .read_file("/workspace/note.txt")
                        .expect("read bridged file from kernel"),
                )
                .expect("utf8 file contents")
            };
            assert_eq!(content, "hello from sidecar rpc");

            let process = {
                let vm = sidecar.vms.get_mut(&vm_id).expect("python vm");
                vm.active_processes
                    .remove("proc-python-vfs")
                    .expect("remove fake python process")
            };
            cleanup_fake_runtime_process(process);
        }
        fn javascript_sync_rpc_requests_proxy_into_the_vm_kernel_filesystem() {
            assert_node_available();

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-js-sync-rpc-cwd");
            write_fixture(
                &cwd.join("entry.mjs"),
                r#"
import fs from "node:fs";

fs.writeFileSync("/rpc/note.txt", "hello from sidecar rpc");
fs.mkdirSync("/rpc/subdir", { recursive: true });
fs.symlinkSync("/rpc/note.txt", "/rpc/link.txt");
const linkTarget = fs.readlinkSync("/rpc/link.txt");
const existsBefore = fs.existsSync("/rpc/note.txt");
const lstat = fs.lstatSync("/rpc/link.txt");
fs.linkSync("/rpc/note.txt", "/rpc/hard.txt");
fs.renameSync("/rpc/hard.txt", "/rpc/renamed.txt");
const contents = fs.readFileSync("/rpc/renamed.txt", "utf8");
fs.unlinkSync("/rpc/renamed.txt");
fs.rmdirSync("/rpc/subdir");
console.log(JSON.stringify({ existsBefore, linkTarget, linkIsSymlink: lstat.isSymbolicLink(), contents }));
await new Promise(() => {});
"#,
            );

            let context =
                sidecar
                    .javascript_engine
                    .create_context(CreateJavascriptContextRequest {
                        vm_id: vm_id.clone(),
                        bootstrap_module: None,
                        compile_cache_root: None,
                    });
            let execution = sidecar
                .javascript_engine
                .start_execution(StartJavascriptExecutionRequest {
                    vm_id: vm_id.clone(),
                    context_id: context.context_id,
                    argv: vec![String::from("./entry.mjs")],
                    env: BTreeMap::from([(
                        String::from("AGENT_OS_NODE_SYNC_RPC_ENABLE"),
                        String::from("1"),
                    )]),
                    cwd: cwd.clone(),
                    inline_code: None,
                })
                .expect("start fake javascript execution");

            let kernel_handle = {
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                vm.kernel
                    .spawn_process(
                        JAVASCRIPT_COMMAND,
                        vec![String::from("./entry.mjs")],
                        SpawnOptions {
                            requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                            cwd: Some(String::from("/")),
                            ..SpawnOptions::default()
                        },
                    )
                    .expect("spawn kernel javascript process")
            };

            {
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                vm.active_processes.insert(
                    String::from("proc-js-sync"),
                    ActiveProcess::new(
                        kernel_handle.pid(),
                        kernel_handle,
                        GuestRuntimeKind::JavaScript,
                        ActiveExecution::Javascript(execution),
                    )
                    .with_host_cwd(cwd.clone()),
                );
            }

            let mut saw_stdout = false;
            for _ in 0..16 {
                let event = {
                    let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                    let process = vm
                        .active_processes
                        .get_mut("proc-js-sync")
                        .expect("javascript process should be tracked");
                    process
                        .execution
                        .poll_event_blocking(Duration::from_secs(5))
                        .expect("poll javascript sync rpc event")
                        .expect("javascript sync rpc event")
                };

                if let ActiveExecutionEvent::Stdout(chunk) = &event {
                    let stdout = String::from_utf8(chunk.clone()).expect("stdout utf8");
                    if stdout.contains("\"contents\":\"hello from sidecar rpc\"")
                        && stdout.contains("\"existsBefore\":true")
                        && stdout.contains("\"linkTarget\":\"/rpc/note.txt\"")
                        && stdout.contains("\"linkIsSymlink\":true")
                    {
                        saw_stdout = true;
                        break;
                    }
                }

                sidecar
                    .handle_execution_event(&vm_id, "proc-js-sync", event)
                    .expect("handle javascript sync rpc event");
            }

            let content = {
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                String::from_utf8(
                    vm.kernel
                        .read_file("/rpc/note.txt")
                        .expect("read bridged file from kernel"),
                )
                .expect("utf8 file contents")
            };
            assert_eq!(content, "hello from sidecar rpc");
            let link_target = {
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                vm.kernel
                    .read_link("/rpc/link.txt")
                    .expect("read bridged symlink")
            };
            assert_eq!(link_target, "/rpc/note.txt");
            {
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                assert!(
                    !vm.kernel
                        .exists("/rpc/renamed.txt")
                        .expect("renamed file should be gone"),
                    "expected renamed file to be removed",
                );
                assert!(
                    !vm.kernel
                        .exists("/rpc/subdir")
                        .expect("subdir should be gone"),
                    "expected subdir to be removed",
                );
            }
            assert!(saw_stdout, "expected guest stdout after sync fs round-trip");

            let process = {
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                vm.active_processes
                    .remove("proc-js-sync")
                    .expect("remove fake javascript process")
            };
            cleanup_fake_runtime_process(process);
        }
        fn python_vfs_rpc_paths_are_scoped_to_workspace_root() {
            assert_eq!(
                crate::filesystem::normalize_python_vfs_rpc_path("/workspace/./note.txt")
                    .expect("normalize workspace path"),
                String::from("/workspace/note.txt")
            );
            assert!(
                crate::filesystem::normalize_python_vfs_rpc_path("/workspace/../etc/passwd")
                    .is_err(),
                "workspace escape should be rejected",
            );
            assert!(
                crate::filesystem::normalize_python_vfs_rpc_path("/etc/passwd").is_err(),
                "non-workspace paths should be rejected",
            );
            assert!(
                crate::filesystem::normalize_python_vfs_rpc_path("workspace/note.txt").is_err(),
                "relative paths should be rejected",
            );
        }
        fn javascript_fs_sync_rpc_resolves_proc_self_against_the_kernel_process() {
            let mut config = KernelVmConfig::new("vm-js-procfs-rpc");
            config.permissions = Permissions::allow_all();
            let mut kernel = SidecarKernel::new(MountTable::new(MemoryFileSystem::new()), config);
            kernel
                .register_driver(CommandDriver::new(
                    EXECUTION_DRIVER_NAME,
                    [JAVASCRIPT_COMMAND],
                ))
                .expect("register execution driver");

            let kernel_handle = kernel
                .spawn_process(
                    JAVASCRIPT_COMMAND,
                    Vec::new(),
                    SpawnOptions {
                        requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                        ..SpawnOptions::default()
                    },
                )
                .expect("spawn javascript kernel process");
            let kernel_pid = kernel_handle.pid();
            let mut process = ActiveProcess::new(
                kernel_pid,
                kernel_handle,
                GuestRuntimeKind::JavaScript,
                ActiveExecution::Tool(ToolExecution::default()),
            );

            let link = service_javascript_fs_sync_rpc(
                &mut kernel,
                &mut process,
                kernel_pid,
                &JavascriptSyncRpcRequest {
                    id: 1,
                    method: String::from("fs.readlinkSync"),
                    args: vec![json!("/proc/self")],
                },
            )
            .expect("resolve /proc/self");
            assert_eq!(link, Value::String(format!("/proc/{kernel_pid}")));

            let entries = service_javascript_fs_sync_rpc(
                &mut kernel,
                &mut process,
                kernel_pid,
                &JavascriptSyncRpcRequest {
                    id: 2,
                    method: String::from("fs.readdirSync"),
                    args: vec![json!("/proc/self/fd")],
                },
            )
            .expect("read /proc/self/fd");
            let entry_names = entries
                .as_array()
                .expect("readdir should return an array")
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>();
            assert!(entry_names.contains(&"0"));
            assert!(entry_names.contains(&"1"));
            assert!(entry_names.contains(&"2"));

            process.kernel_handle.finish(0);
            kernel.waitpid(kernel_pid).expect("wait javascript process");
        }
        fn javascript_fd_and_stream_rpc_requests_proxy_into_the_vm_kernel_filesystem() {
            assert_node_available();

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            {
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                vm.kernel
                    .write_file("/rpc/input.txt", b"abcdefg")
                    .expect("seed input file");
            }
            let cwd = temp_dir("agent-os-sidecar-js-fd-rpc-cwd");
            write_fixture(
                &cwd.join("entry.mjs"),
                r#"
import fs from "node:fs";
import { once } from "node:events";

const inFd = fs.openSync("/rpc/input.txt", "r");
const buffer = Buffer.alloc(5);
const bytesRead = fs.readSync(inFd, buffer, 0, buffer.length, 1);
const stat = fs.fstatSync(inFd);
fs.closeSync(inFd);

const defaultUmask = process.umask();
const previousUmask = process.umask(0o027);
const outFd = fs.openSync("/rpc/output.txt", "w", 0o666);
const written = fs.writeSync(outFd, Buffer.from("kernel"), 0, 6, 0);
fs.closeSync(outFd);
fs.mkdirSync("/rpc/private", { mode: 0o777 });
const outputStat = fs.statSync("/rpc/output.txt");
const privateDirStat = fs.statSync("/rpc/private");

const asyncSummary = await new Promise((resolve, reject) => {
  fs.open("/rpc/input.txt", "r", (openError, asyncFd) => {
    if (openError) {
      reject(openError);
      return;
    }

    const target = Buffer.alloc(5);
    fs.read(asyncFd, target, 0, 5, 0, (readError, asyncBytesRead) => {
      if (readError) {
        reject(readError);
        return;
      }

      fs.fstat(asyncFd, (statError, asyncStat) => {
        if (statError) {
          reject(statError);
          return;
        }

        fs.close(asyncFd, (closeError) => {
          if (closeError) {
            reject(closeError);
            return;
          }

          resolve({
            asyncBytesRead,
            asyncText: target.toString("utf8"),
            asyncSize: asyncStat.size,
          });
        });
      });
    });
  });
});

const reader = fs.createReadStream("/rpc/input.txt", {
  encoding: "utf8",
  start: 0,
  end: 4,
  highWaterMark: 3,
});
const streamChunks = [];
reader.on("data", (chunk) => streamChunks.push(chunk));
await once(reader, "close");

const writer = fs.createWriteStream("/rpc/stream.txt", { start: 0 });
writer.write("ab");
writer.end("cd");
await once(writer, "close");

let watchCode = "";
let watchFileCode = "";
let watchSupported = false;
let watchFileSupported = false;
try {
  const watcher = fs.watch("/rpc/input.txt");
  watchSupported = typeof watcher.close === "function";
  watcher.close();
} catch (error) {
  watchCode = error.code;
}
try {
  const watchFileListener = () => {};
  fs.watchFile("/rpc/input.txt", watchFileListener);
  watchFileSupported = true;
  fs.unwatchFile("/rpc/input.txt", watchFileListener);
} catch (error) {
  watchFileCode = error.code;
}

console.log(
  JSON.stringify({
    text: buffer.toString("utf8"),
    bytesRead,
    size: stat.size,
    blocks: stat.blocks,
    dev: stat.dev,
    rdev: stat.rdev,
    written,
    defaultUmask,
    previousUmask,
    outputMode: outputStat.mode & 0o777,
    privateDirMode: privateDirStat.mode & 0o777,
    asyncSummary,
    streamChunks,
    watchSupported,
    watchFileSupported,
    watchCode,
    watchFileCode,
  }),
);
"#,
            );

            let context =
                sidecar
                    .javascript_engine
                    .create_context(CreateJavascriptContextRequest {
                        vm_id: vm_id.clone(),
                        bootstrap_module: None,
                        compile_cache_root: None,
                    });
            let execution = sidecar
            .javascript_engine
            .start_execution(StartJavascriptExecutionRequest {
                vm_id: vm_id.clone(),
                context_id: context.context_id,
                argv: vec![String::from("./entry.mjs")],
                env: BTreeMap::from([(
                    String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
                    String::from(
                        "[\"assert\",\"buffer\",\"child_process\",\"console\",\"crypto\",\"events\",\"fs\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
                    ),
                )]),
                cwd: cwd.clone(),
                inline_code: None,
            })
            .expect("start fake javascript execution");

            let kernel_handle = {
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                vm.kernel
                    .spawn_process(
                        JAVASCRIPT_COMMAND,
                        vec![String::from("./entry.mjs")],
                        SpawnOptions {
                            requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                            cwd: Some(String::from("/")),
                            ..SpawnOptions::default()
                        },
                    )
                    .expect("spawn kernel javascript process")
            };

            {
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                vm.active_processes.insert(
                    String::from("proc-js-fd"),
                    ActiveProcess::new(
                        kernel_handle.pid(),
                        kernel_handle,
                        GuestRuntimeKind::JavaScript,
                        ActiveExecution::Javascript(execution),
                    )
                    .with_host_cwd(cwd.clone()),
                );
            }

            let mut stdout = String::new();
            let mut stderr = String::new();
            let mut exit_code = None;
            for _ in 0..64 {
                let next_event = {
                    let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                    vm.active_processes
                        .get_mut("proc-js-fd")
                        .map(|process| {
                            process
                                .execution
                                .poll_event_blocking(Duration::from_secs(5))
                                .expect("poll javascript fd rpc event")
                        })
                        .flatten()
                };
                let Some(event) = next_event else {
                    if exit_code.is_some() {
                        break;
                    }
                    panic!("javascript fd process disappeared before exit");
                };

                match &event {
                    ActiveExecutionEvent::Stdout(chunk) => {
                        stdout.push_str(&String::from_utf8_lossy(chunk));
                    }
                    ActiveExecutionEvent::Stderr(chunk) => {
                        stderr.push_str(&String::from_utf8_lossy(chunk));
                    }
                    ActiveExecutionEvent::Exited(code) => {
                        exit_code = Some(*code);
                    }
                    _ => {}
                }

                sidecar
                    .handle_execution_event(&vm_id, "proc-js-fd", event)
                    .expect("handle javascript fd rpc event");
            }

            assert_eq!(exit_code, Some(0), "stdout: {stdout}\nstderr: {stderr}");
            assert!(stdout.contains("\"text\":\"bcdef\""), "stdout: {stdout}");
            assert!(stdout.contains("\"bytesRead\":5"), "stdout: {stdout}");
            assert!(stdout.contains("\"size\":7"), "stdout: {stdout}");
            assert!(stdout.contains("\"blocks\":1"), "stdout: {stdout}");
            assert!(stdout.contains("\"dev\":1"), "stdout: {stdout}");
            assert!(stdout.contains("\"rdev\":0"), "stdout: {stdout}");
            assert!(stdout.contains("\"written\":6"), "stdout: {stdout}");
            assert!(stdout.contains("\"defaultUmask\":18"), "stdout: {stdout}");
            assert!(stdout.contains("\"previousUmask\":18"), "stdout: {stdout}");
            assert!(stdout.contains("\"outputMode\":416"), "stdout: {stdout}");
            assert!(
                stdout.contains("\"privateDirMode\":488"),
                "stdout: {stdout}"
            );
            assert!(
                stdout.contains("\"asyncText\":\"abcde\""),
                "stdout: {stdout}"
            );
            assert!(stdout.contains("\"asyncSize\":7"), "stdout: {stdout}");
            assert!(
                stdout.contains("\"streamChunks\":[\"abc\",\"de\"]"),
                "stdout: {stdout}"
            );
            assert!(
                stdout.contains("\"watchSupported\":true"),
                "stdout: {stdout}"
            );
            assert!(
                stdout.contains("\"watchFileSupported\":true"),
                "stdout: {stdout}"
            );
            {
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                let output = String::from_utf8(
                    vm.kernel
                        .read_file("/rpc/output.txt")
                        .expect("read fd output file"),
                )
                .expect("utf8 output contents");
                assert_eq!(output, "kernel");

                let stream = String::from_utf8(
                    vm.kernel
                        .read_file("/rpc/stream.txt")
                        .expect("read stream output file"),
                )
                .expect("utf8 stream contents");
                assert_eq!(stream, "abcd");
            }
        }
        fn javascript_fs_promises_batch_requests_before_waiting_on_sidecar_responses() {
            assert_node_available();

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-js-promises-rpc-cwd");
            write_fixture(
                &cwd.join("entry.mjs"),
                r#"
import fs from "node:fs/promises";

await Promise.all(
  Array.from({ length: 10 }, (_, index) =>
    fs.writeFile(`/rpc/write-${index}.txt`, `value-${index}`)
  )
);
console.log("writes-complete");
const contents = await Promise.all(
  Array.from({ length: 10 }, (_, index) =>
    fs.readFile(`/rpc/write-${index}.txt`, "utf8")
  )
);
console.log(JSON.stringify(contents));
await new Promise(() => {});
"#,
            );

            let context =
                sidecar
                    .javascript_engine
                    .create_context(CreateJavascriptContextRequest {
                        vm_id: vm_id.clone(),
                        bootstrap_module: None,
                        compile_cache_root: None,
                    });
            let execution = sidecar
            .javascript_engine
            .start_execution(StartJavascriptExecutionRequest {
                vm_id: vm_id.clone(),
                context_id: context.context_id,
                argv: vec![String::from("./entry.mjs")],
                env: BTreeMap::from([(
                    String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
                    String::from(
                        "[\"assert\",\"buffer\",\"console\",\"child_process\",\"crypto\",\"events\",\"fs\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
                    ),
                )]),
                cwd: cwd.clone(),
                inline_code: None,
            })
            .expect("start fake javascript execution");

            let kernel_handle = {
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                vm.kernel
                    .spawn_process(
                        JAVASCRIPT_COMMAND,
                        vec![String::from("./entry.mjs")],
                        SpawnOptions {
                            requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                            cwd: Some(String::from("/")),
                            ..SpawnOptions::default()
                        },
                    )
                    .expect("spawn kernel javascript process")
            };

            {
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                vm.active_processes.insert(
                    String::from("proc-js-promises"),
                    ActiveProcess::new(
                        kernel_handle.pid(),
                        kernel_handle,
                        GuestRuntimeKind::JavaScript,
                        ActiveExecution::Javascript(execution),
                    ),
                );
            }

            let mut saw_write_batch = false;
            let mut saw_read_batch = false;
            let mut saw_stdout = false;
            let mut pending_requests = Vec::new();

            for _ in 0..40 {
                let event = {
                    let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                    let process = vm
                        .active_processes
                        .get_mut("proc-js-promises")
                        .expect("javascript process should be tracked");
                    process
                        .execution
                        .poll_event_blocking(Duration::from_secs(5))
                        .expect("poll javascript promises event")
                        .expect("javascript promises event")
                };

                match event {
                    ActiveExecutionEvent::JavascriptSyncRpcRequest(request) => {
                        pending_requests.push(request);

                        let expected_method = if !saw_write_batch {
                            "fs.promises.writeFile"
                        } else if !saw_read_batch {
                            "fs.promises.readFile"
                        } else {
                            panic!("received unexpected extra fs.promises request batch");
                        };

                        if pending_requests.len() == 10 {
                            assert!(
                                pending_requests
                                    .iter()
                                    .all(|request| request.method == expected_method),
                                "expected batched {expected_method} requests, got {:?}",
                                pending_requests
                                    .iter()
                                    .map(|request| request.method.as_str())
                                    .collect::<Vec<_>>()
                            );

                            for request in pending_requests.drain(..) {
                                sidecar
                                    .handle_execution_event(
                                        &vm_id,
                                        "proc-js-promises",
                                        ActiveExecutionEvent::JavascriptSyncRpcRequest(request),
                                    )
                                    .expect("handle batched javascript promises rpc event");
                            }

                            if !saw_write_batch {
                                saw_write_batch = true;
                            } else {
                                saw_read_batch = true;
                            }
                        }
                    }
                    ActiveExecutionEvent::Stdout(chunk) => {
                        let stdout = String::from_utf8(chunk).expect("stdout utf8");
                        if stdout.contains(r#"["value-0","value-1","value-2","value-3","value-4","value-5","value-6","value-7","value-8","value-9"]"#) {
                            saw_stdout = true;
                            break;
                        }
                    }
                    other => {
                        let _ = sidecar
                            .handle_execution_event(&vm_id, "proc-js-promises", other)
                            .expect("handle javascript promises side event");
                    }
                }
            }

            let content = {
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                (0..10)
                    .map(|index| {
                        String::from_utf8(
                            vm.kernel
                                .read_file(&format!("/rpc/write-{index}.txt"))
                                .expect("read bridged file from kernel"),
                        )
                        .expect("utf8 file contents")
                    })
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                content,
                (0..10)
                    .map(|index| format!("value-{index}"))
                    .collect::<Vec<_>>()
            );
            assert!(
                saw_write_batch,
                "expected Promise.all(writeFile) to issue a full batch before the first response"
            );
            assert!(
                saw_read_batch,
                "expected Promise.all(readFile) to issue a full batch before the first response"
            );
            assert!(
                saw_stdout,
                "expected guest stdout after concurrent fs.promises round-trip"
            );

            let process = {
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                vm.active_processes
                    .remove("proc-js-promises")
                    .expect("remove fake javascript process")
            };
            cleanup_fake_runtime_process(process);
        }
        fn javascript_crypto_basic_sync_rpcs_round_trip_through_sidecar() {
            fn decode_hex(input: &str) -> Vec<u8> {
                input
                    .as_bytes()
                    .chunks_exact(2)
                    .map(|chunk| {
                        u8::from_str_radix(std::str::from_utf8(chunk).expect("hex utf8"), 16)
                            .expect("hex byte")
                    })
                    .collect()
            }

            fn decode_base64_response(value: Value) -> Vec<u8> {
                base64::engine::general_purpose::STANDARD
                    .decode(value.as_str().expect("crypto response string"))
                    .expect("crypto response base64")
            }

            let mut process = create_crypto_test_process();

            let sha256 = crate::execution::service_javascript_crypto_sync_rpc(
                &mut process,
                &JavascriptSyncRpcRequest {
                    id: 1,
                    method: String::from("crypto.hashDigest"),
                    args: vec![json!("sha256"), json!("YWdlbnQtb3M=")],
                },
            )
            .expect("hashDigest response");
            assert_eq!(
                decode_base64_response(sha256),
                decode_hex("c242c43a13eb523ec02bb1de36d3d467947790e3f005eb7a9cefff357ca54101")
            );

            let sha512 = crate::execution::service_javascript_crypto_sync_rpc(
                &mut process,
                &JavascriptSyncRpcRequest {
                    id: 2,
                    method: String::from("crypto.hashDigest"),
                    args: vec![json!("sha512"), json!("YWdlbnQtb3M=")],
                },
            )
            .expect("hashDigest response");
            assert_eq!(
                decode_base64_response(sha512),
                decode_hex(
                    "9a2983f6cda25d03276e1d2e4bbeff3dee90d4f549a9f4ea4894569998382be6323a7dd86bcef6f83c1b66ab5d9656da1fde2d1682438cdbe58af61fa5de0bb5",
                )
            );

            let sha1 = crate::execution::service_javascript_crypto_sync_rpc(
                &mut process,
                &JavascriptSyncRpcRequest {
                    id: 3,
                    method: String::from("crypto.hashDigest"),
                    args: vec![json!("sha1"), json!("YWdlbnQtb3M=")],
                },
            )
            .expect("hashDigest response");
            assert_eq!(
                decode_base64_response(sha1),
                decode_hex("1d43407501651ea75bc63085f352f99bdcc6e364")
            );

            let md5 = crate::execution::service_javascript_crypto_sync_rpc(
                &mut process,
                &JavascriptSyncRpcRequest {
                    id: 4,
                    method: String::from("crypto.hashDigest"),
                    args: vec![json!("md5"), json!("YWdlbnQtb3M=")],
                },
            )
            .expect("hashDigest response");
            assert_eq!(
                decode_base64_response(md5),
                decode_hex("43e0189b46f53703cf6cb1e6e93ff10d")
            );

            let hmac = crate::execution::service_javascript_crypto_sync_rpc(
                &mut process,
                &JavascriptSyncRpcRequest {
                    id: 5,
                    method: String::from("crypto.hmacDigest"),
                    args: vec![
                        json!("sha256"),
                        json!("YnJpZGdlLWtleQ=="),
                        json!("YWdlbnQtb3M="),
                    ],
                },
            )
            .expect("hmacDigest response");
            assert_eq!(
                decode_base64_response(hmac),
                decode_hex("c24fdd6215522cb3e716855135a1dec9402a3b13be243892c2192d17c57db3a3")
            );

            let pbkdf2 = crate::execution::service_javascript_crypto_sync_rpc(
                &mut process,
                &JavascriptSyncRpcRequest {
                    id: 6,
                    method: String::from("crypto.pbkdf2"),
                    args: vec![
                        json!("aHVudGVyMg=="),
                        json!("YWdlbnQtb3Mtc2FsdA=="),
                        json!(1000),
                        json!(32),
                        json!("sha256"),
                    ],
                },
            )
            .expect("pbkdf2 response");
            assert_eq!(
                decode_base64_response(pbkdf2),
                decode_hex("8e97a9f68ca2ebf44885a7a82d1ec3185cf2d6dcfde51a90278f793f9e57f0e8")
            );

            let scrypt = crate::execution::service_javascript_crypto_sync_rpc(
                &mut process,
                &JavascriptSyncRpcRequest {
                    id: 7,
                    method: String::from("crypto.scrypt"),
                    args: vec![
                        json!("aHVudGVyMg=="),
                        json!("YWdlbnQtb3Mtc2FsdA=="),
                        json!(32),
                        json!(r#"{"cost":16384,"blockSize":8,"parallelization":1}"#),
                    ],
                },
            )
            .expect("scrypt response");
            assert_eq!(
                decode_base64_response(scrypt),
                decode_hex("1d0e6ac5c075c16c94c156480f725eb1c041e531fbb7f61f294f1d4fa50c14d9")
            );
        }
        fn javascript_crypto_advanced_sync_rpcs_round_trip_through_sidecar() {
            fn decode_base64(input: &str) -> Vec<u8> {
                base64::engine::general_purpose::STANDARD
                    .decode(input)
                    .expect("base64 decode")
            }

            fn parse_json_string(value: Value) -> Value {
                serde_json::from_str(value.as_str().expect("json string response"))
                    .expect("parse json string")
            }

            let cipher_response = crate::execution::service_javascript_crypto_sync_rpc(
                &mut create_crypto_test_process(),
                &JavascriptSyncRpcRequest {
                    id: 10,
                    method: String::from("crypto.cipheriv"),
                    args: vec![
                        json!("aes-256-gcm"),
                        json!(base64::engine::general_purpose::STANDARD.encode([7_u8; 32])),
                        json!(base64::engine::general_purpose::STANDARD.encode([3_u8; 12])),
                        json!(base64::engine::general_purpose::STANDARD.encode(b"agent-os")),
                        json!(r#"{"aad":"YWR2YW5jZWQ=","authTagLength":16}"#),
                    ],
                },
            )
            .expect("cipheriv response");
            let cipher_payload = parse_json_string(cipher_response);
            let ciphertext = cipher_payload["data"].as_str().expect("cipher data");
            let auth_tag = cipher_payload["authTag"].as_str().expect("auth tag");

            let decipher_response = crate::execution::service_javascript_crypto_sync_rpc(
                &mut create_crypto_test_process(),
                &JavascriptSyncRpcRequest {
                    id: 11,
                    method: String::from("crypto.decipheriv"),
                    args: vec![
                        json!("aes-256-gcm"),
                        json!(base64::engine::general_purpose::STANDARD.encode([7_u8; 32])),
                        json!(base64::engine::general_purpose::STANDARD.encode([3_u8; 12])),
                        json!(ciphertext),
                        json!(format!(
                            r#"{{"aad":"YWR2YW5jZWQ=","authTag":"{auth_tag}","authTagLength":16}}"#
                        )),
                    ],
                },
            )
            .expect("decipheriv response");
            assert_eq!(
                decode_base64(decipher_response.as_str().expect("decipher response")),
                b"agent-os"
            );

            let mut streaming_process = create_crypto_test_process();
            let session_id = crate::execution::service_javascript_crypto_sync_rpc(
                &mut streaming_process,
                &JavascriptSyncRpcRequest {
                    id: 12,
                    method: String::from("crypto.cipherivCreate"),
                    args: vec![
                        json!("cipher"),
                        json!("aes-256-cbc"),
                        json!(base64::engine::general_purpose::STANDARD.encode([9_u8; 32])),
                        json!(base64::engine::general_purpose::STANDARD.encode([4_u8; 16])),
                        json!(r#"{}"#),
                    ],
                },
            )
            .expect("cipherivCreate")
            .as_u64()
            .expect("session id");
            let update =
                crate::execution::service_javascript_crypto_sync_rpc(
                    &mut streaming_process,
                    &JavascriptSyncRpcRequest {
                        id: 13,
                        method: String::from("crypto.cipherivUpdate"),
                        args: vec![
                            json!(session_id),
                            json!(base64::engine::general_purpose::STANDARD
                                .encode(b"hello world 1234")),
                        ],
                    },
                )
                .expect("cipherivUpdate");
            let final_payload = parse_json_string(
                crate::execution::service_javascript_crypto_sync_rpc(
                    &mut streaming_process,
                    &JavascriptSyncRpcRequest {
                        id: 14,
                        method: String::from("crypto.cipherivFinal"),
                        args: vec![json!(session_id)],
                    },
                )
                .expect("cipherivFinal"),
            );
            assert!(update.as_str().expect("update string").len() > 0);
            assert!(final_payload["data"].as_str().expect("final data").len() > 0);

            let rsa = openssl::rsa::Rsa::generate(2048).expect("generate rsa");
            let private_key = openssl::pkey::PKey::from_rsa(rsa).expect("private pkey from rsa");
            let private_pem = String::from_utf8(
                private_key
                    .private_key_to_pem_pkcs8()
                    .expect("private key to pem"),
            )
            .expect("private pem utf8");
            let public_pem =
                String::from_utf8(private_key.public_key_to_pem().expect("public key to pem"))
                    .expect("public pem utf8");
            let sign_key_json = serde_json::to_string(&public_pem).expect("public pem json");
            let private_key_json = serde_json::to_string(&private_pem).expect("private pem json");

            let signature = crate::execution::service_javascript_crypto_sync_rpc(
                &mut create_crypto_test_process(),
                &JavascriptSyncRpcRequest {
                    id: 15,
                    method: String::from("crypto.sign"),
                    args: vec![
                        json!("sha256"),
                        json!(base64::engine::general_purpose::STANDARD.encode(b"signed")),
                        json!(private_key_json),
                    ],
                },
            )
            .expect("crypto.sign");
            let verified = crate::execution::service_javascript_crypto_sync_rpc(
                &mut create_crypto_test_process(),
                &JavascriptSyncRpcRequest {
                    id: 16,
                    method: String::from("crypto.verify"),
                    args: vec![
                        json!("sha256"),
                        json!(base64::engine::general_purpose::STANDARD.encode(b"signed")),
                        json!(sign_key_json),
                        signature,
                    ],
                },
            )
            .expect("crypto.verify");
            assert_eq!(verified, json!(true));

            let encrypted = crate::execution::service_javascript_crypto_sync_rpc(
                &mut create_crypto_test_process(),
                &JavascriptSyncRpcRequest {
                    id: 17,
                    method: String::from("crypto.asymmetricOp"),
                    args: vec![
                        json!("publicEncrypt"),
                        json!(sign_key_json),
                        json!(base64::engine::general_purpose::STANDARD.encode(b"secret")),
                    ],
                },
            )
            .expect("publicEncrypt");
            let decrypted = crate::execution::service_javascript_crypto_sync_rpc(
                &mut create_crypto_test_process(),
                &JavascriptSyncRpcRequest {
                    id: 18,
                    method: String::from("crypto.asymmetricOp"),
                    args: vec![json!("privateDecrypt"), json!(private_key_json), encrypted],
                },
            )
            .expect("privateDecrypt");
            assert_eq!(
                decode_base64(decrypted.as_str().expect("privateDecrypt string")),
                b"secret"
            );

            let key_object = parse_json_string(
                crate::execution::service_javascript_crypto_sync_rpc(
                    &mut create_crypto_test_process(),
                    &JavascriptSyncRpcRequest {
                        id: 19,
                        method: String::from("crypto.createKeyObject"),
                        args: vec![json!("createPrivateKey"), json!(private_key_json)],
                    },
                )
                .expect("createKeyObject"),
            );
            assert_eq!(key_object["type"], json!("private"));

            let generated_pair = parse_json_string(
                crate::execution::service_javascript_crypto_sync_rpc(
                    &mut create_crypto_test_process(),
                    &JavascriptSyncRpcRequest {
                        id: 20,
                        method: String::from("crypto.generateKeyPairSync"),
                        args: vec![
                            json!("rsa"),
                            json!(r#"{"hasOptions":true,"options":{"modulusLength":1024,"publicExponent":{"__type":"buffer","value":"AQAB"},"publicKeyEncoding":{"format":"pem","type":"spki"},"privateKeyEncoding":{"format":"pem","type":"pkcs8"}}}"#),
                        ],
                    },
                )
                .expect("generateKeyPairSync"),
            );
            assert_eq!(generated_pair["publicKey"]["kind"], json!("string"));
            assert_eq!(generated_pair["privateKey"]["kind"], json!("string"));

            let generated_secret = parse_json_string(
                crate::execution::service_javascript_crypto_sync_rpc(
                    &mut create_crypto_test_process(),
                    &JavascriptSyncRpcRequest {
                        id: 21,
                        method: String::from("crypto.generateKeySync"),
                        args: vec![
                            json!("aes"),
                            json!(r#"{"hasOptions":true,"options":{"length":256}}"#),
                        ],
                    },
                )
                .expect("generateKeySync"),
            );
            assert_eq!(generated_secret["type"], json!("secret"));

            let generated_prime = parse_json_string(
                crate::execution::service_javascript_crypto_sync_rpc(
                    &mut create_crypto_test_process(),
                    &JavascriptSyncRpcRequest {
                        id: 22,
                        method: String::from("crypto.generatePrimeSync"),
                        args: vec![
                            json!(64),
                            json!(r#"{"hasOptions":true,"options":{"bigint":true}}"#),
                        ],
                    },
                )
                .expect("generatePrimeSync"),
            );
            assert_eq!(generated_prime["__type"], json!("bigint"));

            let mut alice = create_crypto_test_process();
            let alice_id = crate::execution::service_javascript_crypto_sync_rpc(
                &mut alice,
                &JavascriptSyncRpcRequest {
                    id: 23,
                    method: String::from("crypto.diffieHellmanSessionCreate"),
                    args: vec![json!(r#"{"type":"ecdh","name":"P-256"}"#)],
                },
            )
            .expect("alice session")
            .as_u64()
            .expect("alice session id");
            let mut bob = create_crypto_test_process();
            let bob_id = crate::execution::service_javascript_crypto_sync_rpc(
                &mut bob,
                &JavascriptSyncRpcRequest {
                    id: 24,
                    method: String::from("crypto.diffieHellmanSessionCreate"),
                    args: vec![json!(r#"{"type":"ecdh","name":"P-256"}"#)],
                },
            )
            .expect("bob session")
            .as_u64()
            .expect("bob session id");
            let alice_public = parse_json_string(
                crate::execution::service_javascript_crypto_sync_rpc(
                    &mut alice,
                    &JavascriptSyncRpcRequest {
                        id: 25,
                        method: String::from("crypto.diffieHellmanSessionCall"),
                        args: vec![json!(alice_id), json!(r#"{"method":"generateKeys"}"#)],
                    },
                )
                .expect("alice generate keys"),
            )["result"]
                .clone();
            let bob_public = parse_json_string(
                crate::execution::service_javascript_crypto_sync_rpc(
                    &mut bob,
                    &JavascriptSyncRpcRequest {
                        id: 26,
                        method: String::from("crypto.diffieHellmanSessionCall"),
                        args: vec![json!(bob_id), json!(r#"{"method":"generateKeys"}"#)],
                    },
                )
                .expect("bob generate keys"),
            )["result"]
                .clone();
            let alice_secret = parse_json_string(
                crate::execution::service_javascript_crypto_sync_rpc(
                    &mut alice,
                    &JavascriptSyncRpcRequest {
                        id: 27,
                        method: String::from("crypto.diffieHellmanSessionCall"),
                        args: vec![
                            json!(alice_id),
                            json!(format!(
                                r#"{{"method":"computeSecret","args":[{}]}}"#,
                                serde_json::to_string(&bob_public).expect("serialize bob public")
                            )),
                        ],
                    },
                )
                .expect("alice compute secret"),
            )["result"]
                .clone();
            let bob_secret = parse_json_string(
                crate::execution::service_javascript_crypto_sync_rpc(
                    &mut bob,
                    &JavascriptSyncRpcRequest {
                        id: 28,
                        method: String::from("crypto.diffieHellmanSessionCall"),
                        args: vec![
                            json!(bob_id),
                            json!(format!(
                                r#"{{"method":"computeSecret","args":[{}]}}"#,
                                serde_json::to_string(&alice_public)
                                    .expect("serialize alice public")
                            )),
                        ],
                    },
                )
                .expect("bob compute secret"),
            )["result"]
                .clone();
            assert_eq!(alice_secret, bob_secret);

            let subtle_digest = parse_json_string(
                crate::execution::service_javascript_crypto_sync_rpc(
                    &mut create_crypto_test_process(),
                    &JavascriptSyncRpcRequest {
                        id: 29,
                        method: String::from("crypto.subtle"),
                        args: vec![json!(
                            r#"{"op":"digest","algorithm":"SHA-256","data":"YWdlbnQtb3M="}"#
                        )],
                    },
                )
                .expect("crypto.subtle digest"),
            );
            assert_eq!(
                decode_base64(subtle_digest["data"].as_str().expect("subtle digest")),
                decode_base64("wkLEOhPrUj7AK7HeNtPUZ5R3kOPwBet6nO//NXylQQE=")
            );
        }
        fn javascript_sqlite_sync_rpcs_round_trip_and_persist_vm_files() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-js-sqlite-rpc-cwd");
            let process_id = "proc-js-sqlite-rpc";

            let kernel_handle = {
                let vm = sidecar.vms.get_mut(&vm_id).expect("sqlite vm");
                vm.kernel
                    .spawn_process(
                        JAVASCRIPT_COMMAND,
                        vec![String::from("./entry.mjs")],
                        SpawnOptions {
                            requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                            cwd: Some(String::from("/")),
                            ..SpawnOptions::default()
                        },
                    )
                    .expect("spawn sqlite kernel process")
            };
            let vm = sidecar.vms.get_mut(&vm_id).expect("sqlite vm");
            vm.active_processes.insert(
                String::from(process_id),
                ActiveProcess::new(
                    kernel_handle.pid(),
                    kernel_handle,
                    GuestRuntimeKind::JavaScript,
                    ActiveExecution::Tool(ToolExecution::default()),
                )
                .with_host_cwd(cwd.clone()),
            );

            let database_id = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                process_id,
                JavascriptSyncRpcRequest {
                    id: 1,
                    method: String::from("sqlite.open"),
                    args: vec![json!("/workspace/app.db"), json!({})],
                },
            )
            .expect("open sqlite database")
            .as_u64()
            .expect("database id");

            let created = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                process_id,
                JavascriptSyncRpcRequest {
                    id: 2,
                    method: String::from("sqlite.exec"),
                    args: vec![
                        json!(database_id),
                        json!("CREATE TABLE items (id INTEGER PRIMARY KEY, payload BLOB NOT NULL)"),
                    ],
                },
            )
            .expect("create sqlite table");
            assert_eq!(created, json!(0));

            let statement_id = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                process_id,
                JavascriptSyncRpcRequest {
                    id: 3,
                    method: String::from("sqlite.prepare"),
                    args: vec![
                        json!(database_id),
                        json!("INSERT INTO items(id, payload) VALUES (?, ?)"),
                    ],
                },
            )
            .expect("prepare sqlite insert")
            .as_u64()
            .expect("statement id");

            let insert = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                process_id,
                JavascriptSyncRpcRequest {
                    id: 4,
                    method: String::from("sqlite.statement.run"),
                    args: vec![
                        json!(statement_id),
                        json!([
                            {
                                "__agentosSqliteType": "bigint",
                                "value": "9007199254740993",
                            },
                            {
                                "__agentosSqliteType": "uint8array",
                                "value": base64::engine::general_purpose::STANDARD.encode([1_u8, 2, 3]),
                            }
                        ]),
                    ],
                },
            )
            .expect("run sqlite insert");
            assert_eq!(insert["changes"], json!(1));

            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                process_id,
                JavascriptSyncRpcRequest {
                    id: 5,
                    method: String::from("sqlite.statement.finalize"),
                    args: vec![json!(statement_id)],
                },
            )
            .expect("finalize sqlite insert");

            let query = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                process_id,
                JavascriptSyncRpcRequest {
                    id: 6,
                    method: String::from("sqlite.query"),
                    args: vec![
                        json!(database_id),
                        json!("SELECT id, payload FROM items"),
                        Value::Null,
                        json!({ "readBigInts": true }),
                    ],
                },
            )
            .expect("query sqlite row");
            assert_eq!(query[0]["id"]["__agentosSqliteType"], json!("bigint"));
            assert_eq!(query[0]["id"]["value"], json!("9007199254740993"));
            assert_eq!(
                query[0]["payload"]["value"],
                json!(base64::engine::general_purpose::STANDARD.encode([1_u8, 2, 3]))
            );

            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                process_id,
                JavascriptSyncRpcRequest {
                    id: 7,
                    method: String::from("sqlite.close"),
                    args: vec![json!(database_id)],
                },
            )
            .expect("close sqlite database");

            let reopened_id = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                process_id,
                JavascriptSyncRpcRequest {
                    id: 8,
                    method: String::from("sqlite.open"),
                    args: vec![json!("/workspace/app.db"), json!({})],
                },
            )
            .expect("reopen sqlite database")
            .as_u64()
            .expect("reopened database id");

            let reopened = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                process_id,
                JavascriptSyncRpcRequest {
                    id: 9,
                    method: String::from("sqlite.query"),
                    args: vec![
                        json!(reopened_id),
                        json!("SELECT id, payload FROM items"),
                        Value::Null,
                        json!({ "readBigInts": true }),
                    ],
                },
            )
            .expect("query reopened sqlite row");
            assert_eq!(reopened, query);
        }
        fn javascript_sqlite_builtin_round_trips_through_sidecar_sync_rpc() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-js-sqlite-builtins-cwd");
            write_fixture(
                &cwd.join("entry.mjs"),
                r#"
import { existsSync, readFileSync, statSync } from "node:fs";
import { DatabaseSync } from "node:sqlite";

const dbPath = "/workspace/sqlite-builtins.db";
const db = new DatabaseSync(dbPath);
if (db.location() !== dbPath) {
  throw new Error(`unexpected sqlite location: ${String(db.location())}`);
}

const journalModeRows = db.query("PRAGMA journal_mode = WAL");
if (journalModeRows[0]?.journal_mode !== "wal") {
  throw new Error(`unexpected journal mode rows: ${JSON.stringify(journalModeRows)}`);
}

db.exec("CREATE TABLE items (id INTEGER PRIMARY KEY, payload BLOB NOT NULL, quantity INTEGER NOT NULL)");
const insert = db.prepare("INSERT INTO items(id, payload, quantity) VALUES (:id, :payload, :quantity)");
insert.setAllowBareNamedParameters(true);
const insertResult = insert.run({
  id: 9007199254740993n,
  payload: new Uint8Array([7, 8, 9]),
  quantity: 42,
});
if (insertResult.changes !== 1) {
  throw new Error(`unexpected insert result: ${JSON.stringify(insertResult)}`);
}
if (typeof insertResult.lastInsertRowid !== "bigint" || insertResult.lastInsertRowid !== 9007199254740993n) {
  throw new Error(`unexpected lastInsertRowid: ${String(insertResult.lastInsertRowid)}`);
}

const select = db.prepare("SELECT id, payload, quantity FROM items WHERE id = ?");
select.setReadBigInts(true);
const row = select.get(9007199254740993n);
if (typeof row.id !== "bigint" || row.id !== 9007199254740993n) {
  throw new Error(`unexpected bigint row id: ${String(row.id)}`);
}
if (!Buffer.isBuffer(row.payload) || row.payload.length !== 3 || row.payload[1] !== 8) {
  throw new Error(`unexpected blob payload: ${JSON.stringify(row.payload)}`);
}
if (row.quantity !== 42n) {
  throw new Error(`unexpected integer payload: id=${String(row.id)} quantity=${String(row.quantity)}`);
}

const columns = select.columns();
if (columns.length !== 3 || columns[0]?.name !== "id" || columns[1]?.name !== "payload") {
  throw new Error(`unexpected statement columns: ${JSON.stringify(columns)}`);
}

db.checkpoint();
if (!existsSync(dbPath)) {
  throw new Error("sqlite database file is not visible in the guest filesystem");
}
const fileStat = statSync(dbPath);
if (fileStat.size <= 0) {
  throw new Error(`unexpected sqlite file size: ${fileStat.size}`);
}
const fileHeader = readFileSync(dbPath).subarray(0, 16).toString("utf8");
if (!fileHeader.startsWith("SQLite format 3")) {
  throw new Error(`unexpected sqlite file header: ${JSON.stringify(fileHeader)}`);
}

db.close();

const reopened = new DatabaseSync(dbPath);
const verify = reopened.prepare("SELECT COUNT(*) AS count, SUM(quantity) AS totalQuantity FROM items");
verify.setReadBigInts(true);
const count = verify.get();
if (count.count !== 1n) {
  throw new Error(`unexpected persisted count: count=${String(count.count)} totalQuantity=${String(count.totalQuantity)}`);
}
if (count.totalQuantity !== 42n) {
  throw new Error(`unexpected persisted quantity total: count=${String(count.count)} totalQuantity=${String(count.totalQuantity)}`);
}
reopened.close();
console.log("sqlite-ok");
"#,
            );

            let (stdout, stderr, exit_code) = run_javascript_entry(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-sqlite-builtins",
                "[\"assert\",\"buffer\",\"console\",\"crypto\",\"events\",\"fs\",\"path\",\"querystring\",\"sqlite\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
            );

            assert_eq!(exit_code, Some(0), "stderr: {stderr}");
            assert!(stderr.trim().is_empty(), "stderr: {stderr}");
            assert_eq!(stdout.trim(), "sqlite-ok");
            let database_bytes = {
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                vm.kernel
                    .read_file("/workspace/sqlite-builtins.db")
                    .expect("read sqlite builtins database file")
            };
            assert!(
                !database_bytes.is_empty(),
                "sqlite builtins database file should be persisted"
            );
        }
        fn javascript_net_rpc_connects_over_vm_loopback() {
            assert_node_available();

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-js-net-rpc-cwd");
            write_fixture(
                &cwd.join("entry.mjs"),
                r#"
import net from "node:net";

const summary = await new Promise((resolve, reject) => {
  const server = net.createServer((socket) => {
    let received = "";
    socket.setEncoding("utf8");
    socket.on("data", (chunk) => {
      received += chunk;
    });
    socket.on("end", () => {
      if (received !== "ping") {
        reject(new Error(`unexpected server payload: ${received}`));
        return;
      }
      socket.end("pong");
    });
    socket.on("error", reject);
  });
  server.on("error", reject);
  server.listen(0, "127.0.0.1", () => {
    const address = server.address();
    if (!address || typeof address === "string") {
      reject(new Error(`unexpected listener address: ${String(address)}`));
      return;
    }
    const socket = net.createConnection({ host: "127.0.0.1", port: address.port });
    let data = "";
    socket.setEncoding("utf8");
    socket.on("connect", () => {
      socket.end("ping");
    });
    socket.on("data", (chunk) => {
      data += chunk;
    });
    socket.on("error", reject);
    socket.on("close", (hadError) => {
      server.close(() => {
        resolve({
          data,
          hadError,
          remoteAddress: socket.remoteAddress,
          remotePort: socket.remotePort,
          localPort: socket.localPort,
          listenerPort: address.port,
        });
      });
    });
  });
});

if (summary.data !== "pong") {
  throw new Error(`unexpected TCP message: ${summary.data}`);
}
if (summary.remoteAddress !== "127.0.0.1") {
  throw new Error(`unexpected TCP remote address: ${JSON.stringify(summary)}`);
}
if (summary.remotePort !== summary.listenerPort) {
  throw new Error(`unexpected TCP remote port: ${JSON.stringify(summary)}`);
}
if (typeof summary.localPort !== "number" || summary.localPort <= 0) {
  throw new Error(`unexpected TCP local port: ${JSON.stringify(summary)}`);
}

console.log(JSON.stringify(summary));
"#,
            );

            let (stdout, stderr, exit_code) = run_javascript_entry(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-net",
                "[\"assert\",\"buffer\",\"console\",\"crypto\",\"events\",\"fs\",\"net\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
            );

            assert_eq!(exit_code, Some(0), "stderr: {stderr}");
            assert!(
                stdout.contains("\"remoteAddress\":\"127.0.0.1\""),
                "stdout: {stdout}"
            );
            assert!(stdout.contains("\"listenerPort\":"), "stdout: {stdout}");
        }
        fn javascript_dgram_rpc_sends_and_receives_vm_loopback_packets() {
            assert_node_available();

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-js-dgram-rpc-cwd");
            write_fixture(
                &cwd.join("entry.mjs"),
                r#"
import dgram from "node:dgram";

const receiver = dgram.createSocket("udp4");
const sender = dgram.createSocket("udp4");
let receiverAddress;
const summary = await new Promise((resolve) => {
  const reject = (error) => {
    console.error(error.stack ?? error.message);
    process.exit(1);
  };
  receiver.on("error", reject);
  sender.on("error", reject);
  receiver.on("message", (message, rinfo) => {
    receiverAddress = receiver.address();
    if (message.toString("utf8") !== "ping") {
      reject(new Error(`unexpected UDP request: ${message.toString("utf8")}`));
      return;
    }
    receiver.send("pong", rinfo.port, rinfo.address, (error) => {
      if (error) {
        reject(error);
      }
    });
  });
  sender.on("message", (message, rinfo) => {
    const senderAddress = sender.address();
    sender.close(() => {
      receiver.close(() => {
        resolve({
          senderAddress,
          receiverAddress,
          message: message.toString("utf8"),
          rinfo,
        });
      });
    });
  });
  receiver.bind(0, "127.0.0.1", () => {
    receiverAddress = receiver.address();
    sender.bind(0, "127.0.0.1", () => {
      sender.send("ping", receiverAddress.port, "127.0.0.1");
    });
  });
});

if (summary.message !== "pong") {
  throw new Error(`unexpected udp message: ${summary.message}`);
}
if (summary.senderAddress.address !== "127.0.0.1") {
  throw new Error(`unexpected udp sender address: ${JSON.stringify(summary.senderAddress)}`);
}
if (summary.receiverAddress.address !== "127.0.0.1") {
  throw new Error(`unexpected udp receiver address: ${JSON.stringify(summary.receiverAddress)}`);
}
if (summary.rinfo.address !== "127.0.0.1" || summary.rinfo.port !== summary.receiverAddress.port) {
  throw new Error(`unexpected udp remote info: ${JSON.stringify(summary.rinfo)}`);
}

console.log(JSON.stringify(summary));
"#,
            );
            let (_stdout, stderr, exit_code) = run_javascript_entry(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-dgram",
                "[\"assert\",\"buffer\",\"console\",\"crypto\",\"dgram\",\"events\",\"fs\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
            );

            assert_eq!(exit_code, Some(0), "stderr: {stderr}");
        }
        fn javascript_dns_rpc_resolves_localhost() {
            assert_node_available();

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-js-dns-rpc-cwd");
            write_fixture(
                &cwd.join("entry.mjs"),
                r#"
import dns from "node:dns";

const lookup = await dns.promises.lookup("localhost", { all: true });
const resolve4 = await dns.promises.resolve4("localhost");

console.log(JSON.stringify({ lookup, resolve4 }));
"#,
            );

            let context =
                sidecar
                    .javascript_engine
                    .create_context(CreateJavascriptContextRequest {
                        vm_id: vm_id.clone(),
                        bootstrap_module: None,
                        compile_cache_root: None,
                    });
            let execution = sidecar
            .javascript_engine
            .start_execution(StartJavascriptExecutionRequest {
                vm_id: vm_id.clone(),
                context_id: context.context_id,
                argv: vec![String::from("./entry.mjs")],
                env: BTreeMap::from([(
                    String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
                    String::from(
                        "[\"assert\",\"buffer\",\"console\",\"crypto\",\"dns\",\"events\",\"fs\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
                    ),
                )]),
                cwd: cwd.clone(),
                inline_code: None,
            })
            .expect("start fake javascript execution");

            let kernel_handle = {
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                vm.kernel
                    .spawn_process(
                        JAVASCRIPT_COMMAND,
                        vec![String::from("./entry.mjs")],
                        SpawnOptions {
                            requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                            cwd: Some(String::from("/")),
                            ..SpawnOptions::default()
                        },
                    )
                    .expect("spawn kernel javascript process")
            };

            {
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                vm.active_processes.insert(
                    String::from("proc-js-dns"),
                    ActiveProcess::new(
                        kernel_handle.pid(),
                        kernel_handle,
                        GuestRuntimeKind::JavaScript,
                        ActiveExecution::Javascript(execution),
                    )
                    .with_host_cwd(cwd.clone()),
                );
            }

            let mut stdout = String::new();
            let mut stderr = String::new();
            let mut exit_code = None;
            for _ in 0..64 {
                let next_event = {
                    let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                    vm.active_processes
                        .get_mut("proc-js-dns")
                        .map(|process| {
                            process
                                .execution
                                .poll_event_blocking(Duration::from_secs(5))
                                .expect("poll javascript dns rpc event")
                        })
                        .flatten()
                };
                let Some(event) = next_event else {
                    if exit_code.is_some() {
                        break;
                    }
                    panic!("javascript dns process disappeared before exit");
                };

                match &event {
                    ActiveExecutionEvent::Stdout(chunk) => {
                        stdout.push_str(&String::from_utf8_lossy(chunk));
                    }
                    ActiveExecutionEvent::Stderr(chunk) => {
                        stderr.push_str(&String::from_utf8_lossy(chunk));
                    }
                    ActiveExecutionEvent::Exited(code) => {
                        exit_code = Some(*code);
                    }
                    _ => {}
                }

                sidecar
                    .handle_execution_event(&vm_id, "proc-js-dns", event)
                    .expect("handle javascript dns rpc event");
            }

            assert_eq!(exit_code, Some(0), "stderr: {stderr}");
            let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse dns JSON");
            assert!(
                parsed["lookup"]
                    .as_array()
                    .is_some_and(|entries| !entries.is_empty()),
                "stdout: {stdout}"
            );
            assert!(
                parsed["resolve4"]
                    .as_array()
                    .is_some_and(|entries| entries.iter().any(|entry| entry == "127.0.0.1")),
                "stdout: {stdout}"
            );
        }
        fn javascript_network_ssrf_protection_blocks_private_dns_and_unowned_loopback_targets() {
            assert_node_available();

            let loopback_listener =
                TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
            let loopback_port = loopback_listener
                .local_addr()
                .expect("loopback listener address")
                .port();

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm_with_metadata(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
                BTreeMap::from([(
                    String::from("network.dns.override.metadata.test"),
                    String::from("169.254.169.254"),
                )]),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-js-ssrf-protection-cwd");
            write_fixture(
                &cwd.join("entry.mjs"),
                &format!(
                    r#"
import dns from "node:dns";
import net from "node:net";

const dnsLookup = await (async () => {{
  try {{
    await dns.promises.lookup("metadata.test", {{ family: 4 }});
    return {{ unexpected: true }};
  }} catch (error) {{
    return {{ code: error.code ?? null, message: error.message }};
  }}
}})();

const privateConnect = await new Promise((resolve) => {{
  try {{
    const socket = net.createConnection({{ host: "metadata.test", port: 80 }});
    socket.on("connect", () => {{
      socket.destroy();
      resolve({{ unexpected: true }});
    }});
    socket.on("error", (error) => {{
      resolve({{ code: error.code ?? null, message: error.message }});
    }});
  }} catch (error) {{
    resolve({{ code: error.code ?? null, message: error.message }});
  }}
}});

const loopbackConnect = await new Promise((resolve) => {{
  try {{
    const socket = net.createConnection({{ host: "127.0.0.1", port: {loopback_port} }});
    socket.on("connect", () => {{
      socket.destroy();
      resolve({{ unexpected: true }});
    }});
    socket.on("error", (error) => {{
      resolve({{ code: error.code ?? null, message: error.message }});
    }});
  }} catch (error) {{
    resolve({{ code: error.code ?? null, message: error.message }});
  }}
}});

console.log(JSON.stringify({{ dnsLookup, privateConnect, loopbackConnect }}));
process.exit(0);
"#,
                ),
            );

            let context =
                sidecar
                    .javascript_engine
                    .create_context(CreateJavascriptContextRequest {
                        vm_id: vm_id.clone(),
                        bootstrap_module: None,
                        compile_cache_root: None,
                    });
            let execution = sidecar
            .javascript_engine
            .start_execution(StartJavascriptExecutionRequest {
                vm_id: vm_id.clone(),
                context_id: context.context_id,
                argv: vec![String::from("./entry.mjs")],
                env: BTreeMap::from([(
                    String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
                    String::from(
                        "[\"assert\",\"buffer\",\"console\",\"crypto\",\"dns\",\"events\",\"fs\",\"net\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
                    ),
                )]),
                cwd: cwd.clone(),
                inline_code: None,
            })
            .expect("start fake javascript execution");

            let kernel_handle = {
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                vm.kernel
                    .spawn_process(
                        JAVASCRIPT_COMMAND,
                        vec![String::from("./entry.mjs")],
                        SpawnOptions {
                            requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                            cwd: Some(String::from("/")),
                            ..SpawnOptions::default()
                        },
                    )
                    .expect("spawn kernel javascript process")
            };

            {
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                vm.active_processes.insert(
                    String::from("proc-js-ssrf-protection"),
                    ActiveProcess::new(
                        kernel_handle.pid(),
                        kernel_handle,
                        GuestRuntimeKind::JavaScript,
                        ActiveExecution::Javascript(execution),
                    )
                    .with_host_cwd(cwd.clone()),
                );
            }

            let mut stdout = String::new();
            let mut stderr = String::new();
            let mut exit_code = None;
            for _ in 0..64 {
                let next_event = {
                    let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                    vm.active_processes
                        .get_mut("proc-js-ssrf-protection")
                        .map(|process| {
                            process
                                .execution
                                .poll_event_blocking(Duration::from_secs(5))
                                .expect("poll javascript ssrf event")
                        })
                        .flatten()
                };
                let Some(event) = next_event else {
                    if exit_code.is_some() {
                        break;
                    }
                    panic!("javascript ssrf process disappeared before exit");
                };

                match &event {
                    ActiveExecutionEvent::Stdout(chunk) => {
                        stdout.push_str(&String::from_utf8_lossy(chunk));
                    }
                    ActiveExecutionEvent::Stderr(chunk) => {
                        stderr.push_str(&String::from_utf8_lossy(chunk));
                    }
                    ActiveExecutionEvent::Exited(code) => {
                        exit_code = Some(*code);
                    }
                    _ => {}
                }

                sidecar
                    .handle_execution_event(&vm_id, "proc-js-ssrf-protection", event)
                    .expect("handle javascript ssrf event");
            }

            assert_eq!(exit_code, Some(0), "stderr: {stderr}");
            let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse ssrf JSON");
            assert_eq!(
                parsed["dnsLookup"]["code"],
                Value::String(String::from("EACCES"))
            );
            assert!(
                parsed["dnsLookup"]["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("169.254.0.0/16")),
                "stdout: {stdout}"
            );
            assert_eq!(
                parsed["privateConnect"]["code"],
                Value::String(String::from("EACCES"))
            );
            assert!(
                parsed["privateConnect"]["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("169.254.0.0/16")),
                "stdout: {stdout}"
            );
            assert_eq!(
                parsed["loopbackConnect"]["code"],
                Value::String(String::from("EACCES"))
            );
            assert!(
                parsed["loopbackConnect"]["message"]
                    .as_str()
                    .is_some_and(|message| message.contains(LOOPBACK_EXEMPT_PORTS_ENV)),
                "stdout: {stdout}"
            );

            drop(loopback_listener);
        }
        fn javascript_dns_rpc_honors_vm_dns_overrides_and_net_connect_uses_sidecar_dns() {
            assert_node_available();

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm_with_metadata(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
                BTreeMap::from([
                    (
                        String::from("network.dns.override.example.test"),
                        String::from("127.0.0.1"),
                    ),
                    (
                        String::from(VM_DNS_SERVERS_METADATA_KEY),
                        String::from("203.0.113.53:5353"),
                    ),
                ]),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-js-dns-override-rpc-cwd");
            write_fixture(
                &cwd.join("entry.mjs"),
                r#"
import dns from "node:dns";
import net from "node:net";

const lookup = await dns.promises.lookup("example.test", { family: 4 });
const resolved = await dns.promises.resolve("example.test", "A");
const socketSummary = await new Promise((resolve, reject) => {
  const server = net.createServer((socket) => {
    let received = "";
    socket.setEncoding("utf8");
    socket.on("data", (chunk) => {
      received += chunk;
    });
    socket.on("end", () => {
      if (received !== "ping") {
        reject(new Error(`unexpected DNS server payload: ${received}`));
        return;
      }
      socket.end("pong");
    });
    socket.on("error", reject);
  });
  server.on("error", reject);
  server.listen(0, "127.0.0.1", () => {
    const address = server.address();
    if (!address || typeof address === "string") {
      reject(new Error(`unexpected DNS listener address: ${String(address)}`));
      return;
    }
    const socket = net.createConnection({ host: "example.test", port: address.port });
    let data = "";
    socket.setEncoding("utf8");
    socket.on("connect", () => {
      socket.end("ping");
    });
    socket.on("data", (chunk) => {
      data += chunk;
    });
    socket.on("error", reject);
    socket.on("close", (hadError) => {
      server.close(() => {
        resolve({
          data,
          hadError,
          remoteAddress: socket.remoteAddress,
          remotePort: socket.remotePort,
          listenerPort: address.port,
        });
      });
    });
  });
});

console.log(JSON.stringify({ lookup, resolved, socketSummary }));
"#,
            );
            let (stdout, stderr, exit_code) = run_javascript_entry(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-dns-override",
                "[\"assert\",\"buffer\",\"console\",\"crypto\",\"dns\",\"events\",\"fs\",\"net\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
            );

            assert_eq!(exit_code, Some(0), "stderr: {stderr}");
            let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse dns JSON");
            assert_eq!(parsed["lookup"]["address"], Value::from("127.0.0.1"));
            assert_eq!(parsed["lookup"]["family"], Value::from(4));
            assert_eq!(parsed["resolved"][0], Value::from("127.0.0.1"));
            assert_eq!(parsed["socketSummary"]["data"], Value::from("pong"));
            assert_eq!(parsed["socketSummary"]["hadError"], Value::from(false));
            assert_eq!(
                parsed["socketSummary"]["remoteAddress"],
                Value::from("127.0.0.1")
            );
            assert_eq!(
                parsed["socketSummary"]["remotePort"],
                parsed["socketSummary"]["listenerPort"]
            );

            let events = sidecar
                .with_bridge_mut(|bridge| bridge.structured_events.clone())
                .expect("collect structured events");
            let dns_events = events
                .iter()
                .filter(|event| event.name == "network.dns.resolved")
                .filter(|event| {
                    event.fields.get("hostname").map(String::as_str) == Some("example.test")
                })
                .collect::<Vec<_>>();
            assert!(
                dns_events.len() >= 3,
                "expected dns events for lookup, resolve, and net.connect: {dns_events:?}"
            );
            for event in dns_events {
                assert_eq!(event.fields["source"], "override");
                assert_eq!(event.fields["addresses"], "127.0.0.1");
                assert_eq!(event.fields["resolver_count"], "1");
                assert_eq!(event.fields["resolvers"], "203.0.113.53:5353");
            }
        }

        fn javascript_network_dns_resolve_supports_standard_rrtypes() {
            assert_node_available();

            let dns_server = FixtureDnsServer::start();
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm_with_metadata(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
                BTreeMap::from([(
                    String::from(VM_DNS_SERVERS_METADATA_KEY),
                    dns_server.addr.to_string(),
                )]),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-js-dns-rrtype-cwd");
            write_fixture(
                &cwd.join("entry.mjs"),
                r#"
import dns from "node:dns";

const resolveMxCallback = await new Promise((resolve, reject) => {
  dns.resolveMx("bundle.example.test", (error, records) => {
    if (error) reject(error);
    else resolve(records);
  });
});

const data = {
  resolve4: await dns.promises.resolve4("bundle.example.test"),
  resolve6: await dns.promises.resolve6("bundle.example.test"),
  resolveMxCallback,
  resolveTxt: await dns.promises.resolveTxt("bundle.example.test"),
  resolveSrv: await dns.promises.resolveSrv("_svc._tcp.example.test"),
  resolveCname: await dns.promises.resolve("alias.example.test", "CNAME"),
  resolvePtr: await dns.promises.resolvePtr("ptr.example.test"),
  resolveNs: await dns.promises.resolveNs("zone.example.test"),
  resolveSoa: await dns.promises.resolveSoa("zone.example.test"),
  resolveNaptr: await dns.promises.resolveNaptr("naptr.example.test"),
  resolveCaa: await dns.promises.resolveCaa("caa.example.test"),
  resolveAny: await dns.promises.resolveAny("bundle.example.test"),
};

try {
  await dns.promises.resolve("bundle.example.test", "TLSA");
  data.unsupported = { unexpected: true };
} catch (error) {
  data.unsupported = { code: error.code ?? null, message: error.message };
}

console.log(JSON.stringify(data));
"#,
            );
            let (stdout, stderr, exit_code) = run_javascript_entry(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-dns-rrtype",
                "[\"assert\",\"buffer\",\"console\",\"crypto\",\"dns\",\"events\",\"fs\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
            );

            assert_eq!(exit_code, Some(0), "stderr: {stderr}");
            let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse dns rrtype JSON");
            assert_eq!(parsed["resolve4"][0], Value::from("203.0.113.10"));
            assert_eq!(parsed["resolve6"][0], Value::from("2001:db8::10"));
            assert_eq!(parsed["resolveMxCallback"][0]["priority"], Value::from(10));
            assert_eq!(
                parsed["resolveMxCallback"][0]["exchange"],
                Value::from("mail.example.test")
            );
            assert_eq!(
                parsed["resolveTxt"][0],
                json!([String::from("v=spf1"), String::from("-all")])
            );
            assert_eq!(parsed["resolveSrv"][0]["port"], Value::from(8443));
            assert_eq!(
                parsed["resolveSrv"][0]["name"],
                Value::from("svc-target.example.test")
            );
            assert_eq!(
                parsed["resolveCname"][0],
                Value::from("bundle.example.test")
            );
            assert_eq!(parsed["resolvePtr"][0], Value::from("host.example.test"));
            assert_eq!(parsed["resolveNs"][0], Value::from("ns1.example.test"));
            assert_eq!(
                parsed["resolveSoa"],
                json!({
                    "nsname": "ns1.example.test",
                    "hostmaster": "hostmaster.example.test",
                    "serial": 2026041601_u32,
                    "refresh": 3600,
                    "retry": 600,
                    "expire": 86400,
                    "minttl": 60_u32
                })
            );
            assert_eq!(
                parsed["resolveNaptr"][0],
                json!({
                    "flags": "s",
                    "service": "SIP+D2U",
                    "regexp": "!^.*$!sip:service@example.test!",
                    "replacement": "_sip._udp.example.test",
                    "order": 10,
                    "preference": 20
                })
            );
            assert_eq!(parsed["resolveCaa"][0]["critical"], Value::from(0));
            assert_eq!(
                parsed["resolveCaa"][0]["issue"],
                Value::from("letsencrypt.org.")
            );
            assert_eq!(
                parsed["resolveCaa"][1]["iodef"],
                Value::from("https://iodef.example.test/report")
            );

            let any_types = parsed["resolveAny"]
                .as_array()
                .expect("resolveAny array")
                .iter()
                .filter_map(|entry| entry.get("type").and_then(Value::as_str))
                .collect::<Vec<_>>();
            assert!(any_types.contains(&"A"), "stdout: {stdout}");
            assert!(any_types.contains(&"AAAA"), "stdout: {stdout}");
            assert!(any_types.contains(&"MX"), "stdout: {stdout}");
            assert!(any_types.contains(&"TXT"), "stdout: {stdout}");
            assert_eq!(
                parsed["unsupported"]["code"],
                Value::from("ERR_NOT_IMPLEMENTED")
            );
            assert!(
                parsed["unsupported"]["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("TLSA")),
                "stdout: {stdout}"
            );
        }

        fn javascript_network_permission_callbacks_fire_for_dns_lookup_connect_and_listen() {
            assert_node_available();

            let listener = TcpListener::bind("127.0.0.1:0").expect("bind tcp listener");
            let port = listener.local_addr().expect("listener address").port();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept tcp client");
                let mut received = Vec::new();
                stream
                    .read_to_end(&mut received)
                    .expect("read client payload");
                assert_eq!(String::from_utf8(received).expect("client utf8"), "ping");
            });

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm_with_metadata(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
                BTreeMap::from([
                    (
                        format!("env.{LOOPBACK_EXEMPT_PORTS_ENV}"),
                        serde_json::to_string(&vec![port.to_string()])
                            .expect("serialize exempt ports"),
                    ),
                    (
                        String::from("network.dns.override.example.test"),
                        String::from("127.0.0.1"),
                    ),
                ]),
            )
            .expect("create vm");
            sidecar
                .bridge
                .clear_vm_permissions(&vm_id)
                .expect("clear static vm permissions");
            let cwd = temp_dir("agent-os-sidecar-js-network-permission-callbacks");
            write_fixture(
                &cwd.join("entry.mjs"),
                &format!(
                    r#"
import dns from "node:dns";
import net from "node:net";

const lookup = await dns.promises.lookup("example.test", {{ family: 4 }});
const listenAddress = await new Promise((resolve, reject) => {{
  const server = net.createServer();
  server.on("error", reject);
  server.listen(0, "127.0.0.1", () => {{
    const address = server.address();
    server.close((error) => {{
      if (error) {{
        reject(error);
        return;
      }}
      resolve(address);
    }});
  }});
}});
const connectResult = await new Promise((resolve, reject) => {{
  const socket = net.createConnection({{ host: "127.0.0.1", port: {port} }});
  socket.on("error", reject);
  socket.on("connect", () => {{
    socket.end("ping");
  }});
  socket.on("close", (hadError) => {{
    resolve({{ hadError }});
  }});
}});

console.log(JSON.stringify({{ lookup, listenAddress, connectResult }}));
process.exit(0);
"#,
                ),
            );

            let (stdout, stderr, exit_code) = run_javascript_entry(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-network-permission-callbacks",
                "[\"assert\",\"buffer\",\"console\",\"crypto\",\"dns\",\"events\",\"fs\",\"net\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
            );

            server.join().expect("join tcp server");
            assert_eq!(exit_code, Some(0), "stderr: {stderr}");
            let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse callback JSON");
            assert_eq!(
                parsed["lookup"]["address"],
                Value::String(String::from("127.0.0.1"))
            );
            assert_eq!(parsed["connectResult"]["hadError"], Value::Bool(false));
            assert!(
                parsed["listenAddress"]["port"]
                    .as_u64()
                    .is_some_and(|value| value > 0),
                "stdout: {stdout}"
            );

            let expected = [
                format!("net:{vm_id}:{}", format_dns_resource("example.test")),
                format!("net:{vm_id}:{}", format_tcp_resource("127.0.0.1", 0)),
                format!("net:{vm_id}:{}", format_tcp_resource("127.0.0.1", port)),
            ];
            let checks = sidecar
                .with_bridge_mut(|bridge| {
                    bridge
                        .permission_checks
                        .iter()
                        .filter(|entry| entry.starts_with("net:"))
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .expect("read permission checks");
            for check in expected {
                assert!(
                    checks.iter().any(|entry| entry == &check),
                    "missing permission check {check:?} in {checks:?}"
                );
            }
        }
        fn javascript_network_permission_denials_surface_eacces_to_guest_code() {
            assert_node_available();

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm_with_metadata(
                &mut sidecar,
                &connection_id,
                &session_id,
                capability_permissions(&[
                    ("fs", PermissionMode::Allow),
                    ("env", PermissionMode::Allow),
                    ("child_process", PermissionMode::Allow),
                    ("network", PermissionMode::Allow),
                    ("network.dns", PermissionMode::Deny),
                    ("network.http", PermissionMode::Deny),
                    ("network.listen", PermissionMode::Deny),
                ]),
                BTreeMap::from([(
                    String::from("network.dns.override.example.test"),
                    String::from("127.0.0.1"),
                )]),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-js-network-permission-denials");
            write_fixture(
                &cwd.join("entry.mjs"),
                r#"
import dns from "node:dns";
import net from "node:net";

let dnsResult = null;
try {
  dnsResult = { unexpected: await dns.promises.lookup("example.test", { family: 4 }) };
} catch (error) {
  dnsResult = { code: error.code ?? null, message: error.message };
}
const listenResult = await new Promise((resolve) => {
  const server = net.createServer();
  server.on("error", (error) => {
    resolve({ code: error.code ?? null, message: error.message });
  });
  try {
    server.listen(0, "127.0.0.1", () => {
      resolve({ unexpected: true });
    });
  } catch (error) {
    resolve({ code: error.code ?? null, message: error.message });
  }
});
const connectResult = await new Promise((resolve) => {
  try {
    const socket = net.createConnection({ host: "127.0.0.1", port: 43111 });
    socket.on("connect", () => resolve({ unexpected: true }));
    socket.on("error", (error) => {
      resolve({ code: error.code ?? null, message: error.message });
    });
  } catch (error) {
    resolve({ code: error.code ?? null, message: error.message });
  }
});

console.log(JSON.stringify({ dnsResult, listenResult, connectResult }));
process.exit(0);
"#,
            );

            let (stdout, stderr, exit_code) = run_javascript_entry(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-network-permission-denials",
                "[\"assert\",\"buffer\",\"console\",\"crypto\",\"dns\",\"events\",\"fs\",\"net\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
            );

            assert_eq!(exit_code, Some(0), "stderr: {stderr}");
            let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse denial JSON");
            for field in ["dnsResult", "listenResult", "connectResult"] {
                assert_eq!(parsed[field]["code"], Value::String(String::from("EACCES")));
                assert!(
                    parsed[field]["message"]
                        .as_str()
                        .is_some_and(|message| message.contains("blocked by network.")),
                    "missing policy detail for {field}: {stdout}"
                );
            }
        }
        fn javascript_tls_rpc_connects_and_serves_over_guest_net() {
            let _tls_lock = tls_service_test_lock();
            assert_node_available();

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-js-tls-rpc-cwd");
            let entry = format!(
                r#"
import tls from "node:tls";

const key = {key:?};
const cert = {cert:?};

const summary = await new Promise((resolve, reject) => {{
  const server = tls.createServer({{ key, cert }}, (socket) => {{
    let received = "";
    socket.setEncoding("utf8");
    socket.on("data", (chunk) => {{
      received += chunk;
      socket.end(`pong:${{chunk}}`);
    }});
    socket.on("error", reject);
    socket.on("close", () => {{
      server.close(() => {{
        resolve({{
          authorized: client.authorized,
          encrypted: client.encrypted,
          hadError: closeState.hadError,
          localPort: client.localPort,
          received,
          remoteAddress: client.remoteAddress,
          response,
          serverPort: port,
          serverSecure: secureConnectionSeen,
        }});
      }});
    }});
  }});
  let response = "";
  let port = null;
  let secureConnectionSeen = false;
  let closeState = {{ hadError: false }};
  let client = null;

  server.on("secureConnection", () => {{
    secureConnectionSeen = true;
  }});
  server.on("error", reject);
  server.listen(0, "127.0.0.1", () => {{
    port = server.address().port;
    client = tls.connect({{
      host: "127.0.0.1",
      port,
      rejectUnauthorized: false,
    }}, () => {{
      client.write("ping");
    }});
    client.setEncoding("utf8");
    client.on("data", (chunk) => {{
      response += chunk;
    }});
    client.on("error", reject);
    client.on("close", (hadError) => {{
      closeState = {{ hadError }};
    }});
  }});
}});

console.log(JSON.stringify(summary));
"#,
                key = TLS_TEST_KEY_PEM,
                cert = TLS_TEST_CERT_PEM,
            );
            write_fixture(&cwd.join("entry.mjs"), &entry);

            let context =
                sidecar
                    .javascript_engine
                    .create_context(CreateJavascriptContextRequest {
                        vm_id: vm_id.clone(),
                        bootstrap_module: None,
                        compile_cache_root: None,
                    });
            let execution = sidecar
            .javascript_engine
            .start_execution(StartJavascriptExecutionRequest {
                vm_id: vm_id.clone(),
                context_id: context.context_id,
                argv: vec![String::from("./entry.mjs")],
                env: BTreeMap::from([(
                    String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
                    String::from(
                        "[\"assert\",\"buffer\",\"console\",\"crypto\",\"events\",\"fs\",\"net\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"tls\",\"url\",\"util\",\"zlib\"]",
                    ),
                )]),
                cwd: cwd.clone(),
                inline_code: None,
            })
            .expect("start fake javascript execution");

            let kernel_handle = {
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                vm.kernel
                    .spawn_process(
                        JAVASCRIPT_COMMAND,
                        vec![String::from("./entry.mjs")],
                        SpawnOptions {
                            requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                            cwd: Some(String::from("/")),
                            ..SpawnOptions::default()
                        },
                    )
                    .expect("spawn kernel javascript process")
            };

            {
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                vm.active_processes.insert(
                    String::from("proc-js-tls"),
                    ActiveProcess::new(
                        kernel_handle.pid(),
                        kernel_handle,
                        GuestRuntimeKind::JavaScript,
                        ActiveExecution::Javascript(execution),
                    )
                    .with_host_cwd(cwd.clone()),
                );
            }

            let mut stdout = String::new();
            let mut stderr = String::new();
            let mut exit_code = None;
            for _ in 0..192 {
                let next_event = {
                    let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                    vm.active_processes
                        .get_mut("proc-js-tls")
                        .map(|process| {
                            process
                                .execution
                                .poll_event_blocking(Duration::from_secs(5))
                                .expect("poll javascript tls rpc event")
                        })
                        .flatten()
                };
                let Some(event) = next_event else {
                    if exit_code.is_some() {
                        break;
                    }
                    continue;
                };

                match &event {
                    ActiveExecutionEvent::Stdout(chunk) => {
                        stdout.push_str(&String::from_utf8_lossy(chunk));
                    }
                    ActiveExecutionEvent::Stderr(chunk) => {
                        stderr.push_str(&String::from_utf8_lossy(chunk));
                    }
                    ActiveExecutionEvent::Exited(code) => {
                        exit_code = Some(*code);
                    }
                    _ => {}
                }

                sidecar
                    .handle_execution_event(&vm_id, "proc-js-tls", event)
                    .expect("handle javascript tls rpc event");
            }

            assert_eq!(exit_code, Some(0), "stderr: {stderr}");
            let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse tls JSON");
            assert_eq!(parsed["response"], Value::String(String::from("pong:ping")));
            assert_eq!(parsed["received"], Value::String(String::from("ping")));
            assert_eq!(parsed["serverSecure"], Value::Bool(true));
            assert_eq!(parsed["encrypted"], Value::Bool(true));
            assert_eq!(parsed["hadError"], Value::Bool(false));
            assert_eq!(
                parsed["remoteAddress"],
                Value::String(String::from("127.0.0.1"))
            );
            assert!(
                parsed["serverPort"].as_u64().is_some_and(|port| port > 0),
                "stdout: {stdout}"
            );
        }
        fn javascript_http_listen_and_close_registers_server() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-http-listen");
            write_fixture(&cwd.join("entry.mjs"), "");
            start_fake_javascript_process(&mut sidecar, &vm_id, &cwd, "proc-js-http-listen", "[]");

            let listen = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-http-listen",
                JavascriptSyncRpcRequest {
                    id: 1,
                    method: String::from("net.http_listen"),
                    args: vec![Value::String(String::from(
                        "{\"serverId\":7,\"hostname\":\"127.0.0.1\",\"port\":0}",
                    ))],
                },
            )
            .expect("listen via http bridge");

            let payload: Value =
                serde_json::from_str(listen.as_str().expect("listen payload string"))
                    .expect("parse listen payload");
            assert_eq!(
                payload["address"]["family"],
                Value::String(String::from("IPv4"))
            );
            assert!(
                payload["address"]["port"]
                    .as_u64()
                    .is_some_and(|port| port > 0),
                "payload: {payload}"
            );
            assert!(
                sidecar
                    .vms
                    .get(&vm_id)
                    .and_then(|vm| vm.active_processes.get("proc-js-http-listen"))
                    .is_some_and(|process| process.http_servers.contains_key(&7)),
                "HTTP server was not registered",
            );

            let close = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-http-listen",
                JavascriptSyncRpcRequest {
                    id: 2,
                    method: String::from("net.http_close"),
                    args: vec![json!(7)],
                },
            )
            .expect("close http bridge server");
            assert_eq!(close, Value::Null);
            assert!(
                sidecar
                    .vms
                    .get(&vm_id)
                    .and_then(|vm| vm.active_processes.get("proc-js-http-listen"))
                    .is_some_and(|process| process.http_servers.is_empty()),
                "HTTP server should be removed after close",
            );
        }
        fn javascript_http_respond_records_pending_response() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-http-respond");
            write_fixture(&cwd.join("entry.mjs"), "");
            start_fake_javascript_process(&mut sidecar, &vm_id, &cwd, "proc-js-http-respond", "[]");

            let response_json = String::from(
                "{\"status\":200,\"headers\":[[\"content-type\",\"text/plain\"]],\"body\":\"cG9uZw==\",\"bodyEncoding\":\"base64\"}",
            );
            {
                let vm = sidecar.vms.get_mut(&vm_id).expect("vm");
                let process = vm
                    .active_processes
                    .get_mut("proc-js-http-respond")
                    .expect("javascript process");
                process.pending_http_requests.insert((7, 9), None);
            }

            let response = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-http-respond",
                JavascriptSyncRpcRequest {
                    id: 4,
                    method: String::from("net.http_respond"),
                    args: vec![json!(7), json!(9), Value::String(response_json.clone())],
                },
            )
            .expect("record http response");
            assert_eq!(response, Value::Null);
            assert_eq!(
                sidecar
                    .vms
                    .get(&vm_id)
                    .and_then(|vm| vm.active_processes.get("proc-js-http-respond"))
                    .and_then(|process| process.pending_http_requests.get(&(7, 9)))
                    .cloned(),
                Some(Some(response_json)),
            );
        }
        fn javascript_http2_listen_connect_request_and_respond_round_trip() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-http2-round-trip");
            write_fixture(&cwd.join("entry.mjs"), "");
            start_fake_javascript_process(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-http2",
                "[\"buffer\",\"stream\"]",
            );

            let listen = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-http2",
                JavascriptSyncRpcRequest {
                    id: 1,
                    method: String::from("net.http2_server_listen"),
                    args: vec![Value::String(String::from(
                        "{\"serverId\":11,\"secure\":false,\"host\":\"127.0.0.1\",\"port\":0,\"backlog\":8,\"settings\":{}}",
                    ))],
                },
            )
            .expect("listen via http2 bridge");
            let listen_payload: Value =
                serde_json::from_str(listen.as_str().expect("listen payload"))
                    .expect("parse http2 listen payload");
            let port = listen_payload["address"]["port"]
                .as_u64()
                .expect("http2 listen port") as u16;

            let connect = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-http2",
                JavascriptSyncRpcRequest {
                    id: 2,
                    method: String::from("net.http2_session_connect"),
                    args: vec![Value::String(format!(
                        "{{\"authority\":\"http://127.0.0.1:{port}\",\"protocol\":\"http:\",\"host\":\"127.0.0.1\",\"port\":{port},\"settings\":{{}}}}"
                    ))],
                },
            )
            .expect("connect via http2 bridge");
            let connect_payload: Value =
                serde_json::from_str(connect.as_str().expect("connect payload"))
                    .expect("parse http2 connect payload");
            let client_session_id = connect_payload["sessionId"]
                .as_u64()
                .expect("client session id");

            let server_session = poll_http2_event(
                &mut sidecar,
                &vm_id,
                "proc-js-http2",
                "net.http2_server_poll",
                11,
                "serverSession",
            );
            let server_session_id = server_session["extraNumber"]
                .as_u64()
                .or_else(|| server_session["id"].as_u64())
                .unwrap_or_default();
            assert!(server_session_id > 0, "event: {server_session}");

            let stream_id = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-http2",
                JavascriptSyncRpcRequest {
                    id: 3,
                    method: String::from("net.http2_session_request"),
                    args: vec![
                        json!(client_session_id),
                        Value::String(String::from("{\":method\":\"GET\",\":path\":\"/ping\"}")),
                        Value::String(String::from("{\"endStream\":true}")),
                    ],
                },
            )
            .expect("issue http2 request")
            .as_u64()
            .expect("client stream id");

            let server_stream = poll_http2_event(
                &mut sidecar,
                &vm_id,
                "proc-js-http2",
                "net.http2_server_poll",
                11,
                "serverStream",
            );
            let server_stream_id = server_stream["data"]
                .as_str()
                .expect("server stream data")
                .parse::<u64>()
                .expect("server stream id");
            assert!(server_stream_id > 0, "event: {server_stream}");
            let _ = poll_http2_event(
                &mut sidecar,
                &vm_id,
                "proc-js-http2",
                "net.http2_server_poll",
                11,
                "serverStreamEnd",
            );

            let respond = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-http2",
                JavascriptSyncRpcRequest {
                    id: 4,
                    method: String::from("net.http2_stream_respond"),
                    args: vec![
                        json!(server_stream_id),
                        Value::String(String::from(
                            "{\":status\":200,\"content-type\":\"text/plain\"}",
                        )),
                    ],
                },
            )
            .expect("respond over http2");
            assert_eq!(respond, Value::Null);

            let wrote = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-http2",
                JavascriptSyncRpcRequest {
                    id: 5,
                    method: String::from("net.http2_stream_write"),
                    args: vec![
                        json!(server_stream_id),
                        json!(base64::engine::general_purpose::STANDARD.encode("pong")),
                    ],
                },
            )
            .expect("write http2 body");
            assert_eq!(wrote, Value::Bool(true));

            let ended = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-http2",
                JavascriptSyncRpcRequest {
                    id: 6,
                    method: String::from("net.http2_stream_end"),
                    args: vec![json!(server_stream_id), Value::Null],
                },
            )
            .expect("end http2 stream");
            assert_eq!(ended, Value::Bool(true));

            let response_headers = poll_http2_event(
                &mut sidecar,
                &vm_id,
                "proc-js-http2",
                "net.http2_session_poll",
                client_session_id,
                "clientResponseHeaders",
            );
            assert_eq!(
                response_headers["id"].as_u64(),
                Some(stream_id),
                "response event: {response_headers}"
            );

            let response_data = poll_http2_event(
                &mut sidecar,
                &vm_id,
                "proc-js-http2",
                "net.http2_session_poll",
                client_session_id,
                "clientData",
            );
            let body = base64::engine::general_purpose::STANDARD
                .decode(response_data["data"].as_str().expect("response body"))
                .expect("decode http2 body");
            assert_eq!(String::from_utf8(body).expect("utf8 body"), "pong");

            let _ = poll_http2_event(
                &mut sidecar,
                &vm_id,
                "proc-js-http2",
                "net.http2_session_poll",
                client_session_id,
                "clientEnd",
            );

            let close = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-http2",
                JavascriptSyncRpcRequest {
                    id: 7,
                    method: String::from("net.http2_session_close"),
                    args: vec![json!(client_session_id)],
                },
            )
            .expect("close http2 client session");
            assert_eq!(close, Value::Null);

            let server_close = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-http2",
                JavascriptSyncRpcRequest {
                    id: 8,
                    method: String::from("net.http2_server_close"),
                    args: vec![json!(11)],
                },
            )
            .expect("close http2 server");
            assert_eq!(server_close, Value::Null);
        }
        fn javascript_http2_settings_pause_push_and_file_response_surfaces_work() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-http2-surfaces");
            write_fixture(&cwd.join("entry.mjs"), "");
            start_fake_javascript_process(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-http2-surfaces",
                "[\"buffer\",\"stream\"]",
            );
            let file_path = cwd.join("reply.txt");
            write_fixture(&file_path, "from-file");

            let listen = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-http2-surfaces",
                JavascriptSyncRpcRequest {
                    id: 10,
                    method: String::from("net.http2_server_listen"),
                    args: vec![Value::String(String::from(
                        "{\"serverId\":22,\"secure\":false,\"host\":\"127.0.0.1\",\"port\":0,\"settings\":{}}",
                    ))],
                },
            )
            .expect("listen via http2 bridge");
            let port = serde_json::from_str::<Value>(listen.as_str().expect("listen payload"))
                .expect("parse listen payload")["address"]["port"]
                .as_u64()
                .expect("port") as u16;

            let connect = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-http2-surfaces",
                JavascriptSyncRpcRequest {
                    id: 11,
                    method: String::from("net.http2_session_connect"),
                    args: vec![Value::String(format!(
                        "{{\"authority\":\"http://127.0.0.1:{port}\",\"protocol\":\"http:\",\"host\":\"127.0.0.1\",\"port\":{port},\"settings\":{{}}}}"
                    ))],
                },
            )
            .expect("connect via http2 bridge");
            let session_id = serde_json::from_str::<Value>(connect.as_str().expect("connect"))
                .expect("parse connect payload")["sessionId"]
                .as_u64()
                .expect("session id");

            let _ = poll_http2_event(
                &mut sidecar,
                &vm_id,
                "proc-js-http2-surfaces",
                "net.http2_server_poll",
                22,
                "serverSession",
            );

            let settings = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-http2-surfaces",
                JavascriptSyncRpcRequest {
                    id: 12,
                    method: String::from("net.http2_session_settings"),
                    args: vec![
                        json!(session_id),
                        Value::String(String::from("{\"initialWindowSize\":1234}")),
                    ],
                },
            )
            .expect("update http2 settings");
            assert_eq!(settings, Value::Null);
            let settings_event = poll_http2_event(
                &mut sidecar,
                &vm_id,
                "proc-js-http2-surfaces",
                "net.http2_session_poll",
                session_id,
                "sessionLocalSettings",
            );
            assert!(
                settings_event["data"]
                    .as_str()
                    .is_some_and(|payload| payload.contains("1234")),
                "settings event: {settings_event}"
            );

            let local_window = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-http2-surfaces",
                JavascriptSyncRpcRequest {
                    id: 13,
                    method: String::from("net.http2_session_set_local_window_size"),
                    args: vec![json!(session_id), json!(4096)],
                },
            )
            .expect("set local window size");
            let local_window_payload: Value =
                serde_json::from_str(local_window.as_str().expect("window payload"))
                    .expect("parse local window payload");
            assert_eq!(
                local_window_payload["state"]["localWindowSize"],
                json!(4096)
            );

            let stream_id = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-http2-surfaces",
                JavascriptSyncRpcRequest {
                    id: 14,
                    method: String::from("net.http2_session_request"),
                    args: vec![
                        json!(session_id),
                        Value::String(String::from("{\":method\":\"GET\",\":path\":\"/file\"}")),
                        Value::String(String::from("{\"endStream\":true}")),
                    ],
                },
            )
            .expect("request file response")
            .as_u64()
            .expect("stream id");
            let server_stream = poll_http2_event(
                &mut sidecar,
                &vm_id,
                "proc-js-http2-surfaces",
                "net.http2_server_poll",
                22,
                "serverStream",
            );
            let server_stream_id = server_stream["data"]
                .as_str()
                .expect("server stream data")
                .parse::<u64>()
                .expect("server stream id");

            let pause = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-http2-surfaces",
                JavascriptSyncRpcRequest {
                    id: 15,
                    method: String::from("net.http2_stream_pause"),
                    args: vec![json!(server_stream_id)],
                },
            )
            .expect("pause http2 stream");
            assert_eq!(pause, Value::Null);
            let resume = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-http2-surfaces",
                JavascriptSyncRpcRequest {
                    id: 16,
                    method: String::from("net.http2_stream_resume"),
                    args: vec![json!(server_stream_id)],
                },
            )
            .expect("resume http2 stream");
            assert_eq!(resume, Value::Null);

            let push_result = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-http2-surfaces",
                JavascriptSyncRpcRequest {
                    id: 17,
                    method: String::from("net.http2_stream_push_stream"),
                    args: vec![
                        json!(server_stream_id),
                        Value::String(String::from("{\":method\":\"GET\",\":path\":\"/pushed\"}")),
                        Value::String(String::from("{}")),
                    ],
                },
            )
            .expect("push http2 stream");
            let push_payload: Value =
                serde_json::from_str(push_result.as_str().expect("push payload"))
                    .expect("parse push payload");
            let pushed_stream_id = push_payload["streamId"].as_u64().expect("pushed stream id");

            let pushed_close = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-http2-surfaces",
                JavascriptSyncRpcRequest {
                    id: 18,
                    method: String::from("net.http2_stream_close"),
                    args: vec![json!(pushed_stream_id), json!(0)],
                },
            )
            .expect("close pushed stream");
            assert_eq!(pushed_close, Value::Null);

            let file_response = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-http2-surfaces",
                JavascriptSyncRpcRequest {
                    id: 19,
                    method: String::from("net.http2_stream_respond_with_file"),
                    args: vec![
                        json!(server_stream_id),
                        Value::String(file_path.to_string_lossy().into_owned()),
                        Value::String(String::from(
                            "{\":status\":200,\"content-type\":\"text/plain\"}",
                        )),
                        Value::String(String::from("{}")),
                    ],
                },
            )
            .expect("respond with file");
            assert_eq!(file_response, Value::Null);

            let response_headers = poll_http2_event(
                &mut sidecar,
                &vm_id,
                "proc-js-http2-surfaces",
                "net.http2_session_poll",
                session_id,
                "clientResponseHeaders",
            );
            assert_eq!(response_headers["id"].as_u64(), Some(stream_id));
            let response_data = poll_http2_event(
                &mut sidecar,
                &vm_id,
                "proc-js-http2-surfaces",
                "net.http2_session_poll",
                session_id,
                "clientData",
            );
            let body = base64::engine::general_purpose::STANDARD
                .decode(response_data["data"].as_str().expect("response body"))
                .expect("decode file body");
            assert_eq!(String::from_utf8(body).expect("utf8 body"), "from-file");
        }
        fn javascript_http2_secure_listen_connect_request_and_respond_round_trip() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-http2-secure-round-trip");
            write_fixture(&cwd.join("entry.mjs"), "");
            start_fake_javascript_process(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-http2-secure",
                "[\"buffer\",\"stream\",\"tls\"]",
            );

            let listen = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-http2-secure",
                JavascriptSyncRpcRequest {
                    id: 20,
                    method: String::from("net.http2_server_listen"),
                    args: vec![Value::String(
                        json!({
                            "serverId": 33,
                            "secure": true,
                            "host": "127.0.0.1",
                            "port": 0,
                            "backlog": 8,
                            "settings": {},
                            "tls": {
                                "isServer": true,
                                "key": { "kind": "string", "data": TLS_TEST_KEY_PEM },
                                "cert": { "kind": "string", "data": TLS_TEST_CERT_PEM },
                                "ALPNProtocols": ["h2"],
                            }
                        })
                        .to_string(),
                    )],
                },
            )
            .expect("listen via secure http2 bridge");
            let listen_payload: Value =
                serde_json::from_str(listen.as_str().expect("listen payload"))
                    .expect("parse http2 listen payload");
            let port = listen_payload["address"]["port"]
                .as_u64()
                .expect("http2 secure listen port") as u16;

            let connect = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-http2-secure",
                JavascriptSyncRpcRequest {
                    id: 21,
                    method: String::from("net.http2_session_connect"),
                    args: vec![Value::String(
                        json!({
                            "authority": format!("https://127.0.0.1:{port}"),
                            "protocol": "https:",
                            "host": "127.0.0.1",
                            "port": port,
                            "settings": {},
                            "tls": {
                                "servername": "localhost",
                                "rejectUnauthorized": false,
                                "ALPNProtocols": ["h2"],
                            }
                        })
                        .to_string(),
                    )],
                },
            )
            .expect("connect via secure http2 bridge");
            let connect_payload: Value =
                serde_json::from_str(connect.as_str().expect("connect payload"))
                    .expect("parse secure http2 connect payload");
            let client_session_id = connect_payload["sessionId"]
                .as_u64()
                .expect("client session id");

            let server_session = poll_http2_event(
                &mut sidecar,
                &vm_id,
                "proc-js-http2-secure",
                "net.http2_server_poll",
                33,
                "serverSession",
            );
            let server_session_id = server_session["extraNumber"]
                .as_u64()
                .or_else(|| server_session["id"].as_u64())
                .unwrap_or_default();
            assert!(server_session_id > 0, "event: {server_session}");

            let stream_id = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-http2-secure",
                JavascriptSyncRpcRequest {
                    id: 22,
                    method: String::from("net.http2_session_request"),
                    args: vec![
                        json!(client_session_id),
                        Value::String(String::from("{\":method\":\"GET\",\":path\":\"/secure\"}")),
                        Value::String(String::from("{\"endStream\":true}")),
                    ],
                },
            )
            .expect("issue secure http2 request")
            .as_u64()
            .expect("client stream id");

            let server_stream = poll_http2_event(
                &mut sidecar,
                &vm_id,
                "proc-js-http2-secure",
                "net.http2_server_poll",
                33,
                "serverStream",
            );
            let server_stream_id = server_stream["data"]
                .as_str()
                .expect("server stream data")
                .parse::<u64>()
                .expect("server stream id");

            let respond = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-http2-secure",
                JavascriptSyncRpcRequest {
                    id: 23,
                    method: String::from("net.http2_stream_respond"),
                    args: vec![
                        json!(server_stream_id),
                        Value::String(String::from(
                            "{\":status\":200,\"content-type\":\"text/plain\"}",
                        )),
                    ],
                },
            )
            .expect("respond over secure http2");
            assert_eq!(respond, Value::Null);

            let ended = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-http2-secure",
                JavascriptSyncRpcRequest {
                    id: 24,
                    method: String::from("net.http2_stream_end"),
                    args: vec![
                        json!(server_stream_id),
                        json!(base64::engine::general_purpose::STANDARD.encode("secure-pong")),
                    ],
                },
            )
            .expect("end secure http2 stream");
            assert_eq!(ended, Value::Bool(true));

            let response_headers = poll_http2_event(
                &mut sidecar,
                &vm_id,
                "proc-js-http2-secure",
                "net.http2_session_poll",
                client_session_id,
                "clientResponseHeaders",
            );
            assert_eq!(response_headers["id"].as_u64(), Some(stream_id));

            let response_data = poll_http2_event(
                &mut sidecar,
                &vm_id,
                "proc-js-http2-secure",
                "net.http2_session_poll",
                client_session_id,
                "clientData",
            );
            let body = base64::engine::general_purpose::STANDARD
                .decode(response_data["data"].as_str().expect("response body"))
                .expect("decode secure http2 body");
            assert_eq!(
                String::from_utf8(body).expect("utf8 secure http2 body"),
                "secure-pong"
            );

            let session_state: Value = serde_json::from_str(
                connect_payload["state"]
                    .as_str()
                    .expect("session state payload"),
            )
            .expect("parse secure session state");
            assert_eq!(session_state["encrypted"], json!(true));
            assert_eq!(session_state["socket"]["encrypted"], json!(true));
        }
        fn javascript_http2_server_respond_records_pending_response() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-http2-respond");
            write_fixture(&cwd.join("entry.mjs"), "");
            start_fake_javascript_process(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-http2-respond",
                "[]",
            );

            let response_json = String::from(
                "{\"status\":200,\"headers\":[[\"content-type\",\"text/plain\"]],\"body\":\"c2VjdXJlLXBvbmc=\",\"bodyEncoding\":\"base64\"}",
            );
            {
                let vm = sidecar.vms.get_mut(&vm_id).expect("vm");
                let process = vm
                    .active_processes
                    .get_mut("proc-js-http2-respond")
                    .expect("javascript process");
                process.pending_http_requests.insert((33, 44), None);
            }

            let response = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-http2-respond",
                JavascriptSyncRpcRequest {
                    id: 25,
                    method: String::from("net.http2_server_respond"),
                    args: vec![json!(33), json!(44), Value::String(response_json.clone())],
                },
            )
            .expect("record http2 response");
            assert_eq!(response, Value::Bool(true));
            assert_eq!(
                sidecar
                    .vms
                    .get(&vm_id)
                    .and_then(|vm| vm.active_processes.get("proc-js-http2-respond"))
                    .and_then(|process| process.pending_http_requests.get(&(33, 44)))
                    .cloned(),
                Some(Some(response_json)),
            );
        }
        fn javascript_http_rpc_requests_gets_and_serves_over_guest_net() {
            assert_node_available();

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-js-http-rpc-cwd");
            write_fixture(
                &cwd.join("entry.mjs"),
                r#"
import http from "node:http";

const summary = await new Promise((resolve, reject) => {
  const requests = [];
  let requestResponse = "";
  let getResponse = "";

  const server = http.createServer((req, res) => {
    let body = "";
    req.setEncoding("utf8");
    req.on("data", (chunk) => {
      body += chunk;
    });
    req.on("end", () => {
      requests.push({
        method: req.method,
        url: req.url,
        body,
      });
      res.end(`pong:${req.method}:${body || req.url}`);
    });
  });

  let port = null;
  server.on("error", reject);
  server.listen(0, "127.0.0.1", () => {
    port = server.address().port;
    const req = http.request(
      {
        host: "127.0.0.1",
        method: "POST",
        path: "/submit",
        port,
      },
      (res) => {
        res.setEncoding("utf8");
        res.on("data", (chunk) => {
          requestResponse += chunk;
        });
        res.on("end", () => {
          http
            .get(`http://127.0.0.1:${port}/health`, (getRes) => {
              getRes.setEncoding("utf8");
              getRes.on("data", (chunk) => {
                getResponse += chunk;
              });
              getRes.on("end", () => {
                server.close(() => {
                  resolve({
                    getResponse,
                    port,
                    requestResponse,
                    requests,
                  });
                });
              });
            })
            .on("error", reject);
        });
      },
    );
    req.on("error", reject);
    req.end("ping");
  });
});

console.log(JSON.stringify(summary));
"#,
            );

            let (stdout, stderr, exit_code) = run_javascript_entry(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-http",
                "[\"assert\",\"buffer\",\"console\",\"crypto\",\"events\",\"fs\",\"http\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
            );

            assert_eq!(exit_code, Some(0), "stderr: {stderr}");
            let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse http JSON");
            assert_eq!(
                parsed["requestResponse"],
                Value::String(String::from("pong:POST:ping"))
            );
            assert_eq!(
                parsed["getResponse"],
                Value::String(String::from("pong:GET:/health"))
            );
            assert_eq!(
                parsed["requests"][0]["url"],
                Value::String(String::from("/submit"))
            );
            assert_eq!(
                parsed["requests"][1]["url"],
                Value::String(String::from("/health"))
            );
            assert!(
                parsed["port"].as_u64().is_some_and(|port| port > 0),
                "stdout: {stdout}"
            );
        }
        fn javascript_https_rpc_requests_and_serves_over_guest_tls() {
            assert_node_available();

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-js-https-rpc-cwd");
            let entry = format!(
                r#"
import https from "node:https";

const key = {key:?};
const cert = {cert:?};

const summary = await new Promise((resolve, reject) => {{
  let received = "";
  let response = "";
  const server = https.createServer({{ key, cert }}, (req, res) => {{
    req.setEncoding("utf8");
    req.on("data", (chunk) => {{
      received += chunk;
    }});
    req.on("end", () => {{
      res.end(`pong:${{req.method}}:${{received}}`);
    }});
  }});

  let port = null;
  server.on("error", reject);
  server.listen(0, "127.0.0.1", () => {{
    port = server.address().port;
    const req = https.request({{
      host: "127.0.0.1",
      method: "POST",
      path: "/secure",
      port,
      rejectUnauthorized: false,
    }}, (res) => {{
      res.setEncoding("utf8");
      res.on("data", (chunk) => {{
        response += chunk;
      }});
      res.on("end", () => {{
        server.close(() => {{
          resolve({{
            port,
            received,
            response,
          }});
        }});
      }});
    }});
    req.on("error", reject);
    req.end("ping");
  }});
}});

console.log(JSON.stringify(summary));
"#,
                key = TLS_TEST_KEY_PEM,
                cert = TLS_TEST_CERT_PEM,
            );
            write_fixture(&cwd.join("entry.mjs"), &entry);

            let (stdout, stderr, exit_code) = run_javascript_entry(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-https",
                "[\"assert\",\"buffer\",\"console\",\"crypto\",\"events\",\"fs\",\"https\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
            );

            assert_eq!(exit_code, Some(0), "stderr: {stderr}");
            let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse https JSON");
            assert_eq!(parsed["received"], Value::String(String::from("ping")));
            assert_eq!(
                parsed["response"],
                Value::String(String::from("pong:POST:ping"))
            );
            assert!(
                parsed["port"].as_u64().is_some_and(|port| port > 0),
                "stdout: {stdout}"
            );
        }
        fn javascript_net_rpc_listens_accepts_connections_and_reports_listener_state() {
            assert_node_available();

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-js-net-server-cwd");
            write_fixture(&cwd.join("entry.mjs"), "setInterval(() => {}, 1000);");
            start_fake_javascript_process(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-server",
                "[\"net\"]",
            );

            let listen = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-server",
                JavascriptSyncRpcRequest {
                    id: 1,
                    method: String::from("net.listen"),
                    args: vec![json!({
                        "host": "127.0.0.1",
                        "port": 0,
                        "backlog": 2,
                    })],
                },
            )
            .expect("listen through sidecar net RPC");
            let server_id = listen["serverId"].as_str().expect("server id").to_string();
            let guest_port = listen["localPort"]
                .as_u64()
                .and_then(|value| u16::try_from(value).ok())
                .expect("guest listener port");
            let response = sidecar
                .dispatch_blocking(request(
                    1,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::FindListener(FindListenerRequest {
                        host: Some(String::from("127.0.0.1")),
                        port: Some(guest_port),
                        path: None,
                    }),
                ))
                .expect("query sidecar listener");
            match response.response.payload {
                ResponsePayload::ListenerSnapshot(snapshot) => {
                    let listener = snapshot.listener.expect("listener snapshot");
                    assert_eq!(listener.process_id, "proc-js-server");
                    assert_eq!(listener.host.as_deref(), Some("127.0.0.1"));
                    assert_eq!(listener.port, Some(guest_port));
                }
                other => panic!("unexpected find_listener response payload: {other:?}"),
            }

            let client = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-server",
                JavascriptSyncRpcRequest {
                    id: 2,
                    method: String::from("net.connect"),
                    args: vec![json!({
                        "host": "127.0.0.1",
                        "port": guest_port,
                    })],
                },
            )
            .expect("connect guest tcp client");
            let client_socket_id = client["socketId"]
                .as_str()
                .expect("client socket id")
                .to_string();

            let accepted = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-server",
                JavascriptSyncRpcRequest {
                    id: 3,
                    method: String::from("net.server_poll"),
                    args: vec![json!(server_id), json!(250)],
                },
            )
            .expect("accept connection");
            assert_eq!(accepted["type"], Value::from("connection"));
            assert_eq!(accepted["localAddress"], Value::from("127.0.0.1"));
            assert_eq!(accepted["localPort"], Value::from(guest_port));
            let socket_id = accepted["socketId"]
                .as_str()
                .expect("socket id")
                .to_string();

            let written = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-server",
                JavascriptSyncRpcRequest {
                    id: 4,
                    method: String::from("net.write"),
                    args: vec![
                        json!(client_socket_id.clone()),
                        json!({
                            "__agentOsType": "bytes",
                            "base64": base64::engine::general_purpose::STANDARD.encode("ping"),
                        }),
                    ],
                },
            )
            .expect("write client payload");
            assert_eq!(written, Value::from(4));

            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-server",
                JavascriptSyncRpcRequest {
                    id: 5,
                    method: String::from("net.shutdown"),
                    args: vec![json!(client_socket_id.clone())],
                },
            )
            .expect("shutdown client write half");

            let data = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-server",
                JavascriptSyncRpcRequest {
                    id: 6,
                    method: String::from("net.poll"),
                    args: vec![json!(socket_id.clone()), json!(250)],
                },
            )
            .expect("poll socket data");
            assert_eq!(data["type"], Value::from("data"));

            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data["data"]["base64"].as_str().expect("base64 payload"))
                .expect("decode payload");
            assert_eq!(bytes, b"ping");

            let written = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-server",
                JavascriptSyncRpcRequest {
                    id: 7,
                    method: String::from("net.write"),
                    args: vec![
                        json!(socket_id.clone()),
                        json!({
                            "__agentOsType": "bytes",
                            "base64": base64::engine::general_purpose::STANDARD.encode("pong:ping"),
                        }),
                    ],
                },
            )
            .expect("write response");
            assert_eq!(written, Value::from(9));

            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-server",
                JavascriptSyncRpcRequest {
                    id: 8,
                    method: String::from("net.shutdown"),
                    args: vec![json!(socket_id)],
                },
            )
            .expect("shutdown write half");

            let client_data = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-server",
                JavascriptSyncRpcRequest {
                    id: 9,
                    method: String::from("net.poll"),
                    args: vec![json!(client_socket_id.clone()), json!(250)],
                },
            )
            .expect("poll client response");
            assert_eq!(client_data["type"], Value::from("data"));
            let client_bytes = base64::engine::general_purpose::STANDARD
                .decode(
                    client_data["data"]["base64"]
                        .as_str()
                        .expect("client base64 payload"),
                )
                .expect("decode client payload");
            assert_eq!(client_bytes, b"pong:ping");

            let client_end = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-server",
                JavascriptSyncRpcRequest {
                    id: 10,
                    method: String::from("net.poll"),
                    args: vec![json!(client_socket_id), json!(250)],
                },
            )
            .expect("poll client end");
            assert_eq!(client_end["type"], Value::from("end"));
        }
        fn javascript_net_rpc_reports_connection_counts_and_enforces_backlog() {
            assert_node_available();

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-js-net-backlog-cwd");
            write_fixture(&cwd.join("entry.mjs"), "setInterval(() => {}, 1000);");

            start_fake_javascript_process(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-backlog",
                "[\"net\"]",
            );

            let listen = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-backlog",
                JavascriptSyncRpcRequest {
                    id: 1,
                    method: String::from("net.listen"),
                    args: vec![json!({
                        "host": "127.0.0.1",
                        "port": 0,
                        "backlog": 1,
                    })],
                },
            )
            .expect("listen through sidecar net RPC");
            let server_id = listen["serverId"].as_str().expect("server id").to_string();
            let guest_port = listen["localPort"]
                .as_u64()
                .and_then(|value| u16::try_from(value).ok())
                .expect("listener port");

            let first_client = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-backlog",
                JavascriptSyncRpcRequest {
                    id: 2,
                    method: String::from("net.connect"),
                    args: vec![json!({
                        "host": "127.0.0.1",
                        "port": guest_port,
                    })],
                },
            )
            .expect("queue first backlog client");
            let first_client_socket_id = first_client["socketId"]
                .as_str()
                .expect("first client socket id")
                .to_string();

            let second_connect = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-backlog",
                JavascriptSyncRpcRequest {
                    id: 3,
                    method: String::from("net.connect"),
                    args: vec![json!({
                        "host": "127.0.0.1",
                        "port": guest_port,
                    })],
                },
            )
            .expect_err("reject second queued backlog client");
            assert!(
                second_connect.to_string().contains("backlog is full"),
                "{second_connect}"
            );

            let first_connection = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-backlog",
                JavascriptSyncRpcRequest {
                    id: 4,
                    method: String::from("net.server_poll"),
                    args: vec![json!(server_id.clone()), json!(250)],
                },
            )
            .expect("accept first backlog connection");
            assert_eq!(first_connection["type"], Value::from("connection"));
            let first_socket_id = first_connection["socketId"]
                .as_str()
                .expect("first socket id")
                .to_string();

            let connection_count = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-backlog",
                JavascriptSyncRpcRequest {
                    id: 5,
                    method: String::from("net.server_connections"),
                    args: vec![json!(server_id.clone())],
                },
            )
            .expect("query server connections");
            assert_eq!(connection_count, json!(1));

            let second_poll = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-backlog",
                JavascriptSyncRpcRequest {
                    id: 6,
                    method: String::from("net.server_poll"),
                    args: vec![json!(server_id.clone()), json!(50)],
                },
            )
            .expect("poll second backlog connection");
            assert_eq!(second_poll, Value::Null);

            let connection_count = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-backlog",
                JavascriptSyncRpcRequest {
                    id: 7,
                    method: String::from("net.server_connections"),
                    args: vec![json!(server_id.clone())],
                },
            )
            .expect("query server connections after backlog rejection");
            assert_eq!(connection_count, json!(1));

            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-backlog",
                JavascriptSyncRpcRequest {
                    id: 8,
                    method: String::from("net.destroy"),
                    args: vec![json!(first_socket_id)],
                },
            )
            .expect("destroy first backlog socket");
            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-backlog",
                JavascriptSyncRpcRequest {
                    id: 9,
                    method: String::from("net.destroy"),
                    args: vec![json!(first_client_socket_id)],
                },
            )
            .expect("destroy first backlog client socket");
            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-backlog",
                JavascriptSyncRpcRequest {
                    id: 10,
                    method: String::from("net.server_close"),
                    args: vec![json!(server_id)],
                },
            )
            .expect("close backlog listener");

            sidecar
                .dispose_vm_internal_blocking(
                    &connection_id,
                    &session_id,
                    &vm_id,
                    DisposeReason::Requested,
                )
                .expect("dispose backlog vm");
        }
        fn javascript_net_poll_clamps_guest_wait_to_sidecar_ceiling() {
            assert_eq!(clamp_javascript_net_poll_wait(0), Duration::ZERO);
            assert_eq!(
                clamp_javascript_net_poll_wait(10),
                Duration::from_millis(10)
            );
            assert_eq!(
                clamp_javascript_net_poll_wait(10_000),
                Duration::from_millis(50)
            );
            assert_eq!(
                clamp_javascript_net_poll_wait(u64::MAX),
                Duration::from_millis(50)
            );
        }
        fn javascript_net_poll_timeout_does_not_block_concurrent_vm_dispose() {
            assert_node_available();

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let poll_vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create poll vm");
            let dispose_vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create dispose vm");
            let cwd = temp_dir("agent-os-sidecar-js-net-poll-clamp-cwd");
            write_fixture(&cwd.join("entry.mjs"), "setInterval(() => {}, 1000);");

            start_fake_javascript_process(
                &mut sidecar,
                &poll_vm_id,
                &cwd,
                "proc-js-poll",
                "[\"net\"]",
            );

            let listen = call_javascript_sync_rpc(
                &mut sidecar,
                &poll_vm_id,
                "proc-js-poll",
                JavascriptSyncRpcRequest {
                    id: 1,
                    method: String::from("net.listen"),
                    args: vec![json!({
                        "host": "127.0.0.1",
                        "port": 0,
                    })],
                },
            )
            .expect("listen for net.poll clamp test");
            let server_id = listen["serverId"].as_str().expect("server id").to_string();
            let guest_port = listen["localPort"]
                .as_u64()
                .and_then(|value| u16::try_from(value).ok())
                .expect("listener port");

            let client = call_javascript_sync_rpc(
                &mut sidecar,
                &poll_vm_id,
                "proc-js-poll",
                JavascriptSyncRpcRequest {
                    id: 2,
                    method: String::from("net.connect"),
                    args: vec![json!({
                        "host": "127.0.0.1",
                        "port": guest_port,
                    })],
                },
            )
            .expect("connect poll client");
            let client_socket_id = client["socketId"]
                .as_str()
                .expect("client socket id")
                .to_string();

            let accepted = call_javascript_sync_rpc(
                &mut sidecar,
                &poll_vm_id,
                "proc-js-poll",
                JavascriptSyncRpcRequest {
                    id: 3,
                    method: String::from("net.server_poll"),
                    args: vec![json!(server_id.clone()), json!(250)],
                },
            )
            .expect("accept poll client");
            let server_socket_id = accepted["socketId"]
                .as_str()
                .expect("accepted socket id")
                .to_string();

            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build local runtime for net.poll clamp test");
            let local = tokio::task::LocalSet::new();
            let cleanup_connection_id = connection_id.clone();
            let cleanup_session_id = session_id.clone();
            let cleanup_poll_vm_id = poll_vm_id.clone();
            let cleanup_server_socket_id = server_socket_id.clone();
            let concurrency_elapsed = runtime.block_on(local.run_until(async move {
                let sidecar = std::rc::Rc::new(std::cell::RefCell::new(sidecar));
                let dispose_sidecar = std::rc::Rc::clone(&sidecar);
                let poll_sidecar = std::rc::Rc::clone(&sidecar);
                let dispose_connection_id = connection_id.clone();
                let dispose_session_id = session_id.clone();
                let dispose_vm_id_for_task = dispose_vm_id.clone();
                let poll_vm_id_for_task = poll_vm_id.clone();
                let server_socket_id_for_task = server_socket_id.clone();

                let started = std::time::Instant::now();
                let dispose = tokio::task::spawn_local(async move {
                    tokio::task::yield_now().await;
                    let mut sidecar = dispose_sidecar.borrow_mut();
                    let response = sidecar
                        .dispatch(request(
                            4,
                            OwnershipScope::vm(
                                &dispose_connection_id,
                                &dispose_session_id,
                                &dispose_vm_id_for_task,
                            ),
                            RequestPayload::DisposeVm(DisposeVmRequest {
                                reason: DisposeReason::Requested,
                            }),
                        ))
                        .await
                        .expect("dispose second vm while first net.poll waits");
                    match response.response.payload {
                        ResponsePayload::VmDisposed(_) => {}
                        other => panic!("unexpected dispose response payload: {other:?}"),
                    }
                });
                let poll = tokio::task::spawn_local(async move {
                    let mut sidecar = poll_sidecar.borrow_mut();
                    let poll_started = std::time::Instant::now();
                    let response = call_javascript_sync_rpc(
                        &mut sidecar,
                        &poll_vm_id_for_task,
                        "proc-js-poll",
                        JavascriptSyncRpcRequest {
                            id: 4,
                            method: String::from("net.poll"),
                            args: vec![json!(server_socket_id_for_task), json!(u64::MAX)],
                        },
                    )
                    .expect("poll response");
                    (response, poll_started.elapsed())
                });

                let (dispose_result, poll_result) = tokio::join!(dispose, poll);
                dispose_result.expect("join dispose task");
                let (poll_response, poll_elapsed) = poll_result.expect("join poll task");
                assert_eq!(poll_response, Value::Null);
                assert!(
                    poll_elapsed <= Duration::from_millis(200),
                    "net.poll stayed blocked too long: {poll_elapsed:?}"
                );
                let sidecar = std::rc::Rc::try_unwrap(sidecar)
                    .expect("recover sidecar after local tasks")
                    .into_inner();
                (sidecar, started.elapsed())
            }));
            let (mut sidecar, dispose_elapsed) = concurrency_elapsed;
            assert!(
                dispose_elapsed <= Duration::from_millis(200),
                "dispose should not wait behind guest net.poll: {dispose_elapsed:?}"
            );

            call_javascript_sync_rpc(
                &mut sidecar,
                &cleanup_poll_vm_id,
                "proc-js-poll",
                JavascriptSyncRpcRequest {
                    id: 5,
                    method: String::from("net.destroy"),
                    args: vec![json!(cleanup_server_socket_id)],
                },
            )
            .expect("destroy accepted socket");
            call_javascript_sync_rpc(
                &mut sidecar,
                &cleanup_poll_vm_id,
                "proc-js-poll",
                JavascriptSyncRpcRequest {
                    id: 6,
                    method: String::from("net.destroy"),
                    args: vec![json!(client_socket_id)],
                },
            )
            .expect("destroy client socket");
            call_javascript_sync_rpc(
                &mut sidecar,
                &cleanup_poll_vm_id,
                "proc-js-poll",
                JavascriptSyncRpcRequest {
                    id: 7,
                    method: String::from("net.server_close"),
                    args: vec![json!(server_id)],
                },
            )
            .expect("close poll listener");
            sidecar
                .dispose_vm_internal_blocking(
                    &cleanup_connection_id,
                    &cleanup_session_id,
                    &cleanup_poll_vm_id,
                    DisposeReason::Requested,
                )
                .expect("dispose poll vm");
        }
        fn javascript_network_bind_policy_restricts_hosts_and_ports() {
            assert_node_available();

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm_with_metadata(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
                BTreeMap::from([
                    (
                        String::from(VM_LISTEN_PORT_MIN_METADATA_KEY),
                        String::from("49152"),
                    ),
                    (
                        String::from(VM_LISTEN_PORT_MAX_METADATA_KEY),
                        String::from("49160"),
                    ),
                ]),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-js-bind-policy-cwd");
            write_fixture(&cwd.join("entry.mjs"), "setInterval(() => {}, 1000);");
            start_fake_javascript_process(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-bind-policy",
                "[\"dgram\",\"net\"]",
            );

            let unspecified = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-bind-policy",
                JavascriptSyncRpcRequest {
                    id: 1,
                    method: String::from("net.listen"),
                    args: vec![json!({
                        "host": "0.0.0.0",
                        "port": 49152,
                    })],
                },
            )
            .expect("normalize unspecified TCP listen host onto VM-local loopback");
            assert_eq!(unspecified["localAddress"], Value::from("0.0.0.0"));
            assert_eq!(unspecified["localPort"], Value::from(49152));

            let non_loopback = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-bind-policy",
                JavascriptSyncRpcRequest {
                    id: 2,
                    method: String::from("net.listen"),
                    args: vec![json!({
                        "host": "192.168.1.10",
                        "port": 49154,
                    })],
                },
            )
            .expect_err("deny non-loopback TCP listen host");
            assert!(
                non_loopback
                    .to_string()
                    .contains("must bind to loopback or unspecified addresses"),
                "{non_loopback}"
            );

            let privileged = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-bind-policy",
                JavascriptSyncRpcRequest {
                    id: 3,
                    method: String::from("net.listen"),
                    args: vec![json!({
                        "host": "127.0.0.1",
                        "port": 80,
                    })],
                },
            )
            .expect_err("deny privileged port");
            assert!(
                privileged
                    .to_string()
                    .contains("privileged listen port 80 requires"),
                "{privileged}"
            );

            let out_of_range = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-bind-policy",
                JavascriptSyncRpcRequest {
                    id: 4,
                    method: String::from("net.listen"),
                    args: vec![json!({
                        "host": "127.0.0.1",
                        "port": 40000,
                    })],
                },
            )
            .expect_err("deny out-of-range port");
            assert!(
                out_of_range
                    .to_string()
                    .contains("outside the allowed range 49152-49160"),
                "{out_of_range}"
            );

            let udp_socket = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-bind-policy",
                JavascriptSyncRpcRequest {
                    id: 5,
                    method: String::from("dgram.createSocket"),
                    args: vec![json!({ "type": "udp4" })],
                },
            )
            .expect("create udp socket");
            let udp_socket_id = udp_socket["socketId"]
                .as_str()
                .expect("udp socket id")
                .to_string();

            let udp_unspecified = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-bind-policy",
                JavascriptSyncRpcRequest {
                    id: 6,
                    method: String::from("dgram.bind"),
                    args: vec![
                        json!(udp_socket_id),
                        json!({
                            "address": "0.0.0.0",
                            "port": 49153,
                        }),
                    ],
                },
            )
            .expect("normalize unspecified UDP bind host onto VM-local loopback");
            assert_eq!(udp_unspecified["localAddress"], Value::from("0.0.0.0"));
            assert_eq!(udp_unspecified["localPort"], Value::from(49153));

            let success = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-bind-policy",
                JavascriptSyncRpcRequest {
                    id: 7,
                    method: String::from("net.listen"),
                    args: vec![json!({
                        "host": "127.0.0.1",
                        "port": 49155,
                    })],
                },
            )
            .expect("allow loopback listener inside configured range");
            assert_eq!(success["localAddress"], Value::from("127.0.0.1"));
            assert_eq!(success["localPort"], Value::from(49155));
        }
        fn javascript_network_bind_policy_can_allow_privileged_guest_ports() {
            assert_node_available();

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm_with_metadata(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
                BTreeMap::from([
                    (
                        String::from(VM_LISTEN_PORT_MIN_METADATA_KEY),
                        String::from("1"),
                    ),
                    (
                        String::from(VM_LISTEN_PORT_MAX_METADATA_KEY),
                        String::from("128"),
                    ),
                    (
                        String::from(VM_LISTEN_ALLOW_PRIVILEGED_METADATA_KEY),
                        String::from("true"),
                    ),
                ]),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-js-privileged-listen-cwd");
            write_fixture(&cwd.join("entry.mjs"), "setInterval(() => {}, 1000);");
            start_fake_javascript_process(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-privileged",
                "[\"net\"]",
            );

            let listen = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-privileged",
                JavascriptSyncRpcRequest {
                    id: 1,
                    method: String::from("net.listen"),
                    args: vec![json!({
                        "host": "127.0.0.1",
                        "port": 80,
                    })],
                },
            )
            .expect("allow privileged guest port");
            assert_eq!(listen["localAddress"], Value::from("127.0.0.1"));
            assert_eq!(listen["localPort"], Value::from(80));
        }
        fn javascript_network_listeners_are_isolated_per_vm_even_with_same_guest_port() {
            assert_node_available();

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_a = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm a");
            let vm_b = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm b");
            let cwd_a = temp_dir("agent-os-sidecar-js-net-isolation-a");
            let cwd_b = temp_dir("agent-os-sidecar-js-net-isolation-b");
            write_fixture(&cwd_a.join("entry.mjs"), "setInterval(() => {}, 1000);");
            write_fixture(&cwd_b.join("entry.mjs"), "setInterval(() => {}, 1000);");
            start_fake_javascript_process(&mut sidecar, &vm_a, &cwd_a, "proc-a", "[\"net\"]");
            start_fake_javascript_process(&mut sidecar, &vm_b, &cwd_b, "proc-b", "[\"net\"]");

            let listen_a = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_a,
                "proc-a",
                JavascriptSyncRpcRequest {
                    id: 1,
                    method: String::from("net.listen"),
                    args: vec![json!({
                        "host": "127.0.0.1",
                        "port": 43111,
                    })],
                },
            )
            .expect("listen on vm a");
            let listen_b = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_b,
                "proc-b",
                JavascriptSyncRpcRequest {
                    id: 1,
                    method: String::from("net.listen"),
                    args: vec![json!({
                        "host": "127.0.0.1",
                        "port": 43111,
                    })],
                },
            )
            .expect("listen on vm b");
            assert_eq!(listen_a["localPort"], Value::from(43111));
            assert_eq!(listen_b["localPort"], Value::from(43111));

            let connect_a = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_a,
                "proc-a",
                JavascriptSyncRpcRequest {
                    id: 2,
                    method: String::from("net.connect"),
                    args: vec![json!({
                        "host": "127.0.0.1",
                        "port": 43111,
                    })],
                },
            )
            .expect("connect within vm a");
            let connect_b = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_b,
                "proc-b",
                JavascriptSyncRpcRequest {
                    id: 2,
                    method: String::from("net.connect"),
                    args: vec![json!({
                        "host": "127.0.0.1",
                        "port": 43111,
                    })],
                },
            )
            .expect("connect within vm b");
            assert_eq!(connect_a["remotePort"], Value::from(43111));
            assert_eq!(connect_b["remotePort"], Value::from(43111));

            let server_id_a = listen_a["serverId"]
                .as_str()
                .expect("server id a")
                .to_string();
            let server_id_b = listen_b["serverId"]
                .as_str()
                .expect("server id b")
                .to_string();
            let accepted_a = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_a,
                "proc-a",
                JavascriptSyncRpcRequest {
                    id: 3,
                    method: String::from("net.server_poll"),
                    args: vec![json!(server_id_a), json!(250)],
                },
            )
            .expect("accept vm a connection");
            let accepted_b = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_b,
                "proc-b",
                JavascriptSyncRpcRequest {
                    id: 3,
                    method: String::from("net.server_poll"),
                    args: vec![json!(server_id_b), json!(250)],
                },
            )
            .expect("accept vm b connection");
            assert_eq!(accepted_a["type"], Value::from("connection"));
            assert_eq!(accepted_b["type"], Value::from("connection"));
            assert_eq!(accepted_a["localPort"], Value::from(43111));
            assert_eq!(accepted_b["localPort"], Value::from(43111));

            let query_a = sidecar
                .dispatch_blocking(request(
                    50,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_a),
                    RequestPayload::FindListener(FindListenerRequest {
                        host: Some(String::from("127.0.0.1")),
                        port: Some(43111),
                        path: None,
                    }),
                ))
                .expect("query vm a listener");
            let query_b = sidecar
                .dispatch_blocking(request(
                    51,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_b),
                    RequestPayload::FindListener(FindListenerRequest {
                        host: Some(String::from("127.0.0.1")),
                        port: Some(43111),
                        path: None,
                    }),
                ))
                .expect("query vm b listener");
            match query_a.response.payload {
                ResponsePayload::ListenerSnapshot(snapshot) => {
                    let listener = snapshot.listener.expect("vm a listener");
                    assert_eq!(listener.process_id, "proc-a");
                    assert_eq!(listener.host.as_deref(), Some("127.0.0.1"));
                    assert_eq!(listener.port, Some(43111));
                }
                other => panic!("unexpected vm a listener response: {other:?}"),
            }
            match query_b.response.payload {
                ResponsePayload::ListenerSnapshot(snapshot) => {
                    let listener = snapshot.listener.expect("vm b listener");
                    assert_eq!(listener.process_id, "proc-b");
                    assert_eq!(listener.host.as_deref(), Some("127.0.0.1"));
                    assert_eq!(listener.port, Some(43111));
                }
                other => panic!("unexpected vm b listener response: {other:?}"),
            }
        }
        fn javascript_net_rpc_listens_and_connects_over_unix_domain_sockets() {
            assert_node_available();

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-js-net-unix-cwd");
            write_fixture(&cwd.join("entry.mjs"), "setInterval(() => {}, 1000);");

            let context =
                sidecar
                    .javascript_engine
                    .create_context(CreateJavascriptContextRequest {
                        vm_id: vm_id.clone(),
                        bootstrap_module: None,
                        compile_cache_root: None,
                    });
            let execution = sidecar
            .javascript_engine
            .start_execution(StartJavascriptExecutionRequest {
                vm_id: vm_id.clone(),
                context_id: context.context_id,
                argv: vec![String::from("./entry.mjs")],
                env: BTreeMap::from([(
                    String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
                    String::from(
                        "[\"assert\",\"buffer\",\"console\",\"crypto\",\"events\",\"fs\",\"net\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
                    ),
                )]),
                cwd: cwd.clone(),
                inline_code: None,
            })
            .expect("start fake javascript execution");

            let kernel_handle = {
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                vm.kernel
                    .spawn_process(
                        JAVASCRIPT_COMMAND,
                        vec![String::from("./entry.mjs")],
                        SpawnOptions {
                            requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                            cwd: Some(String::from("/")),
                            ..SpawnOptions::default()
                        },
                    )
                    .expect("spawn kernel javascript process")
            };

            {
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                vm.active_processes.insert(
                    String::from("proc-js-unix"),
                    ActiveProcess::new(
                        kernel_handle.pid(),
                        kernel_handle,
                        GuestRuntimeKind::JavaScript,
                        ActiveExecution::Javascript(execution),
                    )
                    .with_host_cwd(cwd.clone()),
                );
            }

            let bridge = sidecar.bridge.clone();
            let dns = sidecar.vms.get(&vm_id).expect("javascript vm").dns.clone();
            let limits = ResourceLimits::default();
            let socket_paths = JavascriptSocketPathContext {
                sandbox_root: cwd.clone(),
                mounts: Vec::new(),
                listen_policy: VmListenPolicy::default(),
                loopback_exempt_ports: BTreeSet::new(),
                tcp_loopback_guest_to_host_ports: BTreeMap::new(),
                udp_loopback_guest_to_host_ports: BTreeMap::new(),
                udp_loopback_host_to_guest_ports: BTreeMap::new(),
                used_tcp_guest_ports: BTreeMap::new(),
                used_udp_guest_ports: BTreeMap::new(),
            };
            let socket_path = "/tmp/agent-os.sock";
            let host_socket_path = cwd.join("tmp/agent-os.sock");

            let listen = {
                let counts = sidecar
                    .vms
                    .get(&vm_id)
                    .and_then(|vm| vm.active_processes.get("proc-js-unix"))
                    .expect("unix process")
                    .network_resource_counts();
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                let process = vm
                    .active_processes
                    .get_mut("proc-js-unix")
                    .expect("unix process");
                service_javascript_net_sync_rpc(
                    &bridge,
                    &vm_id,
                    &dns,
                    &socket_paths,
                    &mut vm.kernel,
                    process,
                    &JavascriptSyncRpcRequest {
                        id: 1,
                        method: String::from("net.listen"),
                        args: vec![json!({
                            "path": socket_path,
                            "backlog": 1,
                        })],
                    },
                    &limits,
                    counts,
                )
                .expect("listen on unix socket")
            };
            let server_id = listen["serverId"].as_str().expect("server id").to_string();
            assert_eq!(listen["path"], Value::String(String::from(socket_path)));
            {
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                assert!(
                    vm.kernel
                        .exists(socket_path)
                        .expect("kernel socket placeholder exists"),
                    "kernel did not expose unix socket path"
                );
            }
            assert!(host_socket_path.exists(), "host unix socket path missing");

            let listener_lookup = sidecar
                .dispatch_blocking(request(
                    2,
                    OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                    RequestPayload::FindListener(FindListenerRequest {
                        host: None,
                        port: None,
                        path: Some(String::from(socket_path)),
                    }),
                ))
                .expect("query unix listener");
            match listener_lookup.response.payload {
                ResponsePayload::ListenerSnapshot(snapshot) => {
                    let listener = snapshot.listener.expect("listener snapshot");
                    assert_eq!(listener.process_id, "proc-js-unix");
                    assert_eq!(listener.path.as_deref(), Some(socket_path));
                }
                other => panic!("unexpected listener response payload: {other:?}"),
            }

            let connect = {
                let counts = sidecar
                    .vms
                    .get(&vm_id)
                    .and_then(|vm| vm.active_processes.get("proc-js-unix"))
                    .expect("unix process")
                    .network_resource_counts();
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                let process = vm
                    .active_processes
                    .get_mut("proc-js-unix")
                    .expect("unix process");
                service_javascript_net_sync_rpc(
                    &bridge,
                    &vm_id,
                    &dns,
                    &socket_paths,
                    &mut vm.kernel,
                    process,
                    &JavascriptSyncRpcRequest {
                        id: 3,
                        method: String::from("net.connect"),
                        args: vec![json!({
                            "path": socket_path,
                        })],
                    },
                    &limits,
                    counts,
                )
                .expect("connect to unix listener")
            };
            let client_socket_id = connect["socketId"]
                .as_str()
                .expect("client socket id")
                .to_string();
            assert_eq!(
                connect["remotePath"],
                Value::String(String::from(socket_path))
            );

            let accepted = {
                let counts = sidecar
                    .vms
                    .get(&vm_id)
                    .and_then(|vm| vm.active_processes.get("proc-js-unix"))
                    .expect("unix process")
                    .network_resource_counts();
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                let process = vm
                    .active_processes
                    .get_mut("proc-js-unix")
                    .expect("unix process");
                service_javascript_net_sync_rpc(
                    &bridge,
                    &vm_id,
                    &dns,
                    &socket_paths,
                    &mut vm.kernel,
                    process,
                    &JavascriptSyncRpcRequest {
                        id: 4,
                        method: String::from("net.server_poll"),
                        args: vec![json!(server_id), json!(250)],
                    },
                    &limits,
                    counts,
                )
                .expect("accept unix socket connection")
            };
            let server_socket_id = accepted["socketId"]
                .as_str()
                .expect("server socket id")
                .to_string();
            assert_eq!(
                accepted["localPath"],
                Value::String(String::from(socket_path))
            );

            {
                let counts = sidecar
                    .vms
                    .get(&vm_id)
                    .and_then(|vm| vm.active_processes.get("proc-js-unix"))
                    .expect("unix process")
                    .network_resource_counts();
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                let process = vm
                    .active_processes
                    .get_mut("proc-js-unix")
                    .expect("unix process");
                let connections = service_javascript_net_sync_rpc(
                    &bridge,
                    &vm_id,
                    &dns,
                    &socket_paths,
                    &mut vm.kernel,
                    process,
                    &JavascriptSyncRpcRequest {
                        id: 5,
                        method: String::from("net.server_connections"),
                        args: vec![json!(server_id)],
                    },
                    &limits,
                    counts,
                )
                .expect("query unix server connections");
                assert_eq!(connections, json!(1));
            }

            {
                let counts = sidecar
                    .vms
                    .get(&vm_id)
                    .and_then(|vm| vm.active_processes.get("proc-js-unix"))
                    .expect("unix process")
                    .network_resource_counts();
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                let process = vm
                    .active_processes
                    .get_mut("proc-js-unix")
                    .expect("unix process");
                service_javascript_net_sync_rpc(
                    &bridge,
                    &vm_id,
                    &dns,
                    &socket_paths,
                    &mut vm.kernel,
                    process,
                    &JavascriptSyncRpcRequest {
                        id: 6,
                        method: String::from("net.write"),
                        args: vec![
                            json!(client_socket_id),
                            json!({
                                "__agentOsType": "bytes",
                                "base64": "cGluZw==",
                            }),
                        ],
                    },
                    &limits,
                    counts,
                )
                .expect("write unix client payload");
            }

            {
                let counts = sidecar
                    .vms
                    .get(&vm_id)
                    .and_then(|vm| vm.active_processes.get("proc-js-unix"))
                    .expect("unix process")
                    .network_resource_counts();
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                let process = vm
                    .active_processes
                    .get_mut("proc-js-unix")
                    .expect("unix process");
                service_javascript_net_sync_rpc(
                    &bridge,
                    &vm_id,
                    &dns,
                    &socket_paths,
                    &mut vm.kernel,
                    process,
                    &JavascriptSyncRpcRequest {
                        id: 7,
                        method: String::from("net.shutdown"),
                        args: vec![json!(client_socket_id)],
                    },
                    &limits,
                    counts,
                )
                .expect("shutdown unix client write half");
            }

            let server_data = {
                let counts = sidecar
                    .vms
                    .get(&vm_id)
                    .and_then(|vm| vm.active_processes.get("proc-js-unix"))
                    .expect("unix process")
                    .network_resource_counts();
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                let process = vm
                    .active_processes
                    .get_mut("proc-js-unix")
                    .expect("unix process");
                service_javascript_net_sync_rpc(
                    &bridge,
                    &vm_id,
                    &dns,
                    &socket_paths,
                    &mut vm.kernel,
                    process,
                    &JavascriptSyncRpcRequest {
                        id: 8,
                        method: String::from("net.poll"),
                        args: vec![json!(server_socket_id), json!(250)],
                    },
                    &limits,
                    counts,
                )
                .expect("poll unix server socket data")
            };
            assert_eq!(
                server_data["data"]["base64"],
                Value::String(String::from("cGluZw=="))
            );

            {
                let counts = sidecar
                    .vms
                    .get(&vm_id)
                    .and_then(|vm| vm.active_processes.get("proc-js-unix"))
                    .expect("unix process")
                    .network_resource_counts();
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                let process = vm
                    .active_processes
                    .get_mut("proc-js-unix")
                    .expect("unix process");
                let server_end = service_javascript_net_sync_rpc(
                    &bridge,
                    &vm_id,
                    &dns,
                    &socket_paths,
                    &mut vm.kernel,
                    process,
                    &JavascriptSyncRpcRequest {
                        id: 9,
                        method: String::from("net.poll"),
                        args: vec![json!(server_socket_id), json!(250)],
                    },
                    &limits,
                    counts,
                )
                .expect("poll unix server socket end");
                assert_eq!(server_end["type"], Value::String(String::from("end")));
            }

            {
                let counts = sidecar
                    .vms
                    .get(&vm_id)
                    .and_then(|vm| vm.active_processes.get("proc-js-unix"))
                    .expect("unix process")
                    .network_resource_counts();
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                let process = vm
                    .active_processes
                    .get_mut("proc-js-unix")
                    .expect("unix process");
                service_javascript_net_sync_rpc(
                    &bridge,
                    &vm_id,
                    &dns,
                    &socket_paths,
                    &mut vm.kernel,
                    process,
                    &JavascriptSyncRpcRequest {
                        id: 10,
                        method: String::from("net.write"),
                        args: vec![
                            json!(server_socket_id),
                            json!({
                                "__agentOsType": "bytes",
                                "base64": "cG9uZw==",
                            }),
                        ],
                    },
                    &limits,
                    counts,
                )
                .expect("write unix server payload");
            }

            {
                let counts = sidecar
                    .vms
                    .get(&vm_id)
                    .and_then(|vm| vm.active_processes.get("proc-js-unix"))
                    .expect("unix process")
                    .network_resource_counts();
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                let process = vm
                    .active_processes
                    .get_mut("proc-js-unix")
                    .expect("unix process");
                service_javascript_net_sync_rpc(
                    &bridge,
                    &vm_id,
                    &dns,
                    &socket_paths,
                    &mut vm.kernel,
                    process,
                    &JavascriptSyncRpcRequest {
                        id: 11,
                        method: String::from("net.shutdown"),
                        args: vec![json!(server_socket_id)],
                    },
                    &limits,
                    counts,
                )
                .expect("shutdown unix server write half");
            }

            let client_data = {
                let counts = sidecar
                    .vms
                    .get(&vm_id)
                    .and_then(|vm| vm.active_processes.get("proc-js-unix"))
                    .expect("unix process")
                    .network_resource_counts();
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                let process = vm
                    .active_processes
                    .get_mut("proc-js-unix")
                    .expect("unix process");
                service_javascript_net_sync_rpc(
                    &bridge,
                    &vm_id,
                    &dns,
                    &socket_paths,
                    &mut vm.kernel,
                    process,
                    &JavascriptSyncRpcRequest {
                        id: 12,
                        method: String::from("net.poll"),
                        args: vec![json!(client_socket_id), json!(250)],
                    },
                    &limits,
                    counts,
                )
                .expect("poll unix client socket data")
            };
            assert_eq!(
                client_data["data"]["base64"],
                Value::String(String::from("cG9uZw=="))
            );

            {
                let counts = sidecar
                    .vms
                    .get(&vm_id)
                    .and_then(|vm| vm.active_processes.get("proc-js-unix"))
                    .expect("unix process")
                    .network_resource_counts();
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                let process = vm
                    .active_processes
                    .get_mut("proc-js-unix")
                    .expect("unix process");
                let client_end = service_javascript_net_sync_rpc(
                    &bridge,
                    &vm_id,
                    &dns,
                    &socket_paths,
                    &mut vm.kernel,
                    process,
                    &JavascriptSyncRpcRequest {
                        id: 13,
                        method: String::from("net.poll"),
                        args: vec![json!(client_socket_id), json!(250)],
                    },
                    &limits,
                    counts,
                )
                .expect("poll unix client socket end");
                assert_eq!(client_end["type"], Value::String(String::from("end")));
            }

            for (id, request_id) in [(&client_socket_id, 14_u64), (&server_socket_id, 15_u64)] {
                let counts = sidecar
                    .vms
                    .get(&vm_id)
                    .and_then(|vm| vm.active_processes.get("proc-js-unix"))
                    .expect("unix process")
                    .network_resource_counts();
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                let process = vm
                    .active_processes
                    .get_mut("proc-js-unix")
                    .expect("unix process");
                service_javascript_net_sync_rpc(
                    &bridge,
                    &vm_id,
                    &dns,
                    &socket_paths,
                    &mut vm.kernel,
                    process,
                    &JavascriptSyncRpcRequest {
                        id: request_id,
                        method: String::from("net.destroy"),
                        args: vec![json!(id)],
                    },
                    &limits,
                    counts,
                )
                .expect("destroy unix socket");
            }

            {
                let counts = sidecar
                    .vms
                    .get(&vm_id)
                    .and_then(|vm| vm.active_processes.get("proc-js-unix"))
                    .expect("unix process")
                    .network_resource_counts();
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                let process = vm
                    .active_processes
                    .get_mut("proc-js-unix")
                    .expect("unix process");
                service_javascript_net_sync_rpc(
                    &bridge,
                    &vm_id,
                    &dns,
                    &socket_paths,
                    &mut vm.kernel,
                    process,
                    &JavascriptSyncRpcRequest {
                        id: 16,
                        method: String::from("net.server_close"),
                        args: vec![json!(server_id)],
                    },
                    &limits,
                    counts,
                )
                .expect("close unix listener");
            }

            sidecar
                .dispose_vm_internal_blocking(
                    &connection_id,
                    &session_id,
                    &vm_id,
                    DisposeReason::Requested,
                )
                .expect("dispose unix vm");
        }
        fn javascript_child_process_rpc_spawns_nested_node_processes_inside_vm_kernel() {
            assert_node_available();

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-js-child-process-cwd");
            write_fixture(
                &cwd.join("child.mjs"),
                r#"
import fs from "node:fs";

const note = fs.readFileSync("/rpc/note.txt", "utf8").trim();
console.log(`${process.argv[2]}:${process.pid}:${process.ppid}:${note}`);
"#,
            );
            write_fixture(
                &cwd.join("entry.mjs"),
                r#"
const { execSync, spawn } = require("node:child_process");

const child = spawn("node", ["./child.mjs", "spawn"], {
  stdio: ["ignore", "pipe", "pipe"],
});
let spawnOutput = "";
let spawnError = "";
child.stdout.setEncoding("utf8");
child.stderr.setEncoding("utf8");
child.stdout.on("data", (chunk) => {
  spawnOutput += chunk;
});
child.stderr.on("data", (chunk) => {
  spawnError += chunk;
});
await new Promise((resolve, reject) => {
  child.on("error", reject);
  child.on("close", (code) => {
    if (code !== 0) {
      reject(new Error(`spawn exit ${code}: ${spawnError}`));
      return;
    }
    resolve();
  });
});

const execOutput = execSync("node ./child.mjs exec", {
  encoding: "utf8",
}).trim();

console.log(JSON.stringify({
  parentPid: process.pid,
  childPid: child.pid,
  spawnOutput: spawnOutput.trim(),
  execOutput,
}));
"#,
            );

            {
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                vm.kernel
                    .write_file("/rpc/note.txt", b"hello from nested child".to_vec())
                    .expect("seed rpc note");
            }

            let context =
                sidecar
                    .javascript_engine
                    .create_context(CreateJavascriptContextRequest {
                        vm_id: vm_id.clone(),
                        bootstrap_module: None,
                        compile_cache_root: None,
                    });
            let execution = sidecar
            .javascript_engine
            .start_execution(StartJavascriptExecutionRequest {
                vm_id: vm_id.clone(),
                context_id: context.context_id,
                argv: vec![String::from("./entry.mjs")],
                env: BTreeMap::from([(
                    String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
                    String::from(
                        "[\"assert\",\"buffer\",\"console\",\"child_process\",\"crypto\",\"events\",\"fs\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
                    ),
                )]),
                cwd: cwd.clone(),
                inline_code: None,
            })
            .expect("start fake javascript execution");

            let kernel_handle = {
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                vm.kernel
                    .spawn_process(
                        JAVASCRIPT_COMMAND,
                        vec![String::from("./entry.mjs")],
                        SpawnOptions {
                            requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                            cwd: Some(String::from("/")),
                            ..SpawnOptions::default()
                        },
                    )
                    .expect("spawn kernel javascript process")
            };

            {
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                vm.active_processes.insert(
                    String::from("proc-js-child"),
                    ActiveProcess::new(
                        kernel_handle.pid(),
                        kernel_handle,
                        GuestRuntimeKind::JavaScript,
                        ActiveExecution::Javascript(execution),
                    )
                    .with_host_cwd(cwd.clone()),
                );
            }

            let mut stdout = String::new();
            let mut stderr = String::new();
            let mut exit_code = None;
            for _ in 0..96 {
                let next_event = {
                    let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                    vm.active_processes
                        .get_mut("proc-js-child")
                        .map(|process| {
                            process
                                .execution
                                .poll_event_blocking(Duration::from_secs(5))
                                .expect("poll javascript child_process event")
                        })
                        .flatten()
                };
                let Some(event) = next_event else {
                    if exit_code.is_some() {
                        break;
                    }
                    continue;
                };

                match &event {
                    ActiveExecutionEvent::Stdout(chunk) => {
                        stdout.push_str(&String::from_utf8_lossy(chunk));
                    }
                    ActiveExecutionEvent::Stderr(chunk) => {
                        stderr.push_str(&String::from_utf8_lossy(chunk));
                    }
                    ActiveExecutionEvent::Exited(code) => exit_code = Some(*code),
                    _ => {}
                }

                sidecar
                    .handle_execution_event(&vm_id, "proc-js-child", event)
                    .expect("handle javascript child_process event");
            }

            assert_eq!(exit_code, Some(0), "stderr: {stderr}");
            let parsed: Value =
                serde_json::from_str(stdout.trim()).expect("parse child_process JSON");
            let parent_pid = parsed["parentPid"].as_u64().expect("parent pid") as u32;
            let child_pid = parsed["childPid"].as_u64().expect("child pid") as u32;
            let spawn_parts = parsed["spawnOutput"]
                .as_str()
                .expect("spawn output")
                .split(':')
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let exec_parts = parsed["execOutput"]
                .as_str()
                .expect("exec output")
                .split(':')
                .map(str::to_owned)
                .collect::<Vec<_>>();

            assert_eq!(spawn_parts[0], "spawn");
            assert_eq!(spawn_parts[1].parse::<u32>().expect("spawn pid"), child_pid);
            assert_eq!(
                spawn_parts[2].parse::<u32>().expect("spawn ppid"),
                parent_pid
            );
            assert_eq!(spawn_parts[3], "hello from nested child");
            assert_eq!(exec_parts[0], "exec");
            assert_eq!(exec_parts[2].parse::<u32>().expect("exec ppid"), parent_pid);
            assert_eq!(exec_parts[3], "hello from nested child");
        }
        fn javascript_child_process_rpc_preserves_nested_sigchld_registrations() {
            assert_node_available();

            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");
            let cwd = temp_dir("agent-os-sidecar-js-nested-sigchld-cwd");
            write_fixture(
                &cwd.join("leaf.mjs"),
                [
                    "await new Promise((resolve) => setTimeout(resolve, 200));",
                    "console.log('leaf-exit');",
                ]
                .join("\n"),
            );
            write_fixture(
                &cwd.join("child.mjs"),
                [
                    "import { spawn } from 'node:child_process';",
                    "let sigchldCount = 0;",
                    "process.on('SIGCHLD', () => {",
                    "  sigchldCount += 1;",
                    "  console.log(`nested-sigchld:${sigchldCount}`);",
                    "});",
                    "console.log('nested-sigchld-registered');",
                    "await new Promise((resolve) => setTimeout(resolve, 75));",
                    "const child = spawn('node', ['./leaf.mjs'], { stdio: ['ignore', 'ignore', 'ignore'] });",
                    "await new Promise((resolve, reject) => {",
                    "  child.on('error', reject);",
                    "  child.on('close', (code, signal) => {",
                    "    if (code !== 0 || signal !== null) {",
                    "      reject(new Error(`leaf exit ${code} signal ${signal}`));",
                    "      return;",
                    "    }",
                    "    resolve();",
                    "  });",
                    "});",
                    "const deadline = Date.now() + 2000;",
                    "while (sigchldCount === 0 && Date.now() < deadline) {",
                    "  await new Promise((resolve) => setTimeout(resolve, 10));",
                    "}",
                    "if (sigchldCount === 0) {",
                    "  throw new Error('nested SIGCHLD was not delivered');",
                    "}",
                    "console.log(`nested-sigchld-final:${sigchldCount}`);",
                ]
                .join("\n"),
            );
            write_fixture(
                &cwd.join("entry.mjs"),
                [
                    "import { spawn } from 'node:child_process';",
                    "const child = spawn('node', ['./child.mjs'], { stdio: ['ignore', 'pipe', 'pipe'] });",
                    "let childStdout = '';",
                    "let childStderr = '';",
                    "child.stdout.setEncoding('utf8');",
                    "child.stdout.on('data', (chunk) => {",
                    "  childStdout += chunk;",
                    "});",
                    "child.stderr.setEncoding('utf8');",
                    "child.stderr.on('data', (chunk) => {",
                    "  childStderr += chunk;",
                    "});",
                    "const result = await new Promise((resolve, reject) => {",
                    "  child.on('error', reject);",
                    "  child.on('close', (code, signal) => resolve({ code, signal }));",
                    "});",
                    "console.log(JSON.stringify({",
                    "  code: result.code,",
                    "  signal: result.signal,",
                    "  stdout: childStdout.trim(),",
                    "  stderr: childStderr.trim(),",
                    "}));",
                    "if (result.code !== 0 || result.signal !== null) {",
                    "  process.exitCode = result.code ?? 1;",
                    "}",
                ]
                .join("\n"),
            );

            let context =
                sidecar
                    .javascript_engine
                    .create_context(CreateJavascriptContextRequest {
                        vm_id: vm_id.clone(),
                        bootstrap_module: None,
                        compile_cache_root: None,
                    });
            let execution = sidecar
                .javascript_engine
                .start_execution(StartJavascriptExecutionRequest {
                    vm_id: vm_id.clone(),
                    context_id: context.context_id,
                    argv: vec![String::from("./entry.mjs")],
                    env: BTreeMap::from([(
                        String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
                        String::from(
                            "[\"assert\",\"buffer\",\"console\",\"child_process\",\"crypto\",\"events\",\"fs\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
                        ),
                    )]),
                    cwd: cwd.clone(),
                    inline_code: None,
                })
                .expect("start nested SIGCHLD javascript execution");

            let kernel_handle = {
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                vm.kernel
                    .spawn_process(
                        JAVASCRIPT_COMMAND,
                        vec![String::from("./entry.mjs")],
                        SpawnOptions {
                            requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                            cwd: Some(String::from("/")),
                            ..SpawnOptions::default()
                        },
                    )
                    .expect("spawn kernel javascript process")
            };

            {
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                vm.active_processes.insert(
                    String::from("proc-js-nested-sigchld"),
                    ActiveProcess::new(
                        kernel_handle.pid(),
                        kernel_handle,
                        GuestRuntimeKind::JavaScript,
                        ActiveExecution::Javascript(execution),
                    )
                    .with_host_cwd(cwd.clone()),
                );
            }

            let mut stdout = String::new();
            let mut stderr = String::new();
            let mut exit_code = None;
            for _ in 0..128 {
                let next_event = {
                    let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                    vm.active_processes
                        .get_mut("proc-js-nested-sigchld")
                        .map(|process| {
                            process
                                .execution
                                .poll_event_blocking(Duration::from_secs(5))
                                .expect("poll nested SIGCHLD event")
                        })
                        .flatten()
                };
                let Some(event) = next_event else {
                    if exit_code.is_some() {
                        break;
                    }
                    continue;
                };

                match &event {
                    ActiveExecutionEvent::Stdout(chunk) => {
                        stdout.push_str(&String::from_utf8_lossy(chunk));
                    }
                    ActiveExecutionEvent::Stderr(chunk) => {
                        stderr.push_str(&String::from_utf8_lossy(chunk));
                    }
                    ActiveExecutionEvent::Exited(code) => exit_code = Some(*code),
                    _ => {}
                }

                sidecar
                    .handle_execution_event(&vm_id, "proc-js-nested-sigchld", event)
                    .expect("handle nested SIGCHLD event");
            }

            assert_eq!(exit_code, Some(0), "stderr: {stderr}");
            let parsed: Value =
                serde_json::from_str(stdout.trim()).expect("parse nested SIGCHLD JSON");
            assert_eq!(parsed["code"].as_i64(), Some(0), "stdout: {stdout}");
            assert!(parsed["signal"].is_null(), "stdout: {stdout}");

            let nested_stdout = parsed["stdout"].as_str().expect("nested child stdout");
            assert!(
                nested_stdout.contains("nested-sigchld-registered"),
                "missing registration output: {nested_stdout}"
            );
            assert!(
                nested_stdout.contains("nested-sigchld:1"),
                "missing nested SIGCHLD delivery: {nested_stdout}"
            );
            assert!(
                nested_stdout.contains("nested-sigchld-final:1"),
                "missing nested SIGCHLD final count: {nested_stdout}"
            );
            assert_eq!(
                parsed["stderr"].as_str(),
                Some(""),
                "nested child stderr should stay empty"
            );
        }
        fn javascript_child_process_poll_reports_echild_when_child_disappears_after_drain() {
            let mut sidecar = create_test_sidecar();
            let (connection_id, session_id) =
                authenticate_and_open_session(&mut sidecar).expect("authenticate and open session");
            let vm_id = create_vm(
                &mut sidecar,
                &connection_id,
                &session_id,
                PermissionsPolicy::allow_all(),
            )
            .expect("create vm");

            let kernel_handle = create_kernel_process_handle_for_tests();
            {
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                vm.active_processes.insert(
                    String::from("proc-js-child-gone"),
                    ActiveProcess::new(
                        kernel_handle.pid(),
                        kernel_handle,
                        GuestRuntimeKind::JavaScript,
                        ActiveExecution::Tool(ToolExecution::default()),
                    ),
                );
            }

            sidecar
                .pending_process_events
                .push_back(ProcessEventEnvelope {
                    connection_id: connection_id.clone(),
                    session_id: session_id.clone(),
                    vm_id: vm_id.clone(),
                    process_id: String::from("proc-js-child-gone/ghost-child"),
                    event: ActiveExecutionEvent::Stdout(b"queued-but-undeliverable".to_vec()),
                });

            let mut poll_loop_terminated = false;
            for attempt in 0..3 {
                let error = sidecar
                    .poll_javascript_child_process(&vm_id, "proc-js-child-gone", "ghost-child", 0)
                    .expect_err("missing child should surface ECHILD");
                match error {
                    SidecarError::Execution(message) => {
                        assert!(
                            message.starts_with("ECHILD:"),
                            "expected ECHILD code, got {message}"
                        );
                        assert!(
                            message.contains("proc-js-child-gone/ghost-child"),
                            "expected child label in error, got {message}"
                        );
                        assert_eq!(
                            attempt, 0,
                            "poll loop should stop on first ECHILD instead of retrying"
                        );
                        poll_loop_terminated = true;
                        break;
                    }
                    other => panic!("expected execution error, got {other}"),
                }
            }
            assert!(poll_loop_terminated, "poll loop should terminate on ECHILD");

            let queued = sidecar
                .pending_process_events
                .front()
                .expect("queued event should remain deferred");
            assert_eq!(queued.process_id, "proc-js-child-gone/ghost-child");
            assert_eq!(sidecar.pending_process_events.len(), 1);
        }
        fn javascript_child_process_internal_bootstrap_env_is_allowlisted() {
            let filtered =
                sanitize_javascript_child_process_internal_bootstrap_env(&BTreeMap::from([
                    (
                        String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
                        String::from("[\"fs\"]"),
                    ),
                    (
                        String::from("AGENT_OS_GUEST_PATH_MAPPINGS"),
                        String::from("[]"),
                    ),
                    (
                        String::from("AGENT_OS_VIRTUAL_PROCESS_UID"),
                        String::from("0"),
                    ),
                    (
                        String::from("AGENT_OS_VIRTUAL_PROCESS_VERSION"),
                        String::from("v24.0.0"),
                    ),
                    (
                        String::from("AGENT_OS_VIRTUAL_OS_HOSTNAME"),
                        String::from("agent-os-test"),
                    ),
                    (
                        String::from("AGENT_OS_PARENT_NODE_ALLOW_CHILD_PROCESS"),
                        String::from("1"),
                    ),
                    (
                        String::from("VISIBLE_MARKER"),
                        String::from("child-visible"),
                    ),
                ]));

            assert_eq!(
                filtered.get("AGENT_OS_ALLOWED_NODE_BUILTINS"),
                Some(&String::from("[\"fs\"]"))
            );
            assert_eq!(
                filtered.get("AGENT_OS_GUEST_PATH_MAPPINGS"),
                Some(&String::from("[]"))
            );
            assert_eq!(
                filtered.get("AGENT_OS_VIRTUAL_PROCESS_UID"),
                Some(&String::from("0"))
            );
            assert_eq!(
                filtered.get("AGENT_OS_VIRTUAL_PROCESS_VERSION"),
                Some(&String::from("v24.0.0"))
            );
            assert_eq!(
                filtered.get("AGENT_OS_VIRTUAL_OS_HOSTNAME"),
                Some(&String::from("agent-os-test"))
            );
            assert!(!filtered.contains_key("AGENT_OS_PARENT_NODE_ALLOW_CHILD_PROCESS"));
            assert!(!filtered.contains_key("VISIBLE_MARKER"));
        }
        fn run_service_suite() {
            // Multiple libtest cases in this sidecar integration binary still
            // trip teardown/init crashes around V8-backed execution paths, so
            // keep the broad coverage in one top-level suite.
            session_timeout_response_includes_structured_diagnostics();
            kernel_socket_queries_ignore_stale_sidecar_guest_addresses();
            find_listener_rejects_without_network_inspect_permission();
            find_listener_returns_listener_with_network_inspect_permission();
            find_bound_udp_rejects_without_network_inspect_permission();
            find_bound_udp_returns_socket_with_network_inspect_permission();
            get_process_snapshot_rejects_without_process_inspect_permission();
            get_process_snapshot_returns_processes_with_process_inspect_permission();
            vm_network_resource_counts_ignore_duplicate_sidecar_kernel_entries();
            acp_inbound_fs_requests_read_and_write_vm_files();
            acp_inbound_terminal_requests_manage_internal_processes();
            acp_inbound_requests_wait_for_forwarded_host_responses_before_method_not_found();
            acp_inbound_requests_time_out_to_method_not_found_when_host_never_answers();
            acp_termination_sends_cancel_before_sigterm_and_closes_session_early();
            acp_termination_sigkills_after_grace_when_agent_ignores_sigterm();
            loopback_tls_transport_survives_concurrent_handshakes_without_panicking();
            loopback_tls_endpoint_read_survives_competing_drain_and_peer_drop();
            javascript_net_socket_wait_connect_reports_tcp_socket_info();
            javascript_net_socket_read_and_socket_options_work_for_tcp_sockets();
            javascript_net_upgrade_socket_aliases_use_tcp_socket_state();
            javascript_dgram_address_and_buffer_size_sync_rpcs_work();
            javascript_tls_client_upgrade_query_and_cipher_list_work();
            javascript_tls_server_client_hello_and_server_upgrade_work();
            javascript_net_server_accept_returns_timeout_then_pending_connection();
            javascript_kernel_stdin_reads_buffered_input_and_reports_timeout_and_eof();
            javascript_sync_rpc_pty_set_raw_mode_toggles_kernel_tty_discipline();
            dispose_vm_removes_per_vm_javascript_import_cache_directory();
            execution_dispose_vm_race_skips_stale_process_events_without_panicking();
            execution_javascript_sync_rpc_handler_ignores_stale_vm_and_process_races();
            execution_poll_event_smoke_skips_queued_stale_process_envelopes_after_dispose();
            execution_poll_event_concurrent_dispose_logs_stale_process_event();
            filesystem_requests_ignore_stale_vm_and_process_races();
            get_zombie_timer_count_reports_kernel_state_before_and_after_waitpid();
            parse_signal_accepts_full_guest_signal_table();
            runtime_child_liveness_only_tracks_owned_children();
            authenticated_connection_id_returns_error_for_unexpected_response();
            opened_session_id_returns_error_for_unexpected_response();
            created_vm_id_returns_error_for_unexpected_response();
            configure_vm_instantiates_memory_mounts_through_the_plugin_registry();
            configure_vm_applies_read_only_mount_wrappers();
            configure_vm_instantiates_host_dir_mounts_through_the_plugin_registry();
            configure_vm_js_bridge_mount_dispatches_filesystem_calls_via_sidecar_requests();
            configure_vm_js_bridge_mount_maps_callback_errors_to_errno_codes();
            configure_vm_instantiates_sandbox_agent_mounts_through_the_plugin_registry();
            configure_vm_instantiates_s3_mounts_through_the_plugin_registry();
            bridge_permissions_map_symlink_operations_to_symlink_access();
            parse_resource_limits_reads_filesystem_limits();
            create_vm_applies_filesystem_permission_descriptors_to_kernel_access();
            create_vm_without_permissions_defaults_to_static_deny_all();
            configure_vm_rollback_restore_failure_falls_back_to_static_deny_all();
            toolkit_registration_rollback_restore_failure_keeps_registry_consistent();
            create_vm_rejects_permission_rules_with_empty_operations();
            configure_vm_rejects_permission_rules_with_empty_paths_or_patterns();
            configure_vm_mounts_bypass_guest_fs_write_policy();
            guest_filesystem_link_and_truncate_preserve_hard_link_semantics();
            configure_vm_sensitive_mounts_bypass_guest_fs_mount_sensitive_policy();
            guest_mount_request_default_deny_rejects_without_changing_operator_mounts();
            scoped_host_filesystem_unscoped_target_requires_exact_guest_root_prefix();
            scoped_host_filesystem_realpath_preserves_paths_outside_guest_root();
            host_filesystem_realpath_fails_closed_on_circular_symlinks();
            configure_vm_host_dir_plugin_fails_closed_for_escape_symlinks();
            execute_starts_python_runtime_instead_of_rejecting_it();
            command_resolution_executes_wasm_command_from_sidecar_path();
            wasm_fd_write_sync_rpc_keeps_stdout_isolated_per_vm();
            wasm_fd_write_sync_rpc_routes_stdout_into_kernel_pty();
            javascript_child_process_searches_path_for_mounted_wasm_commands();
            javascript_child_process_spawns_path_resolved_tool_commands();
            javascript_child_process_resolves_path_resolved_tool_commands_as_tools();
            javascript_child_process_spawns_internal_tool_command_paths();
            javascript_child_process_resolves_internal_tool_command_paths_as_tools();
            tools_register_toolkit_rejects_duplicate_names_without_replacing_existing_toolkit();
            tools_javascript_child_process_denies_tool_invocation_without_permission();
            tools_javascript_child_process_invokes_tool_with_matching_permission();
            tools_javascript_child_process_rejects_invalid_json_file_input_before_dispatch();
            tools_javascript_child_process_accepts_valid_json_input();
            command_resolution_executes_javascript_path_command_with_sidecar_mappings();
            command_resolution_executes_node_eval_command();
            command_resolution_rejects_unknown_command();
            python_vfs_rpc_requests_proxy_into_the_vm_kernel_filesystem();
            javascript_sync_rpc_requests_proxy_into_the_vm_kernel_filesystem();
            python_vfs_rpc_paths_are_scoped_to_workspace_root();
            javascript_fs_sync_rpc_resolves_proc_self_against_the_kernel_process();
            javascript_fd_and_stream_rpc_requests_proxy_into_the_vm_kernel_filesystem();
            javascript_fs_promises_batch_requests_before_waiting_on_sidecar_responses();
            javascript_crypto_basic_sync_rpcs_round_trip_through_sidecar();
            javascript_crypto_advanced_sync_rpcs_round_trip_through_sidecar();
            javascript_sqlite_sync_rpcs_round_trip_and_persist_vm_files();
            javascript_sqlite_builtin_round_trips_through_sidecar_sync_rpc();
            javascript_net_rpc_connects_over_vm_loopback();
            javascript_dgram_rpc_sends_and_receives_vm_loopback_packets();
            javascript_dns_rpc_resolves_localhost();
            javascript_network_ssrf_protection_blocks_private_dns_and_unowned_loopback_targets();
            javascript_dns_rpc_honors_vm_dns_overrides_and_net_connect_uses_sidecar_dns();
            javascript_network_dns_resolve_supports_standard_rrtypes();
            javascript_network_permission_callbacks_fire_for_dns_lookup_connect_and_listen();
            javascript_network_permission_denials_surface_eacces_to_guest_code();
            javascript_tls_rpc_connects_and_serves_over_guest_net();
            javascript_http_listen_and_close_registers_server();
            javascript_http_respond_records_pending_response();
            javascript_http2_listen_connect_request_and_respond_round_trip();
            javascript_http2_settings_pause_push_and_file_response_surfaces_work();
            javascript_http2_secure_listen_connect_request_and_respond_round_trip();
            javascript_http2_server_respond_records_pending_response();
            javascript_http_rpc_requests_gets_and_serves_over_guest_net();
            javascript_https_rpc_requests_and_serves_over_guest_tls();
            javascript_net_rpc_listens_accepts_connections_and_reports_listener_state();
            javascript_net_rpc_reports_connection_counts_and_enforces_backlog();
            javascript_network_bind_policy_restricts_hosts_and_ports();
            javascript_network_bind_policy_can_allow_privileged_guest_ports();
            javascript_network_listeners_are_isolated_per_vm_even_with_same_guest_port();
            javascript_net_rpc_listens_and_connects_over_unix_domain_sockets();
            javascript_child_process_rpc_spawns_nested_node_processes_inside_vm_kernel();
            javascript_child_process_rpc_preserves_nested_sigchld_registrations();
            javascript_child_process_poll_reports_echild_when_child_disappears_after_drain();
            javascript_child_process_internal_bootstrap_env_is_allowlisted();
            javascript_net_poll_clamps_guest_wait_to_sidecar_ceiling();
            javascript_net_poll_timeout_does_not_block_concurrent_vm_dispose();
        }

        #[test]
        fn service_suite_javascript_network_dns_javascript_net_poll() {
            run_service_suite();
        }
    }
}

pub use crate::service::{DispatchResult, NativeSidecar, SidecarError};
