use agent_os_sidecar_browser::scaffold;

#[test]
fn browser_sidecar_scaffold_stays_on_main_thread_with_shared_kernel() {
    let scaffold = scaffold();

    assert_eq!(scaffold.package_name, "agent-os-sidecar-browser");
    assert_eq!(scaffold.kernel_package, "agent-os-kernel");
    assert_eq!(scaffold.execution_host_thread, "main");
    assert_eq!(scaffold.guest_worker_owner_thread, "main");
}
