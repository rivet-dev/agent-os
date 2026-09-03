//! Network permission rules must honor the documented pattern form.
//!
//! The kernel formats network resources as URIs (`tcp://host:port`,
//! `dns://host`) before the policy check, while the documented rule form is a
//! bare host or `host:port`. A scheme-less pattern must therefore be matched
//! against the host subject of the URI, and a pattern that carries a scheme
//! must keep matching the full URI. Everything else fails closed.

use agentos_native_sidecar_core::permissions::{
    evaluate_matching_pattern_permission_policy, evaluate_permissions_policy,
};
use agentos_vm_config::{
    PatternPermissionRule, PatternPermissionRuleSet, PatternPermissionScope, PermissionMode,
    PermissionsPolicy,
};

fn network_policy(
    default: PermissionMode,
    rules: Vec<(PermissionMode, &str)>,
) -> PermissionsPolicy {
    PermissionsPolicy {
        fs: None,
        network: Some(PatternPermissionScope::Rules(PatternPermissionRuleSet {
            default: Some(default),
            rules: rules
                .into_iter()
                .map(|(mode, pattern)| PatternPermissionRule {
                    mode,
                    operations: vec![String::from("*")],
                    patterns: vec![String::from(pattern)],
                })
                .collect(),
        })),
        child_process: None,
        process: None,
        env: None,
        binding: None,
    }
}

fn http(policy: &PermissionsPolicy, resource: &str) -> PermissionMode {
    evaluate_permissions_policy(policy, "network", "network.http", Some(resource))
}

fn dns(policy: &PermissionsPolicy, resource: &str) -> PermissionMode {
    evaluate_permissions_policy(policy, "network", "network.dns", Some(resource))
}

// --- Documented host form under `default: deny` (the allowlist posture) ------

#[test]
fn bare_host_allow_rule_matches_tcp_resource_on_any_port() {
    let policy = network_policy(
        PermissionMode::Deny,
        vec![(PermissionMode::Allow, "api.example.com")],
    );
    assert_eq!(
        http(&policy, "tcp://api.example.com:443"),
        PermissionMode::Allow
    );
    assert_eq!(
        http(&policy, "tcp://api.example.com:80"),
        PermissionMode::Allow
    );
    assert_eq!(
        http(&policy, "tcp://other.example.com:443"),
        PermissionMode::Deny
    );
}

#[test]
fn host_port_allow_rule_matches_only_that_port() {
    let policy = network_policy(
        PermissionMode::Deny,
        vec![(PermissionMode::Allow, "api.example.com:443")],
    );
    assert_eq!(
        http(&policy, "tcp://api.example.com:443"),
        PermissionMode::Allow
    );
    assert_eq!(
        http(&policy, "tcp://api.example.com:80"),
        PermissionMode::Deny
    );
}

#[test]
fn bare_host_allow_rule_matches_dns_resource() {
    let policy = network_policy(
        PermissionMode::Deny,
        vec![(PermissionMode::Allow, "api.example.com")],
    );
    assert_eq!(dns(&policy, "dns://api.example.com"), PermissionMode::Allow);
    assert_eq!(
        dns(&policy, "dns://other.example.com"),
        PermissionMode::Deny
    );
}

#[test]
fn subdomain_glob_matches_host_subject() {
    let policy = network_policy(
        PermissionMode::Deny,
        vec![(PermissionMode::Allow, "*.example.com")],
    );
    assert_eq!(
        http(&policy, "tcp://api.example.com:443"),
        PermissionMode::Allow
    );
    assert_eq!(dns(&policy, "dns://api.example.com"), PermissionMode::Allow);
    assert_eq!(http(&policy, "tcp://example.com:443"), PermissionMode::Deny);
    assert_eq!(
        http(&policy, "tcp://api.example.org:443"),
        PermissionMode::Deny
    );
}

#[test]
fn single_star_allow_rule_matches_every_host() {
    let policy = network_policy(PermissionMode::Deny, vec![(PermissionMode::Allow, "*")]);
    assert_eq!(
        http(&policy, "tcp://api.example.com:443"),
        PermissionMode::Allow
    );
    assert_eq!(dns(&policy, "dns://api.example.com"), PermissionMode::Allow);
}

// --- Documented host form under `default: allow` (the blocklist posture) -----
//
// This is the fail-open direction from the report: a deny rule that never
// matches silently permits every host.

#[test]
fn bare_host_deny_rule_blocks_that_host_under_default_allow() {
    let policy = network_policy(
        PermissionMode::Allow,
        vec![(PermissionMode::Deny, "example.com")],
    );
    assert_eq!(http(&policy, "tcp://example.com:443"), PermissionMode::Deny);
    assert_eq!(dns(&policy, "dns://example.com"), PermissionMode::Deny);
    assert_eq!(
        http(&policy, "tcp://api.example.com:443"),
        PermissionMode::Allow
    );
}

#[test]
fn single_star_deny_rule_blocks_every_host_under_default_allow() {
    let policy = network_policy(PermissionMode::Allow, vec![(PermissionMode::Deny, "*")]);
    assert_eq!(http(&policy, "tcp://example.com:443"), PermissionMode::Deny);
    assert_eq!(dns(&policy, "dns://example.com"), PermissionMode::Deny);
}

// --- URI form keeps working exactly as before ---------------------------------

#[test]
fn uri_patterns_still_match_the_full_resource() {
    let policy = network_policy(
        PermissionMode::Deny,
        vec![
            (PermissionMode::Allow, "tcp://api.example.com:*"),
            (PermissionMode::Allow, "dns://api.example.com"),
        ],
    );
    assert_eq!(
        http(&policy, "tcp://api.example.com:443"),
        PermissionMode::Allow
    );
    assert_eq!(dns(&policy, "dns://api.example.com"), PermissionMode::Allow);
    assert_eq!(
        http(&policy, "tcp://other.example.com:443"),
        PermissionMode::Deny
    );
}

#[test]
fn uri_pattern_for_one_scheme_does_not_match_another_scheme() {
    let policy = network_policy(
        PermissionMode::Deny,
        vec![(PermissionMode::Allow, "dns://api.example.com")],
    );
    assert_eq!(
        http(&policy, "tcp://api.example.com:443"),
        PermissionMode::Deny
    );
}

#[test]
fn double_star_uri_pattern_still_matches_every_host() {
    let policy = network_policy(
        PermissionMode::Deny,
        vec![(PermissionMode::Allow, "tcp://**")],
    );
    assert_eq!(
        http(&policy, "tcp://api.example.com:443"),
        PermissionMode::Allow
    );
    assert_eq!(dns(&policy, "dns://api.example.com"), PermissionMode::Deny);
}

// --- Fail closed on anything the subject parser cannot understand ------------

#[test]
fn bare_pattern_does_not_match_a_resource_without_a_scheme_subject() {
    // A resource the network layer never produces must not be matched by a
    // host pattern through some accidental substring equivalence.
    let policy = network_policy(PermissionMode::Deny, vec![(PermissionMode::Allow, "*")]);
    assert_eq!(http(&policy, "unix:/run/agent.sock"), PermissionMode::Deny);
    assert_eq!(http(&policy, ""), PermissionMode::Deny);
}

#[test]
fn ipv6_literal_host_matches_the_kernel_formatted_subject() {
    // The kernel formats IPv6 hosts unbracketed (`tcp://::1:8080`); only the
    // trailing `:port` is split off, so the host subject is `::1`.
    let policy = network_policy(PermissionMode::Deny, vec![(PermissionMode::Allow, "::1")]);
    assert_eq!(http(&policy, "tcp://::1:8080"), PermissionMode::Allow);
    assert_eq!(http(&policy, "tcp://fe80::1:8080"), PermissionMode::Deny);
    let port_policy = network_policy(
        PermissionMode::Deny,
        vec![(PermissionMode::Allow, "::1:8080")],
    );
    assert_eq!(http(&port_policy, "tcp://::1:8080"), PermissionMode::Allow);
    assert_eq!(http(&port_policy, "tcp://::1:9090"), PermissionMode::Deny);
}

// --- Last matching rule still wins, and host matching reaches the
//     post-resolution evaluator too ------------------------------------------

#[test]
fn later_host_rule_overrides_earlier_wildcard_rule() {
    let policy = network_policy(
        PermissionMode::Deny,
        vec![
            (PermissionMode::Allow, "*"),
            (PermissionMode::Deny, "internal.example.com"),
        ],
    );
    assert_eq!(
        http(&policy, "tcp://api.example.com:443"),
        PermissionMode::Allow
    );
    assert_eq!(
        http(&policy, "tcp://internal.example.com:443"),
        PermissionMode::Deny
    );
}

#[test]
fn matching_pattern_evaluation_uses_host_subject_for_resolved_addresses() {
    let policy = network_policy(
        PermissionMode::Allow,
        vec![(PermissionMode::Deny, "203.0.113.*")],
    );
    assert_eq!(
        evaluate_matching_pattern_permission_policy(
            &policy,
            "network",
            "network.http",
            Some("tcp://203.0.113.9:443"),
        ),
        Some(PermissionMode::Deny)
    );
    assert_eq!(
        evaluate_matching_pattern_permission_policy(
            &policy,
            "network",
            "network.http",
            Some("tcp://198.51.100.7:443"),
        ),
        None
    );
}

// --- Other pattern scopes are untouched ---------------------------------------

#[test]
fn child_process_patterns_are_not_subject_parsed() {
    let policy = PermissionsPolicy {
        fs: None,
        network: None,
        child_process: Some(PatternPermissionScope::Rules(PatternPermissionRuleSet {
            default: Some(PermissionMode::Deny),
            rules: vec![PatternPermissionRule {
                mode: PermissionMode::Allow,
                operations: vec![String::from("spawn")],
                patterns: vec![String::from("sh")],
            }],
        })),
        process: None,
        env: None,
        binding: None,
    };
    assert_eq!(
        evaluate_permissions_policy(&policy, "child_process", "child_process.spawn", Some("sh")),
        PermissionMode::Allow
    );
    assert_eq!(
        evaluate_permissions_policy(
            &policy,
            "child_process",
            "child_process.spawn",
            Some("tcp://sh:1")
        ),
        PermissionMode::Deny
    );
}
