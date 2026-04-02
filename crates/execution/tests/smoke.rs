use agent_os_execution::{scaffold, GuestRuntime};

#[test]
fn execution_scaffold_is_native_and_depends_on_kernel() {
    let scaffold = scaffold();

    assert_eq!(scaffold.package_name, "agent-os-execution");
    assert_eq!(scaffold.kernel_package, "agent-os-kernel");
    assert_eq!(scaffold.target, "native");
    assert_eq!(
        scaffold.planned_guest_runtimes,
        [GuestRuntime::JavaScript, GuestRuntime::WebAssembly]
    );
}
