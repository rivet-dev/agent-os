use agent_os_sidecar::scaffold;

#[test]
fn native_sidecar_scaffold_tracks_kernel_and_execution_dependencies() {
    let scaffold = scaffold();

    assert_eq!(scaffold.package_name, "agent-os-sidecar");
    assert_eq!(scaffold.binary_name, "agent-os-sidecar");
    assert_eq!(scaffold.kernel_package, "agent-os-kernel");
    assert_eq!(scaffold.execution_package, "agent-os-execution");
    assert_eq!(scaffold.protocol_name, "agent-os-sidecar");
    assert_eq!(scaffold.protocol_version, 1);
    assert_eq!(scaffold.max_frame_bytes, 1024 * 1024);
}
