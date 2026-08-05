//! Regression test: AgentOS must not impose an absolute ACP prompt-turn timeout.
//!
//! Long, multi-tool turns and human permission waits can legitimately run for
//! hours. ACP provides explicit `session/cancel`; a hidden transport deadline
//! must not synthesize cancellation.
//!
//! `request_timeout` is a crate-private helper, so this test loads the production ACP
//! module directly. That exercises the shipped value rather than a copy. If a future
//! refactor restores an arbitrary prompt deadline, this build fails.
//!
//! NOTE: a separate test file is used (rather than editing the inline `#[cfg(test)]`
//! module or the existing `tests/acp_extension.rs`) to keep this regression guard
//! standalone and to avoid touching shared test files or production source.

#[path = "../src/session_store.rs"]
mod session_store;

#[allow(dead_code, unused_imports, unused_variables, clippy::all)]
#[path = "../src/acp/mod.rs"]
mod under_test;

/// Prompt lifetime is controlled by completion, explicit cancellation, adapter
/// exit, VM shutdown, and the outer actor action bound.
#[test]
fn session_prompt_has_no_sidecar_deadline() {
    assert_eq!(under_test::request_timeout("session/prompt"), None);
}

/// Bootstrap and control operations retain finite failure bounds.
#[test]
fn control_methods_remain_bounded() {
    assert_eq!(
        under_test::request_timeout("session/foo"),
        Some(std::time::Duration::from_secs(120)),
        "default (non-prompt) control-method timeout is expected to be 120s"
    );
}
