#[test]
fn conformance_package_is_test_only() {
    assert_eq!(env!("CARGO_PKG_NAME"), "agentos-executor-conformance");
}
