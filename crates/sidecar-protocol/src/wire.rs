//! Generated AgentOS language execution sidecar wire protocol surface.
//!
//! This module is the public generated protocol entrypoint. The hand-written
//! `protocol` module remains an internal compatibility layer while callers move
//! to generated wire frames.

use std::error::Error;
use std::fmt;

pub use crate::generated_protocol::v1::*;

// The generated BARE types intentionally omit `Copy`/`Default`; restore them on the
// crate-local generated types so the wider sidecar keeps the ergonomics it relies on
// after the hand-written protocol types were replaced with these aliases. These live in
// `wire` (not `protocol`) because `protocol.rs` is `#[path]`-included by integration
// tests, where the generated types would be foreign and the impls would break the orphan rule.
impl Copy for crate::generated_protocol::v1::GuestFilesystemOperation {}
impl Copy for crate::generated_protocol::v1::RootFilesystemMode {}
impl Copy for crate::generated_protocol::v1::WasmPermissionTier {}

// `derive(Default)` cannot be added: these are foreign generated types, so the
// `Default` impl must be written by hand here (orphan rule).
#[allow(clippy::derivable_impls)]
impl Default for crate::generated_protocol::v1::RootFilesystemEntryKind {
    fn default() -> Self {
        Self::File
    }
}

impl Default for crate::generated_protocol::v1::RootFilesystemEntry {
    fn default() -> Self {
        Self {
            path: String::new(),
            kind: crate::generated_protocol::v1::RootFilesystemEntryKind::File,
            mode: None,
            uid: None,
            gid: None,
            content: None,
            encoding: None,
            target: None,
            executable: false,
        }
    }
}

#[allow(clippy::derivable_impls)]
impl Default for crate::generated_protocol::v1::RootFilesystemMode {
    fn default() -> Self {
        Self::Ephemeral
    }
}

#[allow(clippy::derivable_impls)]
impl Default for crate::generated_protocol::v1::RootFilesystemDescriptor {
    fn default() -> Self {
        Self {
            mode: crate::generated_protocol::v1::RootFilesystemMode::default(),
            disable_default_base_layer: false,
            lowers: Vec::new(),
            bootstrap_entries: Vec::new(),
        }
    }
}

impl crate::generated_protocol::v1::PermissionsPolicy {
    pub fn deny_all() -> Self {
        use crate::generated_protocol::v1::{
            FsPermissionScope, PatternPermissionScope, PermissionMode,
        };
        Self {
            fs: Some(FsPermissionScope::PermissionMode(PermissionMode::Deny)),
            network: Some(PatternPermissionScope::PermissionMode(PermissionMode::Deny)),
            child_process: Some(PatternPermissionScope::PermissionMode(PermissionMode::Deny)),
            process: Some(PatternPermissionScope::PermissionMode(PermissionMode::Deny)),
            env: Some(PatternPermissionScope::PermissionMode(PermissionMode::Deny)),
            binding: Some(PatternPermissionScope::PermissionMode(PermissionMode::Deny)),
        }
    }

    pub fn allow_all() -> Self {
        use crate::generated_protocol::v1::{
            FsPermissionScope, PatternPermissionScope, PermissionMode,
        };
        Self {
            fs: Some(FsPermissionScope::PermissionMode(PermissionMode::Allow)),
            network: Some(PatternPermissionScope::PermissionMode(
                PermissionMode::Allow,
            )),
            child_process: Some(PatternPermissionScope::PermissionMode(
                PermissionMode::Allow,
            )),
            process: Some(PatternPermissionScope::PermissionMode(
                PermissionMode::Allow,
            )),
            env: Some(PatternPermissionScope::PermissionMode(
                PermissionMode::Allow,
            )),
            binding: Some(PatternPermissionScope::PermissionMode(
                PermissionMode::Allow,
            )),
        }
    }
}

impl Default for crate::generated_protocol::v1::PermissionsPolicy {
    fn default() -> Self {
        Self::deny_all()
    }
}

impl crate::generated_protocol::v1::CreateVmRequest {
    pub fn json_config(
        runtime: crate::generated_protocol::v1::GuestRuntimeKind,
        config: agentos_vm_config::CreateVmConfig,
    ) -> Self {
        Self {
            runtime,
            config: serde_json::to_string(&config).expect("serialize create VM config"),
        }
    }

    pub fn legacy_test_config(
        runtime: crate::generated_protocol::v1::GuestRuntimeKind,
        metadata: std::collections::HashMap<String, String>,
        root_filesystem: crate::generated_protocol::v1::RootFilesystemDescriptor,
        permissions: Option<crate::generated_protocol::v1::PermissionsPolicy>,
    ) -> Self {
        let metadata: std::collections::BTreeMap<_, _> = metadata.into_iter().collect();
        let mut config = agentos_vm_config::CreateVmConfig {
            cwd: metadata.get("cwd").cloned(),
            env: legacy_env_config(&metadata),
            root_filesystem: legacy_root_filesystem_config(root_filesystem),
            permissions: permissions.map(permissions_policy_config_from_wire),
            limits: legacy_limits_config(&metadata),
            dns: legacy_dns_config(&metadata),
            native_root: legacy_native_root_config(&metadata),
            listen: legacy_listen_config(&metadata),
            ..Default::default()
        };
        config.loopback_exempt_ports = legacy_loopback_exempt_ports(&config.env);
        Self::json_config(runtime, config)
    }
}

fn legacy_env_config(
    metadata: &std::collections::BTreeMap<String, String>,
) -> std::collections::BTreeMap<String, String> {
    metadata
        .iter()
        .filter_map(|(key, value)| {
            key.strip_prefix("env.")
                .map(|name| (name.to_string(), value.clone()))
        })
        .collect()
}

fn legacy_root_filesystem_config(
    descriptor: crate::generated_protocol::v1::RootFilesystemDescriptor,
) -> agentos_vm_config::RootFilesystemConfig {
    agentos_vm_config::RootFilesystemConfig {
        mode: match descriptor.mode {
            crate::generated_protocol::v1::RootFilesystemMode::Ephemeral => {
                agentos_vm_config::RootFilesystemMode::Ephemeral
            }
            crate::generated_protocol::v1::RootFilesystemMode::ReadOnly => {
                agentos_vm_config::RootFilesystemMode::ReadOnly
            }
        },
        disable_default_base_layer: descriptor.disable_default_base_layer,
        lowers: descriptor
            .lowers
            .into_iter()
            .map(legacy_root_lower_config)
            .collect(),
        bootstrap_entries: descriptor
            .bootstrap_entries
            .into_iter()
            .map(legacy_root_entry_config)
            .collect(),
    }
}

fn legacy_root_lower_config(
    lower: crate::generated_protocol::v1::RootFilesystemLowerDescriptor,
) -> agentos_vm_config::RootFilesystemLowerDescriptor {
    match lower {
        crate::generated_protocol::v1::RootFilesystemLowerDescriptor::SnapshotRootFilesystemLower(
            snapshot,
        ) => agentos_vm_config::RootFilesystemLowerDescriptor::Snapshot {
            entries: snapshot
                .entries
                .into_iter()
                .map(legacy_root_entry_config)
                .collect(),
        },
        crate::generated_protocol::v1::RootFilesystemLowerDescriptor::BundledBaseFilesystemLower => {
            agentos_vm_config::RootFilesystemLowerDescriptor::BundledBaseFilesystem
        }
    }
}

fn legacy_root_entry_config(
    entry: crate::generated_protocol::v1::RootFilesystemEntry,
) -> agentos_vm_config::RootFilesystemEntry {
    agentos_vm_config::RootFilesystemEntry {
        path: entry.path,
        kind: match entry.kind {
            crate::generated_protocol::v1::RootFilesystemEntryKind::File => {
                agentos_vm_config::RootFilesystemEntryKind::File
            }
            crate::generated_protocol::v1::RootFilesystemEntryKind::Directory => {
                agentos_vm_config::RootFilesystemEntryKind::Directory
            }
            crate::generated_protocol::v1::RootFilesystemEntryKind::Symlink => {
                agentos_vm_config::RootFilesystemEntryKind::Symlink
            }
        },
        mode: entry.mode,
        uid: entry.uid,
        gid: entry.gid,
        content: entry.content,
        encoding: entry.encoding.map(|encoding| match encoding {
            crate::generated_protocol::v1::RootFilesystemEntryEncoding::Utf8 => {
                agentos_vm_config::RootFilesystemEntryEncoding::Utf8
            }
            crate::generated_protocol::v1::RootFilesystemEntryEncoding::Base64 => {
                agentos_vm_config::RootFilesystemEntryEncoding::Base64
            }
        }),
        target: entry.target,
        executable: entry.executable,
    }
}

pub fn permissions_policy_config_from_wire(
    permissions: crate::generated_protocol::v1::PermissionsPolicy,
) -> agentos_vm_config::PermissionsPolicy {
    agentos_vm_config::PermissionsPolicy {
        fs: permissions.fs.map(legacy_fs_permission_scope_config),
        network: permissions
            .network
            .map(legacy_pattern_permission_scope_config),
        child_process: permissions
            .child_process
            .map(legacy_pattern_permission_scope_config),
        process: permissions
            .process
            .map(legacy_pattern_permission_scope_config),
        env: permissions.env.map(legacy_pattern_permission_scope_config),
        binding: permissions
            .binding
            .map(legacy_pattern_permission_scope_config),
    }
}

fn legacy_permission_mode_config(
    mode: crate::generated_protocol::v1::PermissionMode,
) -> agentos_vm_config::PermissionMode {
    match mode {
        crate::generated_protocol::v1::PermissionMode::Allow => {
            agentos_vm_config::PermissionMode::Allow
        }
        crate::generated_protocol::v1::PermissionMode::Ask => {
            agentos_vm_config::PermissionMode::Ask
        }
        crate::generated_protocol::v1::PermissionMode::Deny => {
            agentos_vm_config::PermissionMode::Deny
        }
    }
}

fn legacy_fs_permission_scope_config(
    scope: crate::generated_protocol::v1::FsPermissionScope,
) -> agentos_vm_config::FsPermissionScope {
    match scope {
        crate::generated_protocol::v1::FsPermissionScope::PermissionMode(mode) => {
            agentos_vm_config::FsPermissionScope::Mode(legacy_permission_mode_config(mode))
        }
        crate::generated_protocol::v1::FsPermissionScope::FsPermissionRuleSet(rules) => {
            agentos_vm_config::FsPermissionScope::Rules(agentos_vm_config::FsPermissionRuleSet {
                default: rules.default.map(legacy_permission_mode_config),
                rules: rules
                    .rules
                    .into_iter()
                    .map(|rule| agentos_vm_config::FsPermissionRule {
                        mode: legacy_permission_mode_config(rule.mode),
                        operations: rule.operations,
                        paths: rule.paths,
                    })
                    .collect(),
            })
        }
    }
}

fn legacy_pattern_permission_scope_config(
    scope: crate::generated_protocol::v1::PatternPermissionScope,
) -> agentos_vm_config::PatternPermissionScope {
    match scope {
        crate::generated_protocol::v1::PatternPermissionScope::PermissionMode(mode) => {
            agentos_vm_config::PatternPermissionScope::Mode(legacy_permission_mode_config(mode))
        }
        crate::generated_protocol::v1::PatternPermissionScope::PatternPermissionRuleSet(rules) => {
            agentos_vm_config::PatternPermissionScope::Rules(
                agentos_vm_config::PatternPermissionRuleSet {
                    default: rules.default.map(legacy_permission_mode_config),
                    rules: rules
                        .rules
                        .into_iter()
                        .map(|rule| agentos_vm_config::PatternPermissionRule {
                            mode: legacy_permission_mode_config(rule.mode),
                            operations: rule.operations,
                            patterns: rule.patterns,
                        })
                        .collect(),
                },
            )
        }
    }
}

fn legacy_dns_config(
    metadata: &std::collections::BTreeMap<String, String>,
) -> Option<agentos_vm_config::VmDnsConfig> {
    let mut dns = agentos_vm_config::VmDnsConfig::default();
    if let Some(value) = metadata.get("network.dns.servers") {
        dns.name_servers = value
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(str::to_string)
            .collect();
    }
    for (key, value) in metadata {
        let Some(hostname) = key.strip_prefix("network.dns.override.") else {
            continue;
        };
        dns.overrides.insert(
            hostname.to_string(),
            value
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(str::to_string)
                .collect(),
        );
    }
    if dns.name_servers.is_empty() && dns.overrides.is_empty() {
        None
    } else {
        Some(dns)
    }
}

fn legacy_native_root_config(
    metadata: &std::collections::BTreeMap<String, String>,
) -> Option<agentos_vm_config::NativeRootFilesystemConfig> {
    let id = metadata.get("rootFilesystem.nativePlugin.id")?;
    let config = metadata
        .get("rootFilesystem.nativePlugin.config")
        .map(|value| serde_json::from_str(value).expect("parse native root plugin config"))
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
    let read_only = metadata
        .get("rootFilesystem.nativePlugin.readOnly")
        .map(|value| value.parse::<bool>().expect("parse native root readOnly"))
        .unwrap_or(false);
    Some(agentos_vm_config::NativeRootFilesystemConfig {
        plugin: agentos_vm_config::MountPluginDescriptor {
            id: id.clone(),
            config,
        },
        read_only,
    })
}

fn legacy_listen_config(
    metadata: &std::collections::BTreeMap<String, String>,
) -> Option<agentos_vm_config::VmListenPolicyConfig> {
    let listen = agentos_vm_config::VmListenPolicyConfig {
        port_min: metadata
            .get("network.listen.port_min")
            .map(|value| value.parse::<u16>().expect("parse network.listen.port_min")),
        port_max: metadata
            .get("network.listen.port_max")
            .map(|value| value.parse::<u16>().expect("parse network.listen.port_max")),
        allow_privileged: metadata
            .get("network.listen.allow_privileged")
            .map(|value| {
                value
                    .parse::<bool>()
                    .expect("parse network.listen.allow_privileged")
            }),
    };
    if listen.port_min.is_none() && listen.port_max.is_none() && listen.allow_privileged.is_none() {
        None
    } else {
        Some(listen)
    }
}

fn legacy_loopback_exempt_ports(env: &std::collections::BTreeMap<String, String>) -> Vec<u16> {
    let Some(value) = env.get("AGENTOS_LOOPBACK_EXEMPT_PORTS") else {
        return Vec::new();
    };
    serde_json::from_str::<Vec<serde_json::Value>>(value)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|value| match value {
            serde_json::Value::Number(number) => number.as_u64(),
            serde_json::Value::String(value) => value.parse::<u64>().ok(),
            _ => None,
        })
        .filter_map(|port| u16::try_from(port).ok())
        .collect()
}

fn legacy_limits_config(
    metadata: &std::collections::BTreeMap<String, String>,
) -> Option<agentos_vm_config::VmLimitsConfig> {
    let resources = agentos_vm_config::ResourceLimitsConfig {
        cpu_count: legacy_u64(metadata, "resource.cpu_count"),
        max_processes: legacy_u64(metadata, "resource.max_processes"),
        max_open_fds: legacy_u64(metadata, "resource.max_open_fds"),
        max_pipes: legacy_u64(metadata, "resource.max_pipes"),
        max_ptys: legacy_u64(metadata, "resource.max_ptys"),
        max_sockets: legacy_u64(metadata, "resource.max_sockets"),
        max_connections: legacy_u64(metadata, "resource.max_connections"),
        max_socket_buffered_bytes: legacy_u64(metadata, "resource.max_socket_buffered_bytes"),
        max_socket_datagram_queue_len: legacy_u64(
            metadata,
            "resource.max_socket_datagram_queue_len",
        ),
        max_filesystem_bytes: legacy_u64(metadata, "resource.max_filesystem_bytes"),
        max_inode_count: legacy_u64(metadata, "resource.max_inode_count"),
        max_blocking_read_ms: legacy_u64(metadata, "resource.max_blocking_read_ms"),
        max_pread_bytes: legacy_u64(metadata, "resource.max_pread_bytes"),
        max_fd_write_bytes: legacy_u64(metadata, "resource.max_fd_write_bytes"),
        max_process_argv_bytes: legacy_u64(metadata, "resource.max_process_argv_bytes"),
        max_process_env_bytes: legacy_u64(metadata, "resource.max_process_env_bytes"),
        max_readdir_entries: legacy_u64(metadata, "resource.max_readdir_entries"),
        max_recursive_fs_depth: legacy_u64(metadata, "resource.max_recursive_fs_depth"),
        max_recursive_fs_entries: legacy_u64(metadata, "resource.max_recursive_fs_entries"),
        max_wasm_memory_bytes: legacy_u64(metadata, "resource.max_wasm_memory_bytes"),
        max_wasm_stack_bytes: legacy_u64(metadata, "resource.max_wasm_stack_bytes"),
    };
    let http = agentos_vm_config::HttpLimitsConfig {
        max_fetch_response_bytes: legacy_u64(metadata, "limits.http.max_fetch_response_bytes"),
    };
    let bindings = agentos_vm_config::BindingLimitsConfig {
        default_binding_timeout_ms: legacy_u64(
            metadata,
            "limits.bindings.default_binding_timeout_ms",
        ),
        max_binding_timeout_ms: legacy_u64(metadata, "limits.bindings.max_binding_timeout_ms"),
        max_registered_collections: legacy_u64(
            metadata,
            "limits.bindings.max_registered_collections",
        ),
        max_registered_bindings_per_vm: legacy_u64(
            metadata,
            "limits.bindings.max_registered_bindings_per_vm",
        ),
        max_bindings_per_collection: legacy_u64(
            metadata,
            "limits.bindings.max_bindings_per_collection",
        ),
        max_binding_schema_bytes: legacy_u64(metadata, "limits.bindings.max_binding_schema_bytes"),
        max_examples_per_binding: legacy_u64(metadata, "limits.bindings.max_examples_per_binding"),
        max_binding_example_input_bytes: legacy_u64(
            metadata,
            "limits.bindings.max_binding_example_input_bytes",
        ),
    };
    let plugins = agentos_vm_config::PluginLimitsConfig {
        max_persisted_manifest_bytes: legacy_u64(
            metadata,
            "limits.plugins.max_persisted_manifest_bytes",
        ),
        max_persisted_manifest_file_bytes: legacy_u64(
            metadata,
            "limits.plugins.max_persisted_manifest_file_bytes",
        ),
    };
    let acp = agentos_vm_config::AcpLimitsConfig {
        max_read_line_bytes: legacy_u64(metadata, "limits.acp.max_read_line_bytes"),
        stdout_buffer_byte_limit: legacy_u64(metadata, "limits.acp.stdout_buffer_byte_limit"),
        max_completed_message_bytes: legacy_u64(metadata, "limits.acp.max_completed_message_bytes"),
        max_turn_output_bytes: legacy_u64(metadata, "limits.acp.max_turn_output_bytes"),
        max_prompt_bytes: legacy_u64(metadata, "limits.acp.max_prompt_bytes"),
        max_prompt_blocks: legacy_u64(metadata, "limits.acp.max_prompt_blocks"),
        max_fallback_continuation_bytes: legacy_u64(
            metadata,
            "limits.acp.max_fallback_continuation_bytes",
        ),
        max_session_history_bytes: legacy_u64(metadata, "limits.acp.max_session_history_bytes"),
        max_session_history_events: legacy_u64(metadata, "limits.acp.max_session_history_events"),
        max_history_page_entries: legacy_u64(metadata, "limits.acp.max_history_page_entries"),
        max_session_list_entries: legacy_u64(metadata, "limits.acp.max_session_list_entries"),
        max_sessions_per_vm: legacy_u64(metadata, "limits.acp.max_sessions_per_vm"),
        max_prompts_per_session: legacy_u64(metadata, "limits.acp.max_prompts_per_session"),
        max_prompts_per_vm: legacy_u64(metadata, "limits.acp.max_prompts_per_vm"),
        max_pending_permissions_per_session: legacy_u64(
            metadata,
            "limits.acp.max_pending_permissions_per_session",
        ),
        max_pending_permissions_per_vm: legacy_u64(
            metadata,
            "limits.acp.max_pending_permissions_per_vm",
        ),
        max_permission_outcomes_per_session: legacy_u64(
            metadata,
            "limits.acp.max_permission_outcomes_per_session",
        ),
        max_permission_outcomes_per_vm: legacy_u64(
            metadata,
            "limits.acp.max_permission_outcomes_per_vm",
        ),
    };
    let sqlite = agentos_vm_config::SqliteLimitsConfig {
        max_result_bytes: legacy_u64(metadata, "limits.sqlite.max_result_bytes"),
    };
    let js_runtime = agentos_vm_config::JsRuntimeLimitsConfig {
        v8_heap_limit_mb: legacy_u64(metadata, "limits.js_runtime.v8_heap_limit_mb"),
        sync_rpc_wait_timeout_ms: legacy_u64(
            metadata,
            "limits.js_runtime.sync_rpc_wait_timeout_ms",
        ),
        cpu_time_limit_ms: legacy_u64(metadata, "limits.js_runtime.cpu_time_limit_ms"),
        wall_clock_limit_ms: legacy_u64(metadata, "limits.js_runtime.wall_clock_limit_ms"),
        import_cache_materialize_timeout_ms: legacy_u64(
            metadata,
            "limits.js_runtime.import_cache_materialize_timeout_ms",
        ),
        captured_output_limit_bytes: legacy_u64(
            metadata,
            "limits.js_runtime.captured_output_limit_bytes",
        ),
        stdin_buffer_limit_bytes: legacy_u64(
            metadata,
            "limits.js_runtime.stdin_buffer_limit_bytes",
        ),
        event_payload_limit_bytes: legacy_u64(
            metadata,
            "limits.js_runtime.event_payload_limit_bytes",
        ),
        max_timers: legacy_u64(metadata, "limits.js_runtime.max_timers"),
        v8_ipc_max_frame_bytes: legacy_u64(metadata, "limits.js_runtime.v8_ipc_max_frame_bytes"),
    };
    let python = agentos_vm_config::PythonLimitsConfig {
        output_buffer_max_bytes: legacy_u64(metadata, "limits.python.output_buffer_max_bytes"),
        execution_timeout_ms: legacy_u64(metadata, "limits.python.execution_timeout_ms"),
        max_old_space_mb: legacy_u64(metadata, "limits.python.max_old_space_mb"),
        vfs_rpc_timeout_ms: legacy_u64(metadata, "limits.python.vfs_rpc_timeout_ms"),
    };
    let wasm = agentos_vm_config::WasmLimitsConfig {
        max_module_file_bytes: legacy_u64(metadata, "limits.wasm.max_module_file_bytes"),
        captured_output_limit_bytes: legacy_u64(
            metadata,
            "limits.wasm.captured_output_limit_bytes",
        ),
        sync_read_limit_bytes: legacy_u64(metadata, "limits.wasm.sync_read_limit_bytes"),
        prewarm_timeout_ms: legacy_u64(metadata, "limits.wasm.prewarm_timeout_ms"),
        runner_heap_limit_mb: legacy_u64(metadata, "limits.wasm.runner_heap_limit_mb"),
        active_cpu_time_limit_ms: legacy_u64(metadata, "limits.wasm.active_cpu_time_limit_ms"),
        wall_clock_limit_ms: legacy_u64(metadata, "limits.wasm.wall_clock_limit_ms"),
        deterministic_fuel: legacy_u64(metadata, "limits.wasm.deterministic_fuel"),
    };
    let execution = agentos_vm_config::ExecutionLimitsConfig {
        completed_ttl_ms: legacy_u64(metadata, "limits.execution.completed_ttl_ms"),
        max_completed_executions: legacy_u64(metadata, "limits.execution.max_completed_executions"),
        live_execution_warning_threshold: legacy_u64(
            metadata,
            "limits.execution.live_execution_warning_threshold",
        ),
    };
    let process = agentos_vm_config::ProcessLimitsConfig {
        max_spawn_file_actions: legacy_u64(metadata, "limits.process.max_spawn_file_actions")
            .or_else(|| legacy_u64(metadata, "limits.wasm.max_spawn_file_actions")),
        max_spawn_file_action_bytes: legacy_u64(
            metadata,
            "limits.process.max_spawn_file_action_bytes",
        )
        .or_else(|| legacy_u64(metadata, "limits.wasm.max_spawn_file_action_bytes")),
        pending_stdin_bytes: legacy_u64(metadata, "limits.process.pending_stdin_bytes"),
        pending_event_count: legacy_u64(metadata, "limits.process.pending_event_count"),
        pending_event_bytes: legacy_u64(metadata, "limits.process.pending_event_bytes"),
        max_pending_child_sync_count: legacy_u64(
            metadata,
            "limits.process.max_pending_child_sync_count",
        ),
        max_pending_child_sync_bytes: legacy_u64(
            metadata,
            "limits.process.max_pending_child_sync_bytes",
        ),
    };

    let config = agentos_vm_config::VmLimitsConfig {
        reactor: None,
        resources: legacy_has_resource_limits(&resources).then_some(resources),
        http: http.max_fetch_response_bytes.is_some().then_some(http),
        udp: None,
        tls: None,
        http2: None,
        bindings: legacy_has_binding_limits(&bindings).then_some(bindings),
        plugins: legacy_has_plugin_limits(&plugins).then_some(plugins),
        acp: legacy_has_acp_limits(&acp).then_some(acp),
        sqlite: sqlite.max_result_bytes.is_some().then_some(sqlite),
        js_runtime: legacy_has_js_runtime_limits(&js_runtime).then_some(js_runtime),
        python: legacy_has_python_limits(&python).then_some(python),
        wasm: legacy_has_wasm_limits(&wasm).then_some(wasm),
        execution: (execution.completed_ttl_ms.is_some()
            || execution.max_completed_executions.is_some()
            || execution.live_execution_warning_threshold.is_some())
        .then_some(execution),
        process: legacy_has_process_limits(&process).then_some(process),
    };

    if config.resources.is_none()
        && config.http.is_none()
        && config.bindings.is_none()
        && config.plugins.is_none()
        && config.acp.is_none()
        && config.sqlite.is_none()
        && config.js_runtime.is_none()
        && config.python.is_none()
        && config.wasm.is_none()
        && config.execution.is_none()
        && config.process.is_none()
    {
        None
    } else {
        Some(config)
    }
}

fn legacy_u64(metadata: &std::collections::BTreeMap<String, String>, key: &str) -> Option<u64> {
    metadata.get(key).map(|value| {
        value
            .parse::<u64>()
            .unwrap_or_else(|error| panic!("parse {key}: {error}"))
    })
}

fn legacy_has_resource_limits(config: &agentos_vm_config::ResourceLimitsConfig) -> bool {
    config.cpu_count.is_some()
        || config.max_processes.is_some()
        || config.max_open_fds.is_some()
        || config.max_pipes.is_some()
        || config.max_ptys.is_some()
        || config.max_sockets.is_some()
        || config.max_connections.is_some()
        || config.max_socket_buffered_bytes.is_some()
        || config.max_socket_datagram_queue_len.is_some()
        || config.max_filesystem_bytes.is_some()
        || config.max_inode_count.is_some()
        || config.max_blocking_read_ms.is_some()
        || config.max_pread_bytes.is_some()
        || config.max_fd_write_bytes.is_some()
        || config.max_process_argv_bytes.is_some()
        || config.max_process_env_bytes.is_some()
        || config.max_readdir_entries.is_some()
        || config.max_wasm_memory_bytes.is_some()
        || config.max_wasm_stack_bytes.is_some()
}

fn legacy_has_binding_limits(config: &agentos_vm_config::BindingLimitsConfig) -> bool {
    config.default_binding_timeout_ms.is_some()
        || config.max_binding_timeout_ms.is_some()
        || config.max_registered_collections.is_some()
        || config.max_registered_bindings_per_vm.is_some()
        || config.max_bindings_per_collection.is_some()
        || config.max_binding_schema_bytes.is_some()
        || config.max_examples_per_binding.is_some()
        || config.max_binding_example_input_bytes.is_some()
}

fn legacy_has_plugin_limits(config: &agentos_vm_config::PluginLimitsConfig) -> bool {
    config.max_persisted_manifest_bytes.is_some()
        || config.max_persisted_manifest_file_bytes.is_some()
}

fn legacy_has_acp_limits(config: &agentos_vm_config::AcpLimitsConfig) -> bool {
    config.max_read_line_bytes.is_some()
        || config.stdout_buffer_byte_limit.is_some()
        || config.max_completed_message_bytes.is_some()
        || config.max_turn_output_bytes.is_some()
        || config.max_prompt_bytes.is_some()
        || config.max_prompt_blocks.is_some()
        || config.max_fallback_continuation_bytes.is_some()
        || config.max_session_history_bytes.is_some()
        || config.max_session_history_events.is_some()
        || config.max_history_page_entries.is_some()
        || config.max_session_list_entries.is_some()
        || config.max_sessions_per_vm.is_some()
        || config.max_prompts_per_session.is_some()
        || config.max_prompts_per_vm.is_some()
        || config.max_pending_permissions_per_session.is_some()
        || config.max_pending_permissions_per_vm.is_some()
        || config.max_permission_outcomes_per_session.is_some()
        || config.max_permission_outcomes_per_vm.is_some()
}

fn legacy_has_js_runtime_limits(config: &agentos_vm_config::JsRuntimeLimitsConfig) -> bool {
    config.v8_heap_limit_mb.is_some()
        || config.sync_rpc_wait_timeout_ms.is_some()
        || config.cpu_time_limit_ms.is_some()
        || config.wall_clock_limit_ms.is_some()
        || config.import_cache_materialize_timeout_ms.is_some()
        || config.captured_output_limit_bytes.is_some()
        || config.stdin_buffer_limit_bytes.is_some()
        || config.event_payload_limit_bytes.is_some()
        || config.max_timers.is_some()
        || config.v8_ipc_max_frame_bytes.is_some()
}

fn legacy_has_python_limits(config: &agentos_vm_config::PythonLimitsConfig) -> bool {
    config.output_buffer_max_bytes.is_some()
        || config.execution_timeout_ms.is_some()
        || config.max_old_space_mb.is_some()
        || config.vfs_rpc_timeout_ms.is_some()
}

fn legacy_has_wasm_limits(config: &agentos_vm_config::WasmLimitsConfig) -> bool {
    config.max_module_file_bytes.is_some()
        || config.captured_output_limit_bytes.is_some()
        || config.sync_read_limit_bytes.is_some()
        || config.prewarm_timeout_ms.is_some()
        || config.runner_heap_limit_mb.is_some()
        || config.active_cpu_time_limit_ms.is_some()
        || config.wall_clock_limit_ms.is_some()
        || config.deterministic_fuel.is_some()
}

fn legacy_has_process_limits(config: &agentos_vm_config::ProcessLimitsConfig) -> bool {
    config.max_spawn_file_actions.is_some()
        || config.max_spawn_file_action_bytes.is_some()
        || config.pending_stdin_bytes.is_some()
        || config.pending_event_count.is_some()
        || config.pending_event_bytes.is_some()
        || config.max_pending_child_sync_count.is_some()
        || config.max_pending_child_sync_bytes.is_some()
}

// Ownership-scope constructor ergonomics. The generated BARE union exposes only the
// tuple-wrapped variants (`ConnectionOwnership`/`SessionOwnership`/`VmOwnership`); restore
// the hand-written `connection`/`session`/`vm` helpers the sidecar relies on. These live in
// `wire` (not `protocol`) for the same orphan-rule reason as the impls above: `protocol.rs`
// is `#[path]`-included by integration tests where the generated type is foreign.
impl crate::generated_protocol::v1::OwnershipScope {
    pub fn connection(connection_id: impl Into<String>) -> Self {
        Self::ConnectionOwnership(crate::generated_protocol::v1::ConnectionOwnership {
            connection_id: connection_id.into(),
        })
    }

    pub fn session(connection_id: impl Into<String>, session_id: impl Into<String>) -> Self {
        Self::SessionOwnership(crate::generated_protocol::v1::SessionOwnership {
            connection_id: connection_id.into(),
            session_id: session_id.into(),
        })
    }

    pub fn vm(
        connection_id: impl Into<String>,
        session_id: impl Into<String>,
        vm_id: impl Into<String>,
    ) -> Self {
        Self::VmOwnership(crate::generated_protocol::v1::VmOwnership {
            connection_id: connection_id.into(),
            session_id: session_id.into(),
            vm_id: vm_id.into(),
        })
    }
}

pub const PROTOCOL_NAME: &str = "agentos-native-sidecar";
pub const PROTOCOL_VERSION: u16 = 8;
// 16 MiB: large enough to carry a trusted-client CreateVm config that inlines an
// entire base-filesystem snapshot, while still bounding a single frame.
pub const DEFAULT_MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolCodecError {
    TruncatedFrame {
        actual: usize,
    },
    LengthPrefixMismatch {
        declared: usize,
        actual: usize,
    },
    FrameTooLarge {
        size: usize,
        max: usize,
    },
    UnsupportedSchema {
        name: String,
        version: u16,
    },
    InvalidRequestId,
    InvalidRequestDirection {
        request_id: RequestId,
        expected: RequestDirection,
    },
    EmptyOwnershipField {
        field: &'static str,
    },
    EmptyAuthToken,
    InvalidOwnershipScope {
        required: OwnershipRequirement,
        actual: OwnershipRequirement,
    },
    SerializeFailure(String),
    DeserializeFailure(String),
}

impl fmt::Display for ProtocolCodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TruncatedFrame { actual } => {
                write!(
                    f,
                    "protocol frame is truncated: only {actual} bytes provided"
                )
            }
            Self::LengthPrefixMismatch { declared, actual } => write!(
                f,
                "protocol frame length prefix mismatch: declared {declared} bytes, got {actual}",
            ),
            Self::FrameTooLarge { size, max } => {
                write!(f, "protocol frame is {size} bytes, limit is {max}")
            }
            Self::UnsupportedSchema { name, version } => write!(
                f,
                "unsupported protocol schema {name}@{version}; expected {PROTOCOL_NAME}@{PROTOCOL_VERSION}",
            ),
            Self::InvalidRequestId => write!(f, "protocol request identifiers must be non-zero"),
            Self::InvalidRequestDirection {
                request_id,
                expected,
            } => write!(f, "protocol request id {request_id} must be {expected}",),
            Self::EmptyOwnershipField { field } => {
                write!(f, "protocol ownership field `{field}` cannot be empty")
            }
            Self::EmptyAuthToken => {
                write!(f, "authenticate requests require a non-empty auth token")
            }
            Self::InvalidOwnershipScope { required, actual } => write!(
                f,
                "protocol frame requires {required} ownership but carried {actual}",
            ),
            Self::SerializeFailure(message) => {
                write!(f, "protocol frame serialization failed: {message}")
            }
            Self::DeserializeFailure(message) => {
                write!(f, "protocol frame deserialization failed: {message}")
            }
        }
    }
}

impl Error for ProtocolCodecError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnershipRequirement {
    Any,
    Connection,
    Session,
    Vm,
    SessionOrVm,
}

impl fmt::Display for OwnershipRequirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Any => write!(f, "any"),
            Self::Connection => write!(f, "connection"),
            Self::Session => write!(f, "session"),
            Self::Vm => write!(f, "vm"),
            Self::SessionOrVm => write!(f, "session-or-vm"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestDirection {
    Host,
    Sidecar,
}

impl fmt::Display for RequestDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Host => write!(f, "positive"),
            Self::Sidecar => write!(f, "negative"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireDispatchResult {
    pub response: ResponseFrame,
    pub events: Vec<EventFrame>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompatDispatchResult {
    pub response: crate::protocol::ResponseFrame,
    pub events: Vec<crate::protocol::EventFrame>,
}

#[derive(Debug, Clone)]
pub struct WireFrameCodec {
    max_frame_bytes: usize,
}

impl WireFrameCodec {
    pub fn new(max_frame_bytes: usize) -> Self {
        Self { max_frame_bytes }
    }

    pub fn max_frame_bytes(&self) -> usize {
        self.max_frame_bytes
    }

    pub fn encode(&self, frame: &ProtocolFrame) -> Result<Vec<u8>, ProtocolCodecError> {
        validate_frame(frame)?;

        let payload = serde_bare::to_vec(frame)
            .map_err(|error| ProtocolCodecError::SerializeFailure(error.to_string()))?;
        if payload.len() > self.max_frame_bytes {
            return Err(ProtocolCodecError::FrameTooLarge {
                size: payload.len(),
                max: self.max_frame_bytes,
            });
        }

        let length =
            u32::try_from(payload.len()).map_err(|_| ProtocolCodecError::FrameTooLarge {
                size: payload.len(),
                max: u32::MAX as usize,
            })?;

        let mut encoded = Vec::with_capacity(4 + payload.len());
        encoded.extend_from_slice(&length.to_be_bytes());
        encoded.extend_from_slice(&payload);
        Ok(encoded)
    }

    pub fn decode(&self, bytes: &[u8]) -> Result<ProtocolFrame, ProtocolCodecError> {
        let payload = self.checked_payload(bytes)?;
        let frame = serde_bare::from_slice(payload)
            .map_err(|error| ProtocolCodecError::DeserializeFailure(error.to_string()))?;
        validate_frame(&frame)?;
        Ok(frame)
    }

    /// Encode a frame as a bare message WITHOUT the 4-byte length prefix.
    ///
    /// Stream transports (stdio) use [`encode`] so frames can be delimited in a
    /// byte stream. Message transports where the boundary is the call itself
    /// (the browser `pushFrame` / `postMessage` path) use this so the on-wire
    /// bytes match the TypeScript `encodeProtocolFramePayload(frame, "bare")`,
    /// which emits the raw bare frame with no prefix.
    pub fn encode_message(&self, frame: &ProtocolFrame) -> Result<Vec<u8>, ProtocolCodecError> {
        validate_frame(frame)?;
        let payload = serde_bare::to_vec(frame)
            .map_err(|error| ProtocolCodecError::SerializeFailure(error.to_string()))?;
        if payload.len() > self.max_frame_bytes {
            return Err(ProtocolCodecError::FrameTooLarge {
                size: payload.len(),
                max: self.max_frame_bytes,
            });
        }
        Ok(payload)
    }

    /// Decode a bare message produced by [`encode_message`] (no length prefix).
    pub fn decode_message(&self, bytes: &[u8]) -> Result<ProtocolFrame, ProtocolCodecError> {
        if bytes.len() > self.max_frame_bytes {
            return Err(ProtocolCodecError::FrameTooLarge {
                size: bytes.len(),
                max: self.max_frame_bytes,
            });
        }
        let frame = serde_bare::from_slice(bytes)
            .map_err(|error| ProtocolCodecError::DeserializeFailure(error.to_string()))?;
        validate_frame(&frame)?;
        Ok(frame)
    }

    fn checked_payload<'a>(&self, bytes: &'a [u8]) -> Result<&'a [u8], ProtocolCodecError> {
        if bytes.len() < 4 {
            return Err(ProtocolCodecError::TruncatedFrame {
                actual: bytes.len(),
            });
        }

        let declared =
            u32::from_be_bytes(bytes[..4].try_into().expect("length prefix is four bytes"))
                as usize;
        if declared > self.max_frame_bytes {
            return Err(ProtocolCodecError::FrameTooLarge {
                size: declared,
                max: self.max_frame_bytes,
            });
        }

        let actual = bytes.len() - 4;
        if declared != actual {
            return Err(ProtocolCodecError::LengthPrefixMismatch { declared, actual });
        }

        Ok(&bytes[4..])
    }
}

impl Default for WireFrameCodec {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_FRAME_BYTES)
    }
}

pub fn protocol_schema() -> ProtocolSchema {
    ProtocolSchema::current()
}

impl ProtocolSchema {
    pub fn current() -> Self {
        Self {
            name: PROTOCOL_NAME.to_string(),
            version: PROTOCOL_VERSION,
        }
    }
}

impl Default for ProtocolSchema {
    fn default() -> Self {
        Self::current()
    }
}

pub fn request_frame_to_compat(
    request: RequestFrame,
) -> Result<crate::protocol::RequestFrame, ProtocolCodecError> {
    match crate::protocol::from_generated_protocol_frame(ProtocolFrame::RequestFrame(request))? {
        crate::protocol::ProtocolFrame::Request(request) => Ok(request),
        crate::protocol::ProtocolFrame::Response(_)
        | crate::protocol::ProtocolFrame::Event(_)
        | crate::protocol::ProtocolFrame::SidecarRequest(_)
        | crate::protocol::ProtocolFrame::SidecarResponse(_)
        | crate::protocol::ProtocolFrame::Control(_) => {
            Err(ProtocolCodecError::DeserializeFailure(String::from(
                "wire request frame converted to non-request compatibility frame",
            )))
        }
    }
}

pub fn ownership_scope_to_compat(ownership: OwnershipScope) -> crate::protocol::OwnershipScope {
    crate::protocol::from_generated_ownership_scope(ownership)
}

pub fn request_payload_to_compat(
    ownership: &crate::protocol::OwnershipScope,
    payload: RequestPayload,
) -> Result<crate::protocol::RequestPayload, ProtocolCodecError> {
    match crate::protocol::from_generated_protocol_frame(ProtocolFrame::RequestFrame(
        RequestFrame {
            schema: protocol_schema(),
            request_id: 1,
            ownership: crate::protocol::to_generated_ownership_scope(ownership),
            payload,
        },
    ))? {
        crate::protocol::ProtocolFrame::Request(request) => Ok(request.payload),
        crate::protocol::ProtocolFrame::Response(_)
        | crate::protocol::ProtocolFrame::Event(_)
        | crate::protocol::ProtocolFrame::SidecarRequest(_)
        | crate::protocol::ProtocolFrame::SidecarResponse(_)
        | crate::protocol::ProtocolFrame::Control(_) => {
            Err(ProtocolCodecError::DeserializeFailure(String::from(
                "wire request payload converted to non-request compatibility frame",
            )))
        }
    }
}

pub fn response_payload_from_compat(
    ownership: &crate::protocol::OwnershipScope,
    payload: crate::protocol::ResponsePayload,
) -> Result<ResponsePayload, ProtocolCodecError> {
    match crate::protocol::to_generated_protocol_frame(&crate::protocol::ProtocolFrame::Response(
        crate::protocol::ResponseFrame::new(1, ownership.clone(), payload),
    ))? {
        ProtocolFrame::ResponseFrame(response) => Ok(response.payload),
        ProtocolFrame::RequestFrame(_)
        | ProtocolFrame::EventFrame(_)
        | ProtocolFrame::SidecarRequestFrame(_)
        | ProtocolFrame::SidecarResponseFrame(_)
        | ProtocolFrame::ControlFrame(_) => Err(ProtocolCodecError::SerializeFailure(
            String::from("compatibility response payload converted to non-response wire frame"),
        )),
    }
}

pub fn event_frame_from_compat(
    event: crate::protocol::EventFrame,
) -> Result<EventFrame, ProtocolCodecError> {
    match crate::protocol::to_generated_protocol_frame(&crate::protocol::ProtocolFrame::Event(
        event,
    ))? {
        ProtocolFrame::EventFrame(event) => Ok(event),
        ProtocolFrame::RequestFrame(_)
        | ProtocolFrame::ResponseFrame(_)
        | ProtocolFrame::SidecarRequestFrame(_)
        | ProtocolFrame::SidecarResponseFrame(_)
        | ProtocolFrame::ControlFrame(_) => Err(ProtocolCodecError::SerializeFailure(
            String::from("compatibility event converted to non-event wire frame"),
        )),
    }
}

pub fn event_frame_to_compat(
    event: EventFrame,
) -> Result<crate::protocol::EventFrame, ProtocolCodecError> {
    match crate::protocol::from_generated_protocol_frame(ProtocolFrame::EventFrame(event))? {
        crate::protocol::ProtocolFrame::Event(event) => Ok(event),
        crate::protocol::ProtocolFrame::Request(_)
        | crate::protocol::ProtocolFrame::Response(_)
        | crate::protocol::ProtocolFrame::SidecarRequest(_)
        | crate::protocol::ProtocolFrame::SidecarResponse(_)
        | crate::protocol::ProtocolFrame::Control(_) => {
            Err(ProtocolCodecError::DeserializeFailure(String::from(
                "wire event converted to non-event compatibility frame",
            )))
        }
    }
}

pub fn sidecar_request_frame_from_compat(
    request: crate::protocol::SidecarRequestFrame,
) -> Result<SidecarRequestFrame, ProtocolCodecError> {
    match crate::protocol::to_generated_protocol_frame(
        &crate::protocol::ProtocolFrame::SidecarRequest(request),
    )? {
        ProtocolFrame::SidecarRequestFrame(request) => Ok(request),
        ProtocolFrame::RequestFrame(_)
        | ProtocolFrame::ResponseFrame(_)
        | ProtocolFrame::EventFrame(_)
        | ProtocolFrame::SidecarResponseFrame(_)
        | ProtocolFrame::ControlFrame(_) => {
            Err(ProtocolCodecError::SerializeFailure(String::from(
                "compatibility sidecar request converted to non-sidecar-request wire frame",
            )))
        }
    }
}

pub fn sidecar_request_payload_to_compat(
    ownership: &crate::protocol::OwnershipScope,
    payload: SidecarRequestPayload,
) -> Result<crate::protocol::SidecarRequestPayload, ProtocolCodecError> {
    match crate::protocol::from_generated_protocol_frame(ProtocolFrame::SidecarRequestFrame(
        SidecarRequestFrame {
            schema: protocol_schema(),
            request_id: -1,
            ownership: crate::protocol::to_generated_ownership_scope(ownership),
            payload,
        },
    ))? {
        crate::protocol::ProtocolFrame::SidecarRequest(request) => Ok(request.payload),
        crate::protocol::ProtocolFrame::Request(_)
        | crate::protocol::ProtocolFrame::Response(_)
        | crate::protocol::ProtocolFrame::Event(_)
        | crate::protocol::ProtocolFrame::SidecarResponse(_)
        | crate::protocol::ProtocolFrame::Control(_) => {
            Err(ProtocolCodecError::DeserializeFailure(String::from(
                "wire sidecar request payload converted to non-sidecar-request compatibility frame",
            )))
        }
    }
}

pub fn sidecar_response_frame_to_compat(
    response: SidecarResponseFrame,
) -> Result<crate::protocol::SidecarResponseFrame, ProtocolCodecError> {
    match crate::protocol::from_generated_protocol_frame(ProtocolFrame::SidecarResponseFrame(
        response,
    ))? {
        crate::protocol::ProtocolFrame::SidecarResponse(response) => Ok(response),
        crate::protocol::ProtocolFrame::Request(_)
        | crate::protocol::ProtocolFrame::Response(_)
        | crate::protocol::ProtocolFrame::Event(_)
        | crate::protocol::ProtocolFrame::SidecarRequest(_)
        | crate::protocol::ProtocolFrame::Control(_) => {
            Err(ProtocolCodecError::DeserializeFailure(String::from(
                "wire sidecar response converted to non-sidecar-response compatibility frame",
            )))
        }
    }
}

pub fn sidecar_response_frame_from_compat(
    response: crate::protocol::SidecarResponseFrame,
) -> Result<SidecarResponseFrame, ProtocolCodecError> {
    match crate::protocol::to_generated_protocol_frame(
        &crate::protocol::ProtocolFrame::SidecarResponse(response),
    )? {
        ProtocolFrame::SidecarResponseFrame(response) => Ok(response),
        ProtocolFrame::RequestFrame(_)
        | ProtocolFrame::ResponseFrame(_)
        | ProtocolFrame::EventFrame(_)
        | ProtocolFrame::SidecarRequestFrame(_)
        | ProtocolFrame::ControlFrame(_) => {
            Err(ProtocolCodecError::SerializeFailure(String::from(
                "compatibility sidecar response converted to non-sidecar-response wire frame",
            )))
        }
    }
}

pub fn dispatch_result_from_compat(
    result: CompatDispatchResult,
) -> Result<WireDispatchResult, ProtocolCodecError> {
    let response = match crate::protocol::to_generated_protocol_frame(
        &crate::protocol::ProtocolFrame::Response(result.response),
    )? {
        ProtocolFrame::ResponseFrame(response) => response,
        ProtocolFrame::RequestFrame(_)
        | ProtocolFrame::EventFrame(_)
        | ProtocolFrame::SidecarRequestFrame(_)
        | ProtocolFrame::SidecarResponseFrame(_)
        | ProtocolFrame::ControlFrame(_) => {
            return Err(ProtocolCodecError::SerializeFailure(String::from(
                "compatibility dispatch response converted to non-response wire frame",
            )));
        }
    };

    let events = result
        .events
        .into_iter()
        .map(|event| {
            match crate::protocol::to_generated_protocol_frame(
                &crate::protocol::ProtocolFrame::Event(event),
            )? {
                ProtocolFrame::EventFrame(event) => Ok(event),
                ProtocolFrame::RequestFrame(_)
                | ProtocolFrame::ResponseFrame(_)
                | ProtocolFrame::SidecarRequestFrame(_)
                | ProtocolFrame::SidecarResponseFrame(_)
                | ProtocolFrame::ControlFrame(_) => Err(ProtocolCodecError::SerializeFailure(
                    String::from("compatibility dispatch event converted to non-event wire frame"),
                )),
            }
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(WireDispatchResult { response, events })
}

fn validate_frame(frame: &ProtocolFrame) -> Result<(), ProtocolCodecError> {
    match frame {
        ProtocolFrame::RequestFrame(frame) => {
            validate_schema(&frame.schema)?;
            validate_request_id(frame.request_id)
        }
        ProtocolFrame::ResponseFrame(frame) => {
            validate_schema(&frame.schema)?;
            validate_request_id(frame.request_id)
        }
        ProtocolFrame::EventFrame(frame) => validate_schema(&frame.schema),
        ProtocolFrame::SidecarRequestFrame(frame) => {
            validate_schema(&frame.schema)?;
            validate_request_id(frame.request_id)
        }
        ProtocolFrame::SidecarResponseFrame(frame) => {
            validate_schema(&frame.schema)?;
            validate_request_id(frame.request_id)
        }
        ProtocolFrame::ControlFrame(frame) => validate_schema(&frame.schema),
    }
}

fn validate_schema(schema: &ProtocolSchema) -> Result<(), ProtocolCodecError> {
    if schema.name != PROTOCOL_NAME || schema.version != PROTOCOL_VERSION {
        return Err(ProtocolCodecError::UnsupportedSchema {
            name: schema.name.clone(),
            version: schema.version,
        });
    }
    Ok(())
}

fn validate_request_id(request_id: RequestId) -> Result<(), ProtocolCodecError> {
    if request_id == 0 {
        return Err(ProtocolCodecError::InvalidRequestId);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generated_protocol::v1::{
        FsPermissionScope, PatternPermissionScope, PermissionMode,
    };
    use std::collections::BTreeMap;

    #[test]
    fn legacy_metadata_preserves_js_runtime_limits_with_only_new_fields() {
        let metadata = BTreeMap::from([(
            String::from("limits.js_runtime.cpu_time_limit_ms"),
            String::from("123"),
        )]);

        let config = legacy_limits_config(&metadata).expect("limits config");
        let js_runtime = config.js_runtime.expect("js runtime limits");

        assert_eq!(js_runtime.cpu_time_limit_ms, Some(123));
    }

    #[test]
    fn legacy_metadata_preserves_wasm_limits_with_only_new_fields() {
        let metadata = BTreeMap::from([(
            String::from("limits.wasm.prewarm_timeout_ms"),
            String::from("456"),
        )]);

        let config = legacy_limits_config(&metadata).expect("limits config");
        let wasm = config.wasm.expect("wasm limits");

        assert_eq!(wasm.prewarm_timeout_ms, Some(456));
    }

    #[test]
    fn legacy_metadata_preserves_wasm_runner_heap_limit_as_only_new_field() {
        let metadata = BTreeMap::from([(
            String::from("limits.wasm.runner_heap_limit_mb"),
            String::from("789"),
        )]);

        let config = legacy_limits_config(&metadata).expect("limits config");
        let wasm = config.wasm.expect("wasm limits");

        assert_eq!(wasm.runner_heap_limit_mb, Some(789));
    }

    #[test]
    fn legacy_metadata_preserves_wasm_cpu_fields() {
        let metadata = BTreeMap::from([
            (
                String::from("limits.wasm.active_cpu_time_limit_ms"),
                String::from("987"),
            ),
            (
                String::from("limits.wasm.wall_clock_limit_ms"),
                String::from("654"),
            ),
            (
                String::from("limits.wasm.deterministic_fuel"),
                String::from("321"),
            ),
        ]);

        let config = legacy_limits_config(&metadata).expect("limits config");
        let wasm = config.wasm.expect("wasm limits");

        assert_eq!(wasm.active_cpu_time_limit_ms, Some(987));
        assert_eq!(wasm.wall_clock_limit_ms, Some(654));
        assert_eq!(wasm.deterministic_fuel, Some(321));
    }

    #[test]
    fn permissions_policy_default_matches_no_policy_deny_all() {
        let policy = PermissionsPolicy::default();

        assert!(matches!(
            policy.fs,
            Some(FsPermissionScope::PermissionMode(PermissionMode::Deny))
        ));
        for scope in [
            policy.network,
            policy.child_process,
            policy.process,
            policy.env,
            policy.binding,
        ] {
            assert!(matches!(
                scope,
                Some(PatternPermissionScope::PermissionMode(PermissionMode::Deny))
            ));
        }
    }

    #[test]
    fn permissions_policy_allow_all_remains_explicit() {
        let policy = PermissionsPolicy::allow_all();

        assert!(matches!(
            policy.fs,
            Some(FsPermissionScope::PermissionMode(PermissionMode::Allow))
        ));
        for scope in [
            policy.network,
            policy.child_process,
            policy.process,
            policy.env,
            policy.binding,
        ] {
            assert!(matches!(
                scope,
                Some(PatternPermissionScope::PermissionMode(
                    PermissionMode::Allow
                ))
            ));
        }
    }
}
