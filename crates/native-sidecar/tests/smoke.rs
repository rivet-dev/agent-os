use agentos_native_sidecar::scaffold;
use agentos_native_sidecar::wire::{DEFAULT_MAX_FRAME_BYTES, PROTOCOL_NAME, PROTOCOL_VERSION};
use agentos_native_sidecar::NativeSidecarConfig;

#[test]
fn native_sidecar_scaffold_tracks_kernel_and_execution_dependencies() {
    let scaffold = scaffold();

    assert_eq!(scaffold.package_name, "agentos-native-sidecar");
    assert_eq!(scaffold.binary_name, "agentos-native-sidecar");
    assert_eq!(scaffold.kernel_package, "agentos-kernel");
    assert_eq!(scaffold.execution_package, "agentos-executor-contract");
    assert_eq!(scaffold.protocol_name, PROTOCOL_NAME);
    assert_eq!(scaffold.protocol_version, PROTOCOL_VERSION);
    assert_eq!(scaffold.max_frame_bytes, DEFAULT_MAX_FRAME_BYTES);
    assert_eq!(
        NativeSidecarConfig::default().sidecar_id,
        "agentos-native-sidecar"
    );
}
