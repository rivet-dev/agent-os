#![forbid(unsafe_code)]

//! Backend-agnostic sidecar logic shared by native and browser shells.

pub mod bindings;
pub mod bridge_bytes;
pub mod ca;
pub mod diagnostics;
pub mod frames;
pub mod guest_fs;
pub mod guest_net;
pub mod guest_pty;
pub mod identity;
pub mod layers;
pub mod limits;
pub mod net;
pub mod permissions;
pub mod root_fs;
pub mod router;
pub mod services;
pub mod signals;
pub mod vm_fetch;

pub use bindings::{
    ensure_binding_registry_capacity, ensure_collection_name_available,
    ensure_command_aliases_available, registered_binding_command_names,
    validate_bindings_registration, BindingRegistrationError, DEFAULT_BINDING_TIMEOUT_MS,
    MAX_BINDINGS_PER_COLLECTION, MAX_BINDING_COLLECTION_NAME_LENGTH,
    MAX_BINDING_DESCRIPTION_LENGTH, MAX_BINDING_EXAMPLE_INPUT_BYTES, MAX_BINDING_NAME_LENGTH,
    MAX_BINDING_SCHEMA_BYTES, MAX_BINDING_SCHEMA_DEPTH, MAX_BINDING_TIMEOUT_MS,
    MAX_EXAMPLES_PER_BINDING, MAX_REGISTERED_BINDINGS_PER_VM, MAX_REGISTERED_BINDING_COLLECTIONS,
};
pub use bridge_bytes::{
    bridge_buffer_value, decode_base64, decode_bridge_buffer_value, decode_encoded_bytes_value,
    encoded_bytes_value,
};
pub use diagnostics::{
    process_snapshot_entry_from_kernel, process_status_from_kernel,
    protocol_process_snapshot_entry, SharedProcessSnapshotEntry, SharedProcessSnapshotStatus,
};
pub use frames::{
    authenticated_response, bound_udp_snapshot_response, event, layer_created_response,
    layer_sealed_response, listener_snapshot_response, mounts_listed_response,
    overlay_created_response, package_linked_response, process_exited_event,
    process_killed_response, process_output_event, process_snapshot_response,
    process_started_response, provided_commands_response, reject, respond, response_with_ownership,
    root_filesystem_bootstrapped_response, root_filesystem_snapshot_response,
    session_opened_response, signal_state_response, snapshot_exported_response,
    snapshot_imported_response, stdin_closed_response, stdin_written_response,
    unsupported_guest_kernel_call_detail, unsupported_guest_kernel_call_event,
    validate_authenticate_versions, vm_configured_response, vm_created_response,
    vm_disposed_response, vm_lifecycle_event, zombie_timer_count_response,
    AuthenticateVersionError, DispatchResult, UNSUPPORTED_GUEST_KERNEL_CALL_EVENT,
};
pub use guest_fs::{
    decode_guest_filesystem_content, empty_guest_filesystem_response,
    encode_guest_filesystem_content, guest_filesystem_stat, handle_guest_filesystem_call,
    targeted_guest_filesystem_response,
};
pub use guest_net::handle_guest_kernel_call;
pub use identity::{
    shared_guest_runtime_identity, shared_guest_runtime_identity_with_system,
    SharedGuestRuntimeIdentity,
};
pub use layers::{VmLayerStore, MAX_VM_LAYERS};
pub use limits::{
    validate_vm_limits, virtual_os_cpu_count, virtual_os_freemem_bytes, virtual_os_totalmem_bytes,
    vm_limits_from_config, AcpLimits, BindingLimits, HttpLimits, JsRuntimeLimits, PluginLimits,
    PythonLimits, VmLimits, WasmLimits,
};
pub use net::{
    local_endpoint_value, remote_endpoint_value, socket_addr_family, socket_address_value,
    tcp_socket_info_value, unix_socket_info_value,
};
pub use permissions::{
    allow_all_policy, deny_all_policy, environment_permission_capability,
    evaluate_permissions_policy, filesystem_permission_capability, fs_permission_capability,
    network_permission_capability, permission_mode_to_kernel_decision, permissions_from_policy,
    validate_permissions_policy,
};
pub use root_fs::{
    apply_root_filesystem_entry, build_root_filesystem, build_root_filesystem_with_loaded_snapshot,
    build_root_mount_table, build_root_mount_table_with_loaded_snapshot,
    convert_root_filesystem_entry, protocol_root_filesystem_mode,
    root_filesystem_descriptor_from_config, root_filesystem_mode_from_config,
    root_filesystem_protocol_descriptor_from_config, root_snapshot_entry,
    root_snapshot_from_entries, SidecarCoreError,
};
pub use router::{
    connection_id_of, generated_wire_blocking_extension_interrupt, request_dispatch_mode,
    request_is_unsupported_host_callback_direction, route_request_payload, session_scope_of,
    unsupported_host_callback_direction_dispatch, vm_id_of, BlockingExtensionInterrupt,
    RequestDispatchMode, RequestRoute, UNSUPPORTED_HOST_CALLBACK_DIRECTION_CODE,
    UNSUPPORTED_HOST_CALLBACK_DIRECTION_MESSAGE,
};
pub use signals::{
    apply_process_signal_state_update, canonical_signal_name, default_signal_exit_code,
    execution_signal_from_number, execution_signal_to_kernel, is_valid_posix_signal_number,
    parse_posix_signal, parse_process_signal_state_request, signal_number_from_name,
};
pub use vm_fetch::{
    ensure_vm_fetch_raw_response_buffer_within_limit, ensure_vm_fetch_response_within_limit,
    parse_kernel_http_fetch_response, serialize_kernel_http_fetch_request,
    VM_FETCH_BUFFER_LIMIT_BYTES,
};
