use agent_os_kernel::scaffold;

#[test]
fn kernel_scaffold_targets_native_and_browser_sidecars() {
    let scaffold = scaffold();

    assert_eq!(scaffold.package_name, "agent-os-kernel");
    assert!(scaffold.supports_native_sidecar);
    assert!(scaffold.supports_browser_sidecar);
}
