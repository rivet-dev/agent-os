# Codex patches for agentOS

Codex targets agentOS's Linux-in-WASM environment. Although Rust identifies the
target as `wasm32-wasip1` / `target_os = "wasi"`, agentOS supplies an owned POSIX
sysroot, libc, Rust standard library, and host imports. The target therefore has
real executable modes, symlinks, sockets, processes, PTYs, and signals.

Patches in `../codex/` may expose those existing capabilities through API names
that upstream crates normally guard with `cfg(unix)`. A missing declaration or
operation must be fixed in this order:

1. the agentOS Rust standard library;
2. the agentOS libc and C sysroot;
3. the kernel/sidecar host import;
4. a narrow Codex or dependency cfg patch that selects the real implementation.

Do not replace POSIX behavior with `Unsupported`, a hard-coded capability value,
an empty result, or a success-returning no-op merely because the target is named
WASI. The `portable-pty` compatibility crate is backed by agentOS's real PTY,
process, descriptor, terminal-size, wait, and signal host interfaces. The build
removes the fork's stale `ctrlc` override (the session-turn dependency graph does
not use it) and uses the real AWS authentication crate.

The one dependency-level adapter is `toolchain/stubs/reqwest-shim`. It is not a
socket substitute: guest TCP and Unix sockets remain real. It routes outbound
HTTP through the trusted sidecar because enforcement of HTTP policy, credentials,
and TLS identity belongs outside the untrusted VM. Unsupported per-client policy
or TLS overrides return explicit typed errors.

Codex's managed proxy listener, MITM, and credential-injection service are the
other host-native boundary. The fork retains its real policy/configuration types,
but attempting to launch that trusted service inside the untrusted guest fails
explicitly and directs ownership to the sidecar. This is not evidence that agentOS
lacks listeners, Unix sockets, or networking.

Every new patch must include a behavioral test where practical. Compile success
alone is not proof that the POSIX behavior survived.
