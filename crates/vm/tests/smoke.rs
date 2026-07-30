use agentos_vm::scaffold;
use agentos_vm::wire::{DEFAULT_MAX_FRAME_BYTES, PROTOCOL_NAME, PROTOCOL_VERSION};
use agentos_vm::VmManagerConfig;

#[test]
fn vm_scaffold_tracks_kernel_and_execution_dependencies() {
    let scaffold = scaffold();

    assert_eq!(scaffold.package_name, "agentos-vm");
    assert_eq!(scaffold.kernel_package, "agentos-vm-kernel");
    assert_eq!(scaffold.execution_package, "agentos-executor-contract");
    assert_eq!(scaffold.protocol_name, PROTOCOL_NAME);
    assert_eq!(scaffold.protocol_version, PROTOCOL_VERSION);
    assert_eq!(scaffold.max_frame_bytes, DEFAULT_MAX_FRAME_BYTES);
    assert_eq!(VmManagerConfig::default().instance_id, "agentos-vm");
}
