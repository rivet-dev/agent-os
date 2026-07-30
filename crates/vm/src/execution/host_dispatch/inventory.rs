//! Reviewed compatibility-WASM RPC inventory.
//!
//! The architecture guard compares these exact lists with static `callSyncRpc`
//! literals in `wasm-runner.mjs` and methods hidden behind the generic
//! `process.wasm_sync_rpc` bootstrap. A new runner method must be assigned to
//! one capability family or explicitly reviewed as adapter-only.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HostCapabilityFamily {
    Filesystem,
    Network,
    Process,
    Terminal,
    Signal,
    Identity,
    Clock,
    Entropy,
}

pub(super) const WASM_RUNNER_RPC_INVENTORY: &[&str] = &[
    "__kernel_isatty",
    "__kernel_poll",
    "__kernel_stdin_read",
    "__kernel_stdio_write",
    "__kernel_tcgetattr",
    "__kernel_tcgetpgrp",
    "__kernel_tcgetsid",
    "__kernel_tcsetattr",
    "__kernel_tcsetpgrp",
    "__kernel_tty_set_size",
    "__kernel_tty_size",
    "__pty_set_raw_mode",
    "child_process.close_stdin",
    "child_process.poll",
    "child_process.spawn",
    "child_process.write_stdin",
    "dgram.bind",
    "dgram.close",
    "dgram.createSocket",
    "dgram.poll",
    "dgram.send",
    "dns.lookup",
    "dns.resolveRawRr",
    "fs.accessSync",
    "fs.blockingIoTimeoutMsSync",
    "fs.collapseRangeSync",
    "fs.fallocateSync",
    "fs.fgetxattrSync",
    "fs.fiemapAtSync",
    "fs.flistxattrSync",
    "fs.fremovexattrSync",
    "fs.fsetxattrSync",
    "fs.getxattrSync",
    "fs.insertRangeSync",
    "fs.linkFdSync",
    "fs.listxattrSync",
    "fs.mknodSync",
    "fs.namedFifoPeerReadySync",
    "fs.openTmpfileSync",
    "fs.punchHoleSync",
    "fs.remountSync",
    "fs.removexattrSync",
    "fs.renameAt2Sync",
    "fs.setxattrSync",
    "fs.statSync",
    "fs.statfsSync",
    "fs.zeroRangeSync",
    "net.bind_connected_unix",
    "net.bind_unix",
    "net.connect",
    "net.destroy",
    "net.listen",
    "net.poll",
    "net.release_tcp_port",
    "net.reserve_tcp_port",
    "net.server_accept",
    "net.server_close",
    "net.socket_read",
    "net.socket_upgrade_tls",
    "net.socket_wait_connect",
    "net.write",
    "process.clock_resolution",
    "process.clock_time",
    "process.exec",
    "process.exec_image_close",
    "process.exec_image_open",
    "process.exec_image_open_fd",
    "process.exec_image_read",
    "process.exec_fd_image_commit",
    "process.fd_chdir_path",
    "process.fd_chmod",
    "process.fd_chown",
    "process.fd_close",
    "process.fd_closefrom",
    "process.fd_datasync",
    "process.fd_description_identity",
    "process.fd_dup",
    "process.fd_dup_min",
    "process.fd_filestat",
    "process.fd_flock",
    "process.fd_getfd",
    "process.fd_move",
    "process.fd_path",
    "process.fd_pipe",
    "process.fd_pread",
    "process.fd_preopen",
    "process.fd_preopens",
    "process.fd_pwrite",
    "process.fd_read",
    "process.fd_readdir",
    "process.fd_record_lock",
    "process.fd_record_lock_cancel",
    "process.fd_recvmsg_rights",
    "process.fd_seek",
    "process.fd_sendmsg_rights",
    "process.fd_set_flags",
    "process.fd_setfd",
    "process.fd_snapshot",
    "process.fd_socket_shutdown",
    "process.fd_socketpair",
    "process.fd_stat",
    "process.fd_sync",
    "process.fd_truncate",
    "process.fd_utimes",
    "process.fd_write",
    "process.getegid",
    "process.geteuid",
    "process.getgid",
    "process.getgrent",
    "process.getgrgid",
    "process.getgrnam",
    "process.getgroups",
    "process.getpgid",
    "process.getpwent",
    "process.getpwnam",
    "process.getpwuid",
    "process.getresgid",
    "process.getresuid",
    "process.getrlimit",
    "process.getuid",
    "process.hostnet_accept",
    "process.hostnet_bind",
    "process.hostnet_connect",
    "process.hostnet_fd_open",
    "process.hostnet_get_option",
    "process.hostnet_listen",
    "process.hostnet_local_address",
    "process.hostnet_peer_address",
    "process.hostnet_poll",
    "process.hostnet_recv",
    "process.hostnet_send",
    "process.hostnet_set_option",
    "process.hostnet_tls_connect",
    "process.hostnet_validate",
    "process.itimer_real",
    "process.kill",
    "process.path_chmod_at",
    "process.path_chown_at",
    "process.path_link_at",
    "process.path_mkdir_at",
    "process.path_open_at",
    "process.path_readlink_at",
    "process.path_remove_dir_at",
    "process.path_rename_at",
    "process.path_stat_at",
    "process.path_statfs_at",
    "process.path_symlink_at",
    "process.path_unlink_at",
    "process.path_utimes_at",
    "process.posix_poll",
    "process.pty_open",
    "process.random_get",
    "process.setegid",
    "process.seteuid",
    "process.setgid",
    "process.setgroups",
    "process.setpgid",
    "process.setregid",
    "process.setresgid",
    "process.setresuid",
    "process.setreuid",
    "process.setrlimit",
    "process.setuid",
    "process.signal_end",
    "process.signal_mask",
    "process.signal_state",
    "process.sleep",
    "process.system_identity",
    "process.take_signal",
    "process.umask",
    "process.waitpid",
    "process.waitpid_transition",
];

/// Semantic RPCs emitted only through the generic `process.wasm_sync_rpc`
/// wrapper in the V8 compatibility bootstrap. They do not appear as literal
/// `callSyncRpc(...)` targets in `wasm-runner.mjs`, so keeping this reviewed
/// delta frozen prevents Linux operations from silently bypassing typed host
/// dispatch behind the wrapper.
pub(super) const WASM_WRAPPED_ONLY_RPC_INVENTORY: &[&str] = &[
    "fs.chmodForProcessSync",
    "fs.chownSync",
    "fs.fiemapSync",
    "fs.lchownSync",
    "fs.truncateForProcessSync",
    "process.fd_description_alias_count",
    "process.fd_dup2",
    "process.fd_open",
    "process.image",
    "process.signal_begin",
    "process.signal_mask_scope_begin",
    "process.signal_mask_scope_end",
];

/// These calls coordinate the current V8 compatibility adapter rather than
/// representing a guest Linux operation. They may remain on the legacy bridge
/// until the corresponding runner projection is deleted.
pub(super) const WASM_ADAPTER_ONLY_RPCS: &[&str] = &[
    "fs.blockingIoTimeoutMsSync",
    "process.fd_description_alias_count",
    "process.fd_description_identity",
    "process.fd_snapshot",
];

pub(super) fn capability_family(method: &str) -> Option<HostCapabilityFamily> {
    if WASM_ADAPTER_ONLY_RPCS.contains(&method) {
        return None;
    }
    if method.starts_with("child_process.")
        || matches!(
            method,
            "process.exec"
                | "process.exec_fd_image_commit"
                | "process.exec_image_close"
                | "process.exec_image_open"
                | "process.exec_image_open_fd"
                | "process.exec_image_read"
                | "process.getpgid"
                | "process.image"
                | "process.getrlimit"
                | "process.kill"
                | "process.setpgid"
                | "process.setrlimit"
                | "process.system_identity"
                | "process.umask"
                | "process.waitpid"
                | "process.waitpid_transition"
        )
    {
        return Some(HostCapabilityFamily::Process);
    }
    if matches!(
        method,
        "process.getuid"
            | "process.getgid"
            | "process.geteuid"
            | "process.getegid"
            | "process.getresuid"
            | "process.getresgid"
            | "process.getgroups"
            | "process.getpwuid"
            | "process.getpwnam"
            | "process.getpwent"
            | "process.getgrgid"
            | "process.getgrnam"
            | "process.getgrent"
            | "process.setuid"
            | "process.seteuid"
            | "process.setreuid"
            | "process.setresuid"
            | "process.setgid"
            | "process.setegid"
            | "process.setregid"
            | "process.setresgid"
            | "process.setgroups"
    ) {
        return Some(HostCapabilityFamily::Identity);
    }
    if method.starts_with("process.signal_") || method == "process.take_signal" {
        return Some(HostCapabilityFamily::Signal);
    }
    if matches!(
        method,
        "process.clock_time" | "process.clock_resolution" | "process.itimer_real" | "process.sleep"
    ) {
        return Some(HostCapabilityFamily::Clock);
    }
    if method == "process.random_get" {
        return Some(HostCapabilityFamily::Entropy);
    }
    if method.starts_with("__kernel_tc")
        || method.starts_with("__kernel_tty")
        || method == "__kernel_isatty"
        || method == "__pty_set_raw_mode"
        || method == "process.pty_open"
    {
        return Some(HostCapabilityFamily::Terminal);
    }
    if method.starts_with("net.")
        || method.starts_with("dgram.")
        || method.starts_with("dns.")
        || method.starts_with("process.hostnet_")
        || matches!(
            method,
            "__kernel_poll"
                | "process.posix_poll"
                | "process.fd_recvmsg_rights"
                | "process.fd_sendmsg_rights"
                | "process.fd_socket_shutdown"
                | "process.fd_socketpair"
        )
    {
        return Some(HostCapabilityFamily::Network);
    }
    if method.starts_with("fs.")
        || method.starts_with("process.fd_")
        || method.starts_with("process.path_")
        || matches!(method, "__kernel_stdin_read" | "__kernel_stdio_write")
    {
        return Some(HostCapabilityFamily::Filesystem);
    }
    None
}

#[cfg(test)]
pub(super) fn semantic_rpc_inventory() -> std::collections::BTreeSet<&'static str> {
    WASM_RUNNER_RPC_INVENTORY
        .iter()
        .chain(WASM_WRAPPED_ONLY_RPC_INVENTORY)
        .copied()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn every_frozen_semantic_rpc_is_typed_or_explicitly_adapter_only() {
        let direct = WASM_RUNNER_RPC_INVENTORY
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        assert_eq!(direct.len(), WASM_RUNNER_RPC_INVENTORY.len());
        let wrapped_only = WASM_WRAPPED_ONLY_RPC_INVENTORY
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        assert_eq!(wrapped_only.len(), WASM_WRAPPED_ONLY_RPC_INVENTORY.len());
        assert!(direct.is_disjoint(&wrapped_only));
        let inventory = semantic_rpc_inventory();
        let adapter_only = WASM_ADAPTER_ONLY_RPCS
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        assert_eq!(adapter_only.len(), WASM_ADAPTER_ONLY_RPCS.len());
        assert!(adapter_only.is_subset(&inventory));
        for method in inventory {
            assert_eq!(
                capability_family(method).is_some(),
                !adapter_only.contains(method),
                "{method} must have exactly one reviewed route",
            );
        }
    }
}
