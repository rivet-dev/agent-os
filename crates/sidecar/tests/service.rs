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
        #[path = "/home/nathan/a5/crates/bridge/tests/support.rs"]
        mod bridge_support;

        use super::*;
        use crate::bridge::{bridge_permissions, HostFilesystem, ScopedHostFilesystem};
        use crate::plugins::s3::test_support::MockS3Server;
        use crate::plugins::sandbox_agent::test_support::MockSandboxAgentServer;
        use crate::protocol::VmCreatedResponse;
        use crate::protocol::{
            AuthenticateRequest, BootstrapRootFilesystemRequest, CloseStdinRequest,
            ConfigureVmRequest, CreateVmRequest, DisposeReason, FsPermissionRule,
            FsPermissionRuleSet, FsPermissionScope, GetZombieTimerCountRequest, GuestRuntimeKind,
            MountDescriptor, MountPluginDescriptor, OpenSessionRequest, OwnershipScope,
            PatternPermissionRule, PatternPermissionRuleSet, PatternPermissionScope,
            PermissionMode, PermissionsPolicy, RequestFrame, RequestPayload, ResponsePayload,
            RootFilesystemEntry, RootFilesystemEntryKind, SidecarPlacement, SidecarRequestFrame,
            SidecarRequestPayload, SidecarResponsePayload, WriteStdinRequest,
        };
        use crate::state::{ToolExecution, VM_DNS_SERVERS_METADATA_KEY};
        use agent_os_bridge::{FileKind, SymlinkRequest};
        use agent_os_execution::PythonVfsRpcMethod;
        use agent_os_kernel::command_registry::CommandDriver;
        use agent_os_kernel::kernel::{KernelVmConfig, SpawnOptions};
        use agent_os_kernel::mount_table::{MountEntry, MountTable};
        use agent_os_kernel::permissions::{FsAccessRequest, FsOperation, Permissions};
        use agent_os_kernel::vfs::{
            MemoryFileSystem, VfsError, VirtualDirEntry, VirtualFileSystem, VirtualStat,
        };
        use base64::Engine;
        use bridge_support::RecordingBridge;
        use rustls::client::danger::{
            HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
        };
        use rustls::crypto::aws_lc_rs;
        use rustls::pki_types::{CertificateDer, ServerName};
        use rustls::{
            ClientConfig, ClientConnection, DigitallySignedStruct, RootCertStore, ServerConfig,
            ServerConnection, SignatureScheme,
        };
        use serde_json::{json, Value};
        use socket2::SockRef;
        use std::collections::BTreeMap;
        use std::fs;
        use std::io::{BufReader, Read, Write};
        use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
        use std::path::{Path, PathBuf};
        use std::process::Command;
        use std::sync::{Arc, Mutex};
        use std::thread;
        use std::time::{SystemTime, UNIX_EPOCH};

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

        fn request(
            request_id: agent_os_sidecar::protocol::RequestId,
            ownership: OwnershipScope,
            payload: RequestPayload,
        ) -> RequestFrame {
            RequestFrame::new(request_id, ownership, payload)
        }

        fn create_test_sidecar() -> NativeSidecar<RecordingBridge> {
            NativeSidecar::with_config(
                RecordingBridge::default(),
                NativeSidecarConfig {
                    sidecar_id: String::from("sidecar-test"),
                    compile_cache_root: Some(
                        std::env::temp_dir().join("agent-os-sidecar-test-cache"),
                    ),
                    expected_auth_token: Some(String::from(TEST_AUTH_TOKEN)),
                    ..NativeSidecarConfig::default()
                },
            )
            .expect("create sidecar")
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

        fn empty_permissions_policy() -> PermissionsPolicy {
            PermissionsPolicy {
                fs: None,
                network: None,
                child_process: None,
                env: None,
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
                    "env" => policy.env = Some(PatternPermissionScope::Mode(mode.clone())),
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
                    _ if capability.starts_with("env.") => {
                        append_pattern_rule(
                            &mut policy.env,
                            capability.trim_start_matches("env."),
                            mode.clone(),
                        );
                    }
                    _ => panic!("unsupported test capability {capability}"),
                }
            }

            policy
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
                            paths: Vec::new(),
                        }],
                    })
                }
                FsPermissionScope::Rules(mut rules) => {
                    rules.rules.push(FsPermissionRule {
                        mode,
                        operations: vec![operation.to_owned()],
                        paths: Vec::new(),
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
                            patterns: Vec::new(),
                        }],
                    })
                }
                PatternPermissionScope::Rules(mut rules) => {
                    rules.rules.push(PatternPermissionRule {
                        mode,
                        operations: vec![operation.to_owned()],
                        patterns: Vec::new(),
                    });
                    PatternPermissionScope::Rules(rules)
                }
            });
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
                    let vm = sidecar.vms.get(vm_id).expect("active vm");
                    vm.active_processes
                        .get(process_id)
                        .map(|process| {
                            process
                                .execution
                                .poll_event_blocking(Duration::from_secs(5))
                                .expect("poll process event")
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
                    &Map::new(),
                    &Map::new(),
                ),
            );
            session_id
        }

        #[test]
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

        #[test]
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

        #[test]
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

        #[test]
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
                let stream = socket.stream.lock().expect("lock tcp socket");
                assert!(stream.nodelay().expect("read TCP_NODELAY"));
                assert!(SockRef::from(&*stream)
                    .keepalive()
                    .expect("read SO_KEEPALIVE"));
            }

            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-net-read",
                JavascriptSyncRpcRequest {
                    id: 60,
                    method: String::from("net.write"),
                    args: vec![json!(server_socket_id.clone()), json!("ping")],
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

        #[test]
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

        #[test]
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

        #[test]
        fn javascript_tls_client_upgrade_query_and_cipher_list_work() {
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
                    args: vec![json!(socket_id.clone()), json!("ping")],
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

        #[test]
        fn javascript_tls_server_client_hello_and_server_upgrade_work() {
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
            let host_port = {
                let vm = sidecar.vms.get(&vm_id).expect("javascript vm");
                vm.active_processes
                    .get("proc-js-tls-server")
                    .and_then(|process| process.tcp_listeners.get(&server_id))
                    .expect("sidecar tcp listener")
                    .local_addr()
                    .port()
            };

            let client = thread::spawn(move || {
                let config = tls_test_client_config(false, &["h2", "http/1.1"]);
                let stream =
                    TcpStream::connect(("127.0.0.1", host_port)).expect("connect to TLS server");
                let server_name = ServerName::try_from("localhost").expect("TLS test server name");
                let mut stream = rustls::StreamOwned::new(
                    ClientConnection::new(config, server_name)
                        .expect("create TLS client connection"),
                    stream,
                );
                while stream.conn.is_handshaking() {
                    stream
                        .conn
                        .complete_io(&mut stream.sock)
                        .expect("complete TLS client handshake");
                }
                assert_eq!(stream.conn.alpn_protocol(), Some(b"h2".as_slice()));
                stream.write_all(b"ping").expect("write TLS client payload");
                stream.flush().expect("flush TLS client payload");
                let mut response = [0_u8; 4];
                stream
                    .read_exact(&mut response)
                    .expect("read TLS server response");
                assert_eq!(&response, b"pong");
            });

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

            let protocol = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-tls-server",
                JavascriptSyncRpcRequest {
                    id: 82,
                    method: String::from("net.socket_tls_query"),
                    args: vec![json!(socket_id.clone()), json!("getProtocol")],
                },
            )
            .expect("query TLS protocol");
            let protocol: Value =
                serde_json::from_str(protocol.as_str().expect("TLS protocol JSON"))
                    .expect("parse TLS protocol");
            assert!(
                protocol == Value::String(String::from("TLSv1.3"))
                    || protocol == Value::String(String::from("TLSv1.2")),
                "protocol: {protocol}"
            );

            let payload = (0..30)
                .find_map(|attempt| {
                    let value = call_javascript_sync_rpc(
                        &mut sidecar,
                        &vm_id,
                        "proc-js-tls-server",
                        JavascriptSyncRpcRequest {
                            id: 90 + attempt,
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

            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-tls-server",
                JavascriptSyncRpcRequest {
                    id: 120,
                    method: String::from("net.write"),
                    args: vec![json!(socket_id.clone()), json!("pong")],
                },
            )
            .expect("write TLS server payload");

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
                    method: String::from("net.server_close"),
                    args: vec![json!(server_id)],
                },
            )
            .expect("close TLS listener");

            client.join().expect("join TLS client");
        }

        #[test]
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
            let host_port = {
                let vm = sidecar.vms.get(&vm_id).expect("javascript vm");
                vm.active_processes
                    .get("proc-js-server-accept")
                    .and_then(|process| process.tcp_listeners.get(&server_id))
                    .expect("sidecar tcp listener")
                    .local_addr()
                    .port()
            };

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

            let client = thread::spawn(move || {
                let _stream = TcpStream::connect(("127.0.0.1", host_port))
                    .expect("connect to sidecar listener");
            });

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

            client.join().expect("join tcp client");
        }

        #[test]
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

        #[test]
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

        #[test]
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

        #[test]
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

        #[test]
        fn parse_signal_only_accepts_whitelisted_guest_signals() {
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
            assert!(parse_signal("SIGUSR1").is_err());
        }

        #[test]
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

        #[test]
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

        #[test]
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

        #[test]
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

        #[test]
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

        #[test]
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

        #[test]
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

        #[test]
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

        #[test]
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
                                mtime_ms: 0,
                                ctime_ms: 0,
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

        #[test]
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

        #[test]
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

        #[test]
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

        #[test]
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

        #[test]
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

        #[test]
        fn configure_vm_mounts_require_fs_write_permission() {
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
                ResponsePayload::Rejected(rejected) => {
                    assert_eq!(rejected.code, "kernel_error");
                    assert!(
                        rejected.message.contains("EACCES"),
                        "unexpected error: {}",
                        rejected.message
                    );
                }
                other => panic!("expected rejected response, got {other:?}"),
            }
        }

        #[test]
        fn configure_vm_sensitive_mounts_require_fs_mount_sensitive_permission() {
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
                ResponsePayload::Rejected(rejected) => {
                    assert_eq!(rejected.code, "kernel_error");
                    assert!(
                        rejected.message.contains("EACCES"),
                        "unexpected error: {}",
                        rejected.message
                    );
                    assert!(
                        rejected.message.contains("fs.mount_sensitive"),
                        "unexpected error: {}",
                        rejected.message
                    );
                }
                other => panic!("expected rejected response, got {other:?}"),
            }
        }

        #[test]
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

        #[test]
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

        #[test]
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

        #[test]
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

        #[test]
        fn execute_starts_python_runtime_instead_of_rejecting_it() {
            assert_node_available();

            let cache_root = temp_dir("agent-os-sidecar-python-cache");

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

        #[test]
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

        #[test]
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

        #[test]
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

        #[test]
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

        #[test]
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
      await new Promise(() => {});
    },
  };
}
"#,
            );
            write_fixture(
                &pyodide_dir.join("pyodide-lock.json"),
                "{\"packages\":[]}\n",
            );

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

            sidecar
                .handle_python_vfs_rpc_request(
                    &vm_id,
                    "proc-python-vfs",
                    PythonVfsRpcRequest {
                        id: 1,
                        method: PythonVfsRpcMethod::Mkdir,
                        path: String::from("/workspace"),
                        content_base64: None,
                        recursive: false,
                    },
                )
                .expect("handle python mkdir rpc");
            sidecar
                .handle_python_vfs_rpc_request(
                    &vm_id,
                    "proc-python-vfs",
                    PythonVfsRpcRequest {
                        id: 2,
                        method: PythonVfsRpcMethod::Write,
                        path: String::from("/workspace/note.txt"),
                        content_base64: Some(String::from("aGVsbG8gZnJvbSBzaWRlY2FyIHJwYw==")),
                        recursive: false,
                    },
                )
                .expect("handle python write rpc");

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
            let _ = signal_runtime_process(process.execution.child_pid(), SIGTERM);
        }

        #[test]
        #[ignore = "V8 sidecar JS filesystem integration is flaky in this harness; execution-layer tests cover the V8 bridge path"]
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
                    ),
                );
            }

            let mut saw_stdout = false;
            for _ in 0..16 {
                let event = {
                    let vm = sidecar.vms.get(&vm_id).expect("javascript vm");
                    let process = vm
                        .active_processes
                        .get("proc-js-sync")
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
            let _ = signal_runtime_process(process.execution.child_pid(), SIGTERM);
        }

        #[test]
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

        #[test]
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

            let process = kernel
                .spawn_process(
                    JAVASCRIPT_COMMAND,
                    Vec::new(),
                    SpawnOptions {
                        requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                        ..SpawnOptions::default()
                    },
                )
                .expect("spawn javascript kernel process");

            let link = service_javascript_fs_sync_rpc(
                &mut kernel,
                process.pid(),
                &JavascriptSyncRpcRequest {
                    id: 1,
                    method: String::from("fs.readlinkSync"),
                    args: vec![json!("/proc/self")],
                },
            )
            .expect("resolve /proc/self");
            assert_eq!(link, Value::String(format!("/proc/{}", process.pid())));

            let entries = service_javascript_fs_sync_rpc(
                &mut kernel,
                process.pid(),
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

            process.finish(0);
            kernel
                .waitpid(process.pid())
                .expect("wait javascript process");
        }

        #[test]
        #[ignore = "V8 sidecar JS fd/stream integration is flaky in this harness; execution-layer tests cover the V8 bridge path"]
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
try {
  fs.watch("/rpc/input.txt");
} catch (error) {
  watchCode = error.code;
}
try {
  fs.watchFile("/rpc/input.txt", () => {});
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
                    ),
                );
            }

            let mut stdout = String::new();
            let mut stderr = String::new();
            let mut exit_code = None;
            for _ in 0..64 {
                let next_event = {
                    let vm = sidecar.vms.get(&vm_id).expect("javascript vm");
                    vm.active_processes
                        .get("proc-js-fd")
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
                stdout.contains("\"watchCode\":\"ERR_AGENT_OS_FS_WATCH_UNAVAILABLE\""),
                "stdout: {stdout}"
            );
            assert!(
                stdout.contains("\"watchFileCode\":\"ERR_AGENT_OS_FS_WATCH_UNAVAILABLE\""),
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

        #[test]
        #[ignore = "V8 sidecar JS fs/promises integration is flaky in this harness; execution-layer tests cover the V8 bridge path"]
        fn javascript_fs_promises_rpc_requests_proxy_into_the_vm_kernel_filesystem() {
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

await fs.writeFile("/rpc/note.txt", "hello from sidecar promises rpc");
const contents = await fs.readFile("/rpc/note.txt", "utf8");
console.log(contents);
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

            let mut saw_stdout = false;
            for _ in 0..4 {
                let event = {
                    let vm = sidecar.vms.get(&vm_id).expect("javascript vm");
                    let process = vm
                        .active_processes
                        .get("proc-js-promises")
                        .expect("javascript process should be tracked");
                    process
                        .execution
                        .poll_event_blocking(Duration::from_secs(5))
                        .expect("poll javascript promises rpc event")
                        .expect("javascript promises rpc event")
                };

                if let ActiveExecutionEvent::Stdout(chunk) = &event {
                    let stdout = String::from_utf8(chunk.clone()).expect("stdout utf8");
                    if stdout.contains("hello from sidecar promises rpc") {
                        saw_stdout = true;
                        break;
                    }
                }

                sidecar
                    .handle_execution_event(&vm_id, "proc-js-promises", event)
                    .expect("handle javascript promises rpc event");
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
            assert_eq!(content, "hello from sidecar promises rpc");
            assert!(
                saw_stdout,
                "expected guest stdout after fs.promises round-trip"
            );

            let process = {
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                vm.active_processes
                    .remove("proc-js-promises")
                    .expect("remove fake javascript process")
            };
            let _ = signal_runtime_process(process.execution.child_pid(), SIGTERM);
        }

        #[test]
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

        #[test]
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

        #[test]
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

        #[test]
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
import { DatabaseSync } from "node:sqlite";

const db = new DatabaseSync("/workspace/sqlite-builtins.db");
db.exec("CREATE TABLE items (id INTEGER PRIMARY KEY, payload BLOB NOT NULL)");
const insert = db.prepare("INSERT INTO items(id, payload) VALUES (?, ?)");
const insertResult = insert.run(9007199254740993n, new Uint8Array([7, 8, 9]));
if (insertResult.changes !== 1) {
  throw new Error(`unexpected insert result: ${JSON.stringify(insertResult)}`);
}

const select = db.prepare("SELECT id, payload FROM items");
select.setReadBigInts(true);
const row = select.get();
if (typeof row.id !== "bigint" || row.id !== 9007199254740993n) {
  throw new Error(`unexpected bigint row id: ${String(row.id)}`);
}
if (!Buffer.isBuffer(row.payload) || row.payload.length !== 3 || row.payload[1] !== 8) {
  throw new Error(`unexpected blob payload: ${JSON.stringify(row.payload)}`);
}
db.close();

const reopened = new DatabaseSync("/workspace/sqlite-builtins.db");
const verify = reopened.prepare("SELECT COUNT(*) AS count FROM items");
const count = verify.get();
if (count.count !== 1) {
  throw new Error(`unexpected persisted count: ${JSON.stringify(count)}`);
}
reopened.close();
console.log("sqlite-ok");
"#,
            );

            let (_stdout, stderr, exit_code) = run_javascript_entry(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-sqlite-builtins",
                "[\"assert\",\"buffer\",\"console\",\"crypto\",\"events\",\"fs\",\"path\",\"querystring\",\"sqlite\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
            );

            assert_eq!(exit_code, Some(0), "stderr: {stderr}");
            assert!(stderr.trim().is_empty(), "stderr: {stderr}");
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

        #[test]
        #[ignore = "V8 sidecar TCP integration is flaky in this harness; execution-layer tests cover the V8 bridge path"]
        fn javascript_net_rpc_connects_to_host_tcp_server() {
            assert_node_available();

            let listener = TcpListener::bind("127.0.0.1:0").expect("bind tcp listener");
            let port = listener.local_addr().expect("listener address").port();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept tcp client");
                let mut received = [0_u8; 4];
                stream
                    .read_exact(&mut received)
                    .expect("read client payload");
                assert_eq!(
                    String::from_utf8(received.to_vec()).expect("client utf8"),
                    "ping"
                );
                stream.write_all(b"pong").expect("write server payload");
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
            let cwd = temp_dir("agent-os-sidecar-js-net-rpc-cwd");
            write_fixture(
                &cwd.join("entry.mjs"),
                &format!(
                    r#"
import net from "node:net";

const socket = net.createConnection({{ host: "127.0.0.1", port: {port} }});
let data = "";
socket.setEncoding("utf8");
socket.on("connect", () => {{
  socket.write("ping");
}});
socket.on("data", (chunk) => {{
  data += chunk;
}});
socket.on("error", (error) => {{
  console.error(error.stack ?? error.message);
  process.exit(1);
}});
socket.on("close", (hadError) => {{
  console.log(JSON.stringify({{
    data,
    hadError,
    remoteAddress: socket.remoteAddress,
    remotePort: socket.remotePort,
    localPort: socket.localPort,
  }}));
  process.exit(0);
}});
"#,
                ),
            );

            let (stdout, stderr, exit_code) = run_javascript_entry(
                &mut sidecar,
                &vm_id,
                &cwd,
                "proc-js-net",
                "[\"assert\",\"buffer\",\"console\",\"crypto\",\"events\",\"fs\",\"net\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
            );

            server.join().expect("join tcp server");
            assert_eq!(exit_code, Some(0), "stderr: {stderr}");
            assert!(
                stdout.contains("\"remoteAddress\":\"127.0.0.1\""),
                "stdout: {stdout}"
            );
            assert!(
                stdout.contains(&format!("\"remotePort\":{port}")),
                "stdout: {stdout}"
            );
        }

        #[test]
        #[ignore = "V8 sidecar UDP integration is flaky in this harness; execution-layer tests cover the V8 bridge path"]
        fn javascript_dgram_rpc_sends_and_receives_host_udp_packets() {
            assert_node_available();

            let listener = UdpSocket::bind("127.0.0.1:0").expect("bind udp listener");
            let port = listener.local_addr().expect("listener address").port();
            let server = thread::spawn(move || {
                let mut buffer = [0_u8; 64 * 1024];
                let (bytes_read, remote_addr) =
                    listener.recv_from(&mut buffer).expect("recv packet");
                assert_eq!(
                    String::from_utf8(buffer[..bytes_read].to_vec()).expect("udp payload utf8"),
                    "ping"
                );
                listener
                    .send_to(b"pong", remote_addr)
                    .expect("send udp response");
            });

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
                &format!(
                    r#"
import dgram from "node:dgram";

const socket = dgram.createSocket("udp4");
const summary = await new Promise((resolve) => {{
socket.on("error", (error) => {{
  console.error(error.stack ?? error.message);
  process.exit(1);
}});
socket.on("message", (message, rinfo) => {{
  const address = socket.address();
  socket.close(() => {{
    resolve({{
      address,
      message: message.toString("utf8"),
      rinfo,
    }});
  }});
}});
socket.bind(0, "127.0.0.1", () => {{
  socket.send("ping", {port}, "127.0.0.1");
}});
}});

console.log(JSON.stringify(summary));
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
                        "[\"assert\",\"buffer\",\"console\",\"crypto\",\"dgram\",\"events\",\"fs\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
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
                    String::from("proc-js-dgram"),
                    ActiveProcess::new(
                        kernel_handle.pid(),
                        kernel_handle,
                        GuestRuntimeKind::JavaScript,
                        ActiveExecution::Javascript(execution),
                    ),
                );
            }

            let mut stdout = String::new();
            let mut stderr = String::new();
            let mut exit_code = None;
            for _ in 0..64 {
                let next_event = {
                    let vm = sidecar.vms.get(&vm_id).expect("javascript vm");
                    vm.active_processes
                        .get("proc-js-dgram")
                        .map(|process| {
                            process
                                .execution
                                .poll_event_blocking(Duration::from_secs(5))
                                .expect("poll javascript dgram rpc event")
                        })
                        .flatten()
                };
                let Some(event) = next_event else {
                    if exit_code.is_some() {
                        break;
                    }
                    panic!("javascript dgram process disappeared before exit");
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
                    .handle_execution_event(&vm_id, "proc-js-dgram", event)
                    .expect("handle javascript dgram rpc event");
            }

            server.join().expect("join udp server");
            assert_eq!(exit_code, Some(0), "stderr: {stderr}");
            assert!(stdout.contains("\"message\":\"pong\""), "stdout: {stdout}");
            assert!(
                stdout.contains("\"address\":{\"address\":\"127.0.0.1\""),
                "stdout: {stdout}"
            );
            assert!(
                stdout.contains(&format!("\"port\":{port}")),
                "stdout: {stdout}"
            );
        }

        #[test]
        #[ignore = "V8 sidecar DNS integration is flaky in this harness; execution-layer tests cover the V8 bridge path"]
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
                    ),
                );
            }

            let mut stdout = String::new();
            let mut stderr = String::new();
            let mut exit_code = None;
            for _ in 0..64 {
                let next_event = {
                    let vm = sidecar.vms.get(&vm_id).expect("javascript vm");
                    vm.active_processes
                        .get("proc-js-dns")
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

        #[test]
        #[ignore = "V8 sidecar network SSRF integration is flaky in this harness; execution-layer tests cover the V8 bridge path"]
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
  const socket = net.createConnection({{ host: "metadata.test", port: 80 }});
  socket.on("connect", () => {{
    socket.destroy();
    resolve({{ unexpected: true }});
  }});
  socket.on("error", (error) => {{
    resolve({{ code: error.code ?? null, message: error.message }});
  }});
}});

const loopbackConnect = await new Promise((resolve) => {{
  const socket = net.createConnection({{ host: "127.0.0.1", port: {loopback_port} }});
  socket.on("connect", () => {{
    socket.destroy();
    resolve({{ unexpected: true }});
  }});
  socket.on("error", (error) => {{
    resolve({{ code: error.code ?? null, message: error.message }});
  }});
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
                    ),
                );
            }

            let mut stdout = String::new();
            let mut stderr = String::new();
            let mut exit_code = None;
            for _ in 0..64 {
                let next_event = {
                    let vm = sidecar.vms.get(&vm_id).expect("javascript vm");
                    vm.active_processes
                        .get("proc-js-ssrf-protection")
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

        #[test]
        #[ignore = "V8 sidecar DNS/network integration can exhaust heap in this harness; execution-layer tests cover the V8 bridge path"]
        fn javascript_dns_rpc_honors_vm_dns_overrides_and_net_connect_uses_sidecar_dns() {
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
                stream.write_all(b"pong").expect("write server payload");
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
                &format!(
                    r#"
import dns from "node:dns";
import net from "node:net";

const lookup = await dns.promises.lookup("example.test", {{ family: 4 }});
const resolved = await dns.promises.resolve4("example.test");
const socketSummary = await new Promise((resolve, reject) => {{
  const socket = net.createConnection({{ host: "example.test", port: {port} }});
  let data = "";
  socket.setEncoding("utf8");
  socket.on("connect", () => {{
    socket.end("ping");
  }});
  socket.on("data", (chunk) => {{
    data += chunk;
  }});
  socket.on("error", reject);
  socket.on("close", (hadError) => {{
    resolve({{
      data,
      hadError,
      remoteAddress: socket.remoteAddress,
      remotePort: socket.remotePort,
    }});
  }});
}});

console.log(JSON.stringify({{ lookup, resolved, socketSummary }}));
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
                    String::from("proc-js-dns-override"),
                    ActiveProcess::new(
                        kernel_handle.pid(),
                        kernel_handle,
                        GuestRuntimeKind::JavaScript,
                        ActiveExecution::Javascript(execution),
                    ),
                );
            }

            let mut stdout = String::new();
            let mut stderr = String::new();
            let mut exit_code = None;
            for _ in 0..64 {
                let next_event = {
                    let vm = sidecar.vms.get(&vm_id).expect("javascript vm");
                    vm.active_processes
                        .get("proc-js-dns-override")
                        .map(|process| {
                            process
                                .execution
                                .poll_event_blocking(Duration::from_secs(5))
                                .expect("poll javascript dns override rpc event")
                        })
                        .flatten()
                };
                let Some(event) = next_event else {
                    if exit_code.is_some() {
                        break;
                    }
                    panic!("javascript dns override process disappeared before exit");
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
                    .handle_execution_event(&vm_id, "proc-js-dns-override", event)
                    .expect("handle javascript dns override rpc event");
            }

            server.join().expect("join tcp server");
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
                Value::from(u64::from(port))
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
                "expected dns events for lookup, resolve4, and net.connect: {dns_events:?}"
            );
            for event in dns_events {
                assert_eq!(event.fields["source"], "override");
                assert_eq!(event.fields["addresses"], "127.0.0.1");
                assert_eq!(event.fields["resolver_count"], "1");
                assert_eq!(event.fields["resolvers"], "203.0.113.53:5353");
            }
        }

        #[test]
        #[ignore = "V8 sidecar network permission integration is flaky in this harness; execution-layer tests cover the V8 bridge path"]
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

        #[test]
        #[ignore = "V8 sidecar network denial integration is flaky in this harness; execution-layer tests cover the V8 bridge path"]
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
const listenResult = (() => {
  const server = net.createServer();
  try {
    server.listen(0, "127.0.0.1");
    return { unexpected: true };
  } catch (error) {
    return { code: error.code ?? null, message: error.message };
  }
})();
const connectResult = await new Promise((resolve) => {
  const socket = net.createConnection({ host: "127.0.0.1", port: 43111 });
  socket.on("connect", () => resolve({ unexpected: true }));
  socket.on("error", (error) => {
    resolve({ code: error.code ?? null, message: error.message });
  });
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

        #[test]
        #[ignore = "V8 sidecar TLS integration is flaky in this harness; execution-layer tests cover the V8 bridge path"]
        fn javascript_tls_rpc_connects_and_serves_over_guest_net() {
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
                    ),
                );
            }

            let mut stdout = String::new();
            let mut stderr = String::new();
            let mut exit_code = None;
            for _ in 0..192 {
                let next_event = {
                    let vm = sidecar.vms.get(&vm_id).expect("javascript vm");
                    vm.active_processes
                        .get("proc-js-tls")
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

        #[test]
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

        #[test]
        fn javascript_http_request_uses_outbound_adapter() {
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
            let cwd = temp_dir("agent-os-sidecar-http-request");
            write_fixture(&cwd.join("entry.mjs"), "");
            start_fake_javascript_process(&mut sidecar, &vm_id, &cwd, "proc-js-http-request", "[]");

            let listener = TcpListener::bind("127.0.0.1:0").expect("bind host http listener");
            let port = listener.local_addr().expect("listener addr").port();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept http request");
                let mut buffer = [0_u8; 4096];
                let _ = stream.read(&mut buffer).expect("read http request");
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 4\r\n\r\npong",
                    )
                    .expect("write http response");
                let _ = stream.flush();
            });

            let response = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-http-request",
                JavascriptSyncRpcRequest {
                    id: 3,
                    method: String::from("net.http_request"),
                    args: vec![
                        Value::String(format!("http://127.0.0.1:{port}/health")),
                        Value::String(String::from(
                            "{\"method\":\"GET\",\"headers\":{\"accept\":\"text/plain\"}}",
                        )),
                    ],
                },
            )
            .expect("outbound http request");
            server.join().expect("join http server");

            let payload: Value =
                serde_json::from_str(response.as_str().expect("response payload string"))
                    .expect("parse response payload");
            assert_eq!(payload["status"], json!(200));
            assert_eq!(payload["statusText"], Value::String(String::from("OK")));
            let body = payload["body"].as_str().expect("base64 body");
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(body)
                .expect("decode response body");
            assert_eq!(String::from_utf8(decoded).expect("utf8 response"), "pong");
        }

        #[test]
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

        #[test]
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

        #[test]
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

        #[test]
        #[ignore = "V8 sidecar HTTP integration is flaky in this harness; focused HTTP bridge tests cover the sidecar path"]
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
                        "[\"assert\",\"buffer\",\"console\",\"crypto\",\"events\",\"fs\",\"http\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
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
                    String::from("proc-js-http"),
                    ActiveProcess::new(
                        kernel_handle.pid(),
                        kernel_handle,
                        GuestRuntimeKind::JavaScript,
                        ActiveExecution::Javascript(execution),
                    ),
                );
            }

            let mut stdout = String::new();
            let mut stderr = String::new();
            let mut exit_code = None;
            for _ in 0..192 {
                let next_event = {
                    let vm = sidecar.vms.get(&vm_id).expect("javascript vm");
                    vm.active_processes
                        .get("proc-js-http")
                        .map(|process| {
                            process
                                .execution
                                .poll_event_blocking(Duration::from_secs(5))
                                .expect("poll javascript http rpc event")
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
                    .handle_execution_event(&vm_id, "proc-js-http", event)
                    .expect("handle javascript http rpc event");
            }

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

        #[test]
        #[ignore = "V8 sidecar HTTPS integration is flaky in this harness; execution-layer tests cover the V8 bridge path"]
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
                        "[\"assert\",\"buffer\",\"console\",\"crypto\",\"events\",\"fs\",\"https\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
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
                    String::from("proc-js-https"),
                    ActiveProcess::new(
                        kernel_handle.pid(),
                        kernel_handle,
                        GuestRuntimeKind::JavaScript,
                        ActiveExecution::Javascript(execution),
                    ),
                );
            }

            let mut stdout = String::new();
            let mut stderr = String::new();
            let mut exit_code = None;
            for _ in 0..192 {
                let next_event = {
                    let vm = sidecar.vms.get(&vm_id).expect("javascript vm");
                    vm.active_processes
                        .get("proc-js-https")
                        .map(|process| {
                            process
                                .execution
                                .poll_event_blocking(Duration::from_secs(5))
                                .expect("poll javascript https rpc event")
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
                    .handle_execution_event(&vm_id, "proc-js-https", event)
                    .expect("handle javascript https rpc event");
            }

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

        #[test]
        #[ignore = "V8 sidecar listener integration is flaky in this harness; execution-layer tests cover the V8 bridge path"]
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
            let host_port = {
                let vm = sidecar.vms.get(&vm_id).expect("javascript vm");
                vm.active_processes
                    .get("proc-js-server")
                    .and_then(|process| process.tcp_listeners.get(&server_id))
                    .expect("sidecar tcp listener")
                    .local_addr()
                    .port()
            };

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

            let client = thread::spawn(move || {
                let mut stream = TcpStream::connect(("127.0.0.1", host_port))
                    .expect("connect to sidecar listener");
                stream.write_all(b"ping").expect("write client payload");
                stream
                    .shutdown(Shutdown::Write)
                    .expect("shutdown client write half");
                let mut received = Vec::new();
                stream
                    .read_to_end(&mut received)
                    .expect("read server response");
                assert_eq!(
                    String::from_utf8(received).expect("server response utf8"),
                    "pong:ping"
                );
            });

            let accepted = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-server",
                JavascriptSyncRpcRequest {
                    id: 2,
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

            let data = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-server",
                JavascriptSyncRpcRequest {
                    id: 3,
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
                    id: 4,
                    method: String::from("net.write"),
                    args: vec![json!(socket_id.clone()), json!("pong:ping")],
                },
            )
            .expect("write response");
            assert_eq!(written, Value::from(9));

            call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-server",
                JavascriptSyncRpcRequest {
                    id: 5,
                    method: String::from("net.shutdown"),
                    args: vec![json!(socket_id)],
                },
            )
            .expect("shutdown write half");
            client.join().expect("join tcp client");
        }

        #[test]
        #[ignore = "V8 sidecar listener accounting integration is flaky in this harness; execution-layer tests cover the V8 bridge path"]
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
                    String::from("proc-js-backlog"),
                    ActiveProcess::new(
                        kernel_handle.pid(),
                        kernel_handle,
                        GuestRuntimeKind::JavaScript,
                        ActiveExecution::Javascript(execution),
                    ),
                );
            }

            let bridge = sidecar.bridge.clone();
            let dns = sidecar.vms.get(&vm_id).expect("javascript vm").dns.clone();
            let limits = ResourceLimits::default();
            let socket_paths = {
                let vm = sidecar.vms.get(&vm_id).expect("javascript vm");
                build_javascript_socket_path_context(vm).expect("build socket path context")
            };

            let listen = {
                let counts = sidecar
                    .vms
                    .get(&vm_id)
                    .and_then(|vm| vm.active_processes.get("proc-js-backlog"))
                    .expect("backlog process")
                    .network_resource_counts();
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                let process = vm
                    .active_processes
                    .get_mut("proc-js-backlog")
                    .expect("backlog process");
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
                            "host": "127.0.0.1",
                            "port": 0,
                            "backlog": 1,
                        })],
                    },
                    &limits,
                    counts,
                )
                .expect("listen through sidecar net RPC")
            };
            let server_id = listen["serverId"].as_str().expect("server id").to_string();
            let _port = listen["localPort"]
                .as_u64()
                .and_then(|value| u16::try_from(value).ok())
                .expect("listener port");
            let host_port = {
                let vm = sidecar.vms.get(&vm_id).expect("javascript vm");
                vm.active_processes
                    .get("proc-js-backlog")
                    .and_then(|process| process.tcp_listeners.get(&server_id))
                    .expect("host backlog listener")
                    .local_addr()
                    .port()
            };

            let first_client = thread::spawn(move || {
                let mut stream = TcpStream::connect(("127.0.0.1", host_port))
                    .expect("connect first backlog client");
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .expect("set first client timeout");
                let mut received = Vec::new();
                stream
                    .read_to_end(&mut received)
                    .expect("read first backlog client EOF");
                assert!(
                    received.is_empty(),
                    "first backlog client should not receive data"
                );
            });

            let first_connection = {
                let counts = sidecar
                    .vms
                    .get(&vm_id)
                    .and_then(|vm| vm.active_processes.get("proc-js-backlog"))
                    .expect("backlog process")
                    .network_resource_counts();
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                let process = vm
                    .active_processes
                    .get_mut("proc-js-backlog")
                    .expect("backlog process");
                service_javascript_net_sync_rpc(
                    &bridge,
                    &vm_id,
                    &dns,
                    &socket_paths,
                    &mut vm.kernel,
                    process,
                    &JavascriptSyncRpcRequest {
                        id: 2,
                        method: String::from("net.server_poll"),
                        args: vec![json!(server_id), json!(250)],
                    },
                    &limits,
                    counts,
                )
                .expect("accept first backlog connection")
            };
            let first_socket_id = first_connection["socketId"]
                .as_str()
                .expect("first socket id")
                .to_string();

            let connection_count = {
                let counts = sidecar
                    .vms
                    .get(&vm_id)
                    .and_then(|vm| vm.active_processes.get("proc-js-backlog"))
                    .expect("backlog process")
                    .network_resource_counts();
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                let process = vm
                    .active_processes
                    .get_mut("proc-js-backlog")
                    .expect("backlog process");
                service_javascript_net_sync_rpc(
                    &bridge,
                    &vm_id,
                    &dns,
                    &socket_paths,
                    &mut vm.kernel,
                    process,
                    &JavascriptSyncRpcRequest {
                        id: 3,
                        method: String::from("net.server_connections"),
                        args: vec![json!(server_id)],
                    },
                    &limits,
                    counts,
                )
                .expect("query server connections")
            };
            assert_eq!(connection_count, json!(1));

            let second_client = thread::spawn(move || {
                let address = SocketAddr::from(([127, 0, 0, 1], host_port));
                let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))
                    .expect("connect second backlog client");
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("set second client timeout");
                stream
                    .write_all(b"blocked")
                    .expect("write second backlog client payload");
                let mut buffer = [0_u8; 16];
                match stream.read(&mut buffer) {
                    Ok(0) => {}
                    Ok(bytes_read) => panic!(
                        "unexpected second backlog payload: {}",
                        String::from_utf8_lossy(&buffer[..bytes_read])
                    ),
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::ConnectionAborted
                                | std::io::ErrorKind::ConnectionReset
                                | std::io::ErrorKind::NotConnected
                                | std::io::ErrorKind::TimedOut
                                | std::io::ErrorKind::WouldBlock
                        ) => {}
                    Err(error) => panic!("unexpected second backlog read error: {error}"),
                }
            });

            let second_poll = {
                let counts = sidecar
                    .vms
                    .get(&vm_id)
                    .and_then(|vm| vm.active_processes.get("proc-js-backlog"))
                    .expect("backlog process")
                    .network_resource_counts();
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                let process = vm
                    .active_processes
                    .get_mut("proc-js-backlog")
                    .expect("backlog process");
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
                .expect("poll second backlog connection")
            };
            assert_eq!(second_poll, Value::Null);
            second_client.join().expect("join second backlog client");

            let connection_count = {
                let counts = sidecar
                    .vms
                    .get(&vm_id)
                    .and_then(|vm| vm.active_processes.get("proc-js-backlog"))
                    .expect("backlog process")
                    .network_resource_counts();
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                let process = vm
                    .active_processes
                    .get_mut("proc-js-backlog")
                    .expect("backlog process");
                service_javascript_net_sync_rpc(
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
                .expect("query server connections after backlog rejection")
            };
            assert_eq!(connection_count, json!(1));

            {
                let counts = sidecar
                    .vms
                    .get(&vm_id)
                    .and_then(|vm| vm.active_processes.get("proc-js-backlog"))
                    .expect("backlog process")
                    .network_resource_counts();
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                let process = vm
                    .active_processes
                    .get_mut("proc-js-backlog")
                    .expect("backlog process");
                service_javascript_net_sync_rpc(
                    &bridge,
                    &vm_id,
                    &dns,
                    &socket_paths,
                    &mut vm.kernel,
                    process,
                    &JavascriptSyncRpcRequest {
                        id: 6,
                        method: String::from("net.destroy"),
                        args: vec![json!(first_socket_id)],
                    },
                    &limits,
                    counts,
                )
                .expect("destroy first backlog socket");
            }
            first_client.join().expect("join first backlog client");

            {
                let counts = sidecar
                    .vms
                    .get(&vm_id)
                    .and_then(|vm| vm.active_processes.get("proc-js-backlog"))
                    .expect("backlog process")
                    .network_resource_counts();
                let vm = sidecar.vms.get_mut(&vm_id).expect("javascript vm");
                let process = vm
                    .active_processes
                    .get_mut("proc-js-backlog")
                    .expect("backlog process");
                service_javascript_net_sync_rpc(
                    &bridge,
                    &vm_id,
                    &dns,
                    &socket_paths,
                    &mut vm.kernel,
                    process,
                    &JavascriptSyncRpcRequest {
                        id: 7,
                        method: String::from("net.server_close"),
                        args: vec![json!(server_id)],
                    },
                    &limits,
                    counts,
                )
                .expect("close backlog listener");
            }

            sidecar
                .dispose_vm_internal_blocking(
                    &connection_id,
                    &session_id,
                    &vm_id,
                    DisposeReason::Requested,
                )
                .expect("dispose backlog vm");
        }

        #[test]
        #[ignore = "V8 sidecar bind-policy integration is flaky in this harness; execution-layer tests cover the V8 bridge path"]
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
            .expect_err("deny unspecified TCP listen host");
            assert!(
                unspecified
                    .to_string()
                    .contains("must bind to loopback, not unspecified"),
                "{unspecified}"
            );

            let privileged = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-bind-policy",
                JavascriptSyncRpcRequest {
                    id: 2,
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
                    id: 3,
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
                    id: 4,
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
                    id: 5,
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
            .expect_err("deny unspecified UDP bind host");
            assert!(
                udp_unspecified
                    .to_string()
                    .contains("must bind to loopback, not unspecified"),
                "{udp_unspecified}"
            );

            let success = call_javascript_sync_rpc(
                &mut sidecar,
                &vm_id,
                "proc-js-bind-policy",
                JavascriptSyncRpcRequest {
                    id: 6,
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

        #[test]
        #[ignore = "V8 sidecar privileged bind integration is flaky in this harness; execution-layer tests cover the V8 bridge path"]
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

        #[test]
        #[ignore = "V8 sidecar per-VM listener isolation integration is flaky in this harness; execution-layer tests cover the V8 bridge path"]
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

        #[test]
        #[ignore = "V8 sidecar Unix-socket integration is flaky in this harness; execution-layer tests cover the V8 bridge path"]
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
                    ),
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

        #[test]
        #[ignore = "V8 nested child_process output/lifecycle delivery is flaky in the sidecar harness; execution-layer tests cover the V8 bridge path"]
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
child.stdout.setEncoding("utf8");
child.stdout.on("data", (chunk) => {
  spawnOutput += chunk;
});
await new Promise((resolve, reject) => {
  child.on("error", reject);
  child.on("close", (code) => {
    if (code !== 0) {
      reject(new Error(`spawn exit ${code}`));
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
                    let vm = sidecar.vms.get(&vm_id).expect("javascript vm");
                    vm.active_processes
                        .get("proc-js-child")
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

        #[test]
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
    }
}

pub use crate::service::{DispatchResult, NativeSidecar, SidecarError};
