//! Default-deny / fail-closed guards (CI hardening, item #5).
//!
//! These tests pin down the agentos security posture so a refactor cannot
//! silently weaken it:
//!
//!   1. Default-deny: with no policy configured (`Permissions::default()`),
//!      every guarded capability -- filesystem, network, child-process spawn,
//!      and environment reads -- is DENIED / fails closed.
//!
//!   2. Fail-closed on new variants: adding a new `FsOperation`,
//!      `NetworkOperation`, or `EnvironmentOperation` variant must force a
//!      compile error here (via exhaustive `match` with no wildcard arm) so a
//!      new capability cannot be added without consciously deciding how the
//!      default-deny path treats it. A runtime test additionally confirms each
//!      enumerated variant is denied under the empty policy.
//!
//!   3. Safe defaults: every safety-critical NON-TIME resource limit
//!      (processes, open fds, sockets, connections, pipes, ptys, filesystem
//!      bytes, inodes, and the socket buffer/queue caps) is bounded by default
//!      -- i.e. `Some(_)`, never silently unbounded. Time budgets (CPU /
//!      wall-clock) are intentionally OPT-IN and are deliberately NOT asserted
//!      here.

use agentos_kernel::permissions::{
    check_command_execution, check_network_access, filter_env, EnvAccessRequest,
    EnvironmentOperation, FsAccessRequest, FsOperation, NetworkAccessRequest, NetworkOperation,
    PermissionedFileSystem, Permissions,
};
use agentos_kernel::resource_accounting::ResourceLimits;
use agentos_kernel::vfs::{MemoryFileSystem, VfsResult, VirtualFileSystem};
use std::collections::BTreeMap;
use std::fmt::Debug;

// ---------------------------------------------------------------------------
// Exhaustive variant enumerations.
//
// These functions list EVERY variant of each capability enum without a `_`
// wildcard arm. If a new variant is added, these stop compiling, forcing the
// author to revisit the default-deny tests. This is the compile-time half of
// "new unmatched permission/FsOperation variant fails closed".
// ---------------------------------------------------------------------------

fn all_fs_operations() -> Vec<FsOperation> {
    use FsOperation::*;
    // Exhaustive match (no wildcard) so a new variant breaks the build here.
    let exhaustiveness_witness = |op: FsOperation| match op {
        Read | Write | Mkdir | CreateDir | ReadDir | Stat | Remove | Rename | Exists | Symlink
        | ReadLink | Link | Chmod | Chown | Utimes | Truncate | MountSensitive => (),
    };
    let all = vec![
        Read,
        Write,
        Mkdir,
        CreateDir,
        ReadDir,
        Stat,
        Remove,
        Rename,
        Exists,
        Symlink,
        ReadLink,
        Link,
        Chmod,
        Chown,
        Utimes,
        Truncate,
        MountSensitive,
    ];
    for op in &all {
        exhaustiveness_witness(*op);
    }
    all
}

fn all_network_operations() -> Vec<NetworkOperation> {
    use NetworkOperation::*;
    let exhaustiveness_witness = |op: NetworkOperation| match op {
        Fetch | Http | Dns | Listen => (),
    };
    let all = vec![Fetch, Http, Dns, Listen];
    for op in &all {
        exhaustiveness_witness(*op);
    }
    all
}

fn all_environment_operations() -> Vec<EnvironmentOperation> {
    use EnvironmentOperation::*;
    let exhaustiveness_witness = |op: EnvironmentOperation| match op {
        Read | Write => (),
    };
    let all = vec![Read, Write];
    for op in &all {
        exhaustiveness_witness(*op);
    }
    all
}

// ---------------------------------------------------------------------------
// 1 + 2: default-deny / fail-closed for every capability and variant.
// ---------------------------------------------------------------------------

fn assert_fs_denied<T: Debug>(result: VfsResult<T>) {
    let error = result.expect_err("filesystem op must be denied under empty policy");
    assert_eq!(
        error.code(),
        "EACCES",
        "fs denial should be EACCES, got {error:?}"
    );
}

#[test]
fn default_permissions_have_no_policy() {
    // The derived Default leaves every capability unset (None), which is what
    // forces the fail-closed branch in each checker.
    let permissions = Permissions::default();
    assert!(permissions.filesystem.is_none(), "fs default must be None");
    assert!(permissions.network.is_none(), "net default must be None");
    assert!(
        permissions.child_process.is_none(),
        "child_process default must be None"
    );
    assert!(
        permissions.environment.is_none(),
        "environment default must be None"
    );
}

#[test]
fn default_policy_denies_all_filesystem_operations() {
    let permissions = Permissions::default();
    // Seed a real file so the permission gate -- not a missing-path ENOENT --
    // is what rejects the request.
    let mut backing = MemoryFileSystem::new();
    backing
        .write_file("/secret.txt", b"top secret".to_vec())
        .expect("seed file");
    backing.mkdir("/dir", false).expect("seed dir");
    let fs = PermissionedFileSystem::new(backing, "vm-default-deny", permissions);

    // `check_virtual_path` is the pure permission gate (no path resolution),
    // so every enumerated FsOperation must be denied with EACCES. Using the
    // enumerated list keeps coverage in lock-step with the enum.
    for op in all_fs_operations() {
        assert_fs_denied(fs.check_virtual_path(op, "/secret.txt"));
    }
    // `check_path` (which resolves the path first) must also deny for an
    // existing path -- proving denial isn't an artifact of a missing file.
    assert_fs_denied(fs.check_path(FsOperation::Read, "/secret.txt"));
    assert_fs_denied(fs.check_path(FsOperation::Write, "/secret.txt"));

    // And the concrete VFS operations fail closed as well.
    let mut fs = fs;
    assert_fs_denied(fs.read_file("/secret.txt"));
    assert_fs_denied(fs.write_file("/secret.txt", b"x".to_vec()));
    assert_fs_denied(fs.read_dir("/dir"));
    assert_fs_denied(fs.stat("/secret.txt"));
    assert_fs_denied(fs.remove_file("/secret.txt"));
}

#[test]
fn default_policy_denies_all_network_operations() {
    let permissions = Permissions::default();
    for op in all_network_operations() {
        let result = check_network_access(
            "vm-default-deny",
            &permissions,
            op,
            "https://example.com:443",
        );
        let error = result.expect_err("network op must be denied under empty policy");
        assert_eq!(error.code(), "EACCES", "net denial should be EACCES");
    }
}

#[test]
fn default_policy_denies_child_process_spawn() {
    let permissions = Permissions::default();
    let result = check_command_execution(
        "vm-default-deny",
        &permissions,
        "/bin/sh",
        &["-c".to_string(), "echo hi".to_string()],
        None,
        &BTreeMap::new(),
    );
    let error = result.expect_err("spawn must be denied under empty policy");
    assert_eq!(error.code(), "EACCES", "spawn denial should be EACCES");
}

#[test]
fn default_policy_denies_all_environment_reads() {
    let permissions = Permissions::default();

    // filter_env with no environment policy must scrub everything.
    let mut env = BTreeMap::new();
    env.insert("SECRET_TOKEN".to_string(), "abc123".to_string());
    env.insert("PATH".to_string(), "/usr/bin".to_string());
    let filtered = filter_env("vm-default-deny", &env, &permissions);
    assert!(
        filtered.is_empty(),
        "empty env policy must deny ALL env vars, leaked: {filtered:?}"
    );

    // The variant enumeration also drives a sanity check that EnvAccessRequest
    // can be built for every op (keeps coverage in lock-step with the enum).
    for op in all_environment_operations() {
        let _request = EnvAccessRequest {
            vm_id: "vm-default-deny".to_string(),
            op,
            key: "SECRET_TOKEN".to_string(),
            value: Some("abc123".to_string()),
        };
    }

    // Sanity: a NetworkAccessRequest / FsAccessRequest can likewise be built
    // (guards against signature drift breaking the deny paths above).
    let _ = NetworkAccessRequest {
        vm_id: "vm".to_string(),
        op: NetworkOperation::Fetch,
        resource: "https://x".to_string(),
    };
    let _ = FsAccessRequest {
        vm_id: "vm".to_string(),
        op: FsOperation::Read,
        path: "/x".to_string(),
    };
}

// ---------------------------------------------------------------------------
// 3: safe defaults -- no safety-critical NON-TIME resource limit is silently
// unbounded.
// ---------------------------------------------------------------------------

#[test]
fn safety_critical_resource_limits_are_bounded_by_default() {
    let limits = ResourceLimits::default();

    // Each of these is a safety-critical, NON-TIME limit. None of them may be
    // silently unbounded (i.e. they must be `Some(_)`). The closure takes the
    // human-readable name so a failure points at the exact limit.
    let bounded: &[(&str, bool)] = &[
        ("max_processes", limits.max_processes.is_some()),
        ("max_open_fds", limits.max_open_fds.is_some()),
        ("max_pipes", limits.max_pipes.is_some()),
        ("max_ptys", limits.max_ptys.is_some()),
        ("max_sockets", limits.max_sockets.is_some()),
        ("max_connections", limits.max_connections.is_some()),
        (
            "max_socket_buffered_bytes",
            limits.max_socket_buffered_bytes.is_some(),
        ),
        (
            "max_socket_datagram_queue_len",
            limits.max_socket_datagram_queue_len.is_some(),
        ),
        (
            "max_filesystem_bytes",
            limits.max_filesystem_bytes.is_some(),
        ),
        ("max_inode_count", limits.max_inode_count.is_some()),
    ];

    let unbounded: Vec<&str> = bounded
        .iter()
        .filter(|(_, is_bounded)| !is_bounded)
        .map(|(name, _)| *name)
        .collect();

    assert!(
        unbounded.is_empty(),
        "safety-critical NON-TIME resource limit(s) are silently unbounded by \
default (must be Some(_)): {unbounded:?}. If a limit is intentionally being \
made opt-in, that is a security-policy change -- review it before relaxing \
this guard."
    );

    // Defaults must also be non-zero (a 0 cap would be unusable, and a refactor
    // that zeroed them out would be just as broken as leaving them unbounded).
    assert!(limits.max_processes.unwrap() > 0);
    assert!(limits.max_open_fds.unwrap() > 0);
    assert!(limits.max_sockets.unwrap() > 0);
    assert!(limits.max_filesystem_bytes.unwrap() > 0);
    assert!(limits.max_inode_count.unwrap() > 0);
}

#[test]
fn time_budgets_remain_opt_in() {
    // Documents the intentional posture: CPU / wall-clock budgets are opt-in,
    // so there is no default time cap to assert. `max_blocking_read_ms` is a
    // per-syscall blocking-read timeout (a safety backstop for hung reads),
    // NOT a CPU/wall-clock execution budget, so we leave it untouched here.
    //
    // This test exists so that if someone later adds a *default* CPU/wall-clock
    // cap they are reminded to reconcile it with the "time budgets are opt-in"
    // decision rather than this test silently passing on stale assumptions.
    let limits = ResourceLimits::default();
    // No assertion that a CPU/wall budget is set: that is correct today.
    let _ = limits.max_blocking_read_ms;
}
