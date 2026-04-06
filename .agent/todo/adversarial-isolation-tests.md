# Adversarial Isolation Tests

Moved from `scripts/ralph/prd.json` on 2026-04-05. These are security-focused adversarial tests that verify the isolation boundary works end-to-end, not just that the right flags are passed.

## US-001: Adversarial escape-attempt tests for Node.js filesystem isolation

**Priority:** High
**Why:** Currently `permission_flags.rs` uses `write_fake_node_binary()` which only checks args are passed, not that isolation works.

- Add test in `crates/execution/tests/javascript.rs` that runs guest JS attempting `fs.readFileSync('/etc/hostname')` and verifies it returns kernel VFS content, not host content
- Add test that attempts `fs.readFileSync` on a path outside the sandbox root and verifies EACCES or kernel-mediated denial
- Add test that attempts `require('fs').realpathSync('/')` and verifies it returns kernel VFS root, not host root
- Tests use real Node.js execution, not fake binaries or mocks
- `cargo test -p agent-os-execution --test javascript` passes

## US-002: Adversarial escape-attempt tests for child_process isolation

**Priority:** High
**Why:** US-008 tested exec/execSync hardening but only verified the RPC routing, not that actual host commands are blocked.

- Add test that attempts `require('child_process').execSync('whoami')` and verifies it routes through kernel process table, not host
- Add test that attempts `require('child_process').spawn('/bin/sh', ['-c', 'cat /etc/passwd'])` and verifies denial or kernel mediation
- Add test that verifies nested child processes cannot escalate Node `--permission` flags beyond what parent allows
- Tests use real Node.js execution end-to-end
- `cargo test -p agent-os-execution --test javascript` passes

## US-003: Adversarial escape-attempt tests for network isolation

**Priority:** High
**Why:** US-048 verified permission callbacks fire but didn't test actual blocked connections end-to-end.

- Add test that attempts `net.connect` to a non-exempt loopback port and verifies EACCES
- Add test that attempts `dns.lookup` of an external hostname and verifies it goes through sidecar DNS, not host resolver
- Add test that attempts `dgram.send` to a private IP and verifies SSRF blocking
- Tests use real Node.js execution with actual sidecar networking stack
- `cargo test -p agent-os-sidecar` passes for the new tests

## US-004: Adversarial escape-attempt tests for process.env and process identity leaks

**Priority:** High
**Why:** Execution-level tests exist but sidecar-level end-to-end verification is missing.

- Add sidecar-level test that verifies `process.env` contains no `AGENT_OS_*` keys via `Object.keys()` enumeration
- Add sidecar-level test that verifies `process.pid` returns kernel PID, not host PID
- Add sidecar-level test that verifies `process.cwd()` returns guest path, not host sandbox path
- Add sidecar-level test that verifies `process.execPath` does not contain host Node.js binary path
- Add sidecar-level test that verifies `require.resolve()` returns guest-visible paths
- Tests run through the full sidecar execution stack
- `cargo test -p agent-os-sidecar` passes for the new tests

## US-005: Fix SSRF private IP filter to cover all special-purpose ranges

**Priority:** Medium
**Why:** Current filter covers 10/172.16/192.168/169.254/fe80/fc00 but misses 0.0.0.0, broadcast, and multicast.

- Block 0.0.0.0/8 (current network) in `is_private_ip` check in `crates/sidecar/src/service.rs`
- Block 255.255.255.255/32 (broadcast)
- Block 224.0.0.0/4 (IPv4 multicast)
- Block ff00::/8 (IPv6 multicast)
- Add unit tests for each newly blocked range
- `cargo test -p agent-os-sidecar` passes

## US-006: Add network permission check for Unix socket connections

**Priority:** Medium
**Why:** TCP `net.connect` correctly calls `require_network_access` but Unix socket path skips it.

- Add `bridge.require_network_access()` call in the `net.connect({ path })` handler in `crates/sidecar/src/service.rs` before connecting
- Add test that creates a VM with denied network permissions and verifies Unix socket connect returns EACCES
- Existing Unix socket tests with allowed permissions continue to pass
- `cargo test -p agent-os-sidecar` passes

## US-007: Scrub host info from error messages returned to guest code

**Priority:** Medium
**Why:** Error responses currently leak actual IP/port info and DNS events include full resolver IPs.

- Audit all `respond_javascript_sync_rpc_error` calls in `crates/sidecar/src/service.rs` — ensure error messages do not contain host filesystem paths
- Scrub DNS event emissions so host resolver IPs are not included in guest-visible structured events
- Add test that triggers a filesystem error and verifies the guest-visible error message contains only guest paths
- Add test that triggers a network error and verifies the guest-visible error does not contain actual host IP/port
- `cargo test -p agent-os-sidecar` passes

## US-008: Make sidecar DNS resolver not fall through to host by default

**Priority:** Medium
**Why:** Current default uses `TokioResolver::builder_tokio()` which reads host `/etc/resolv.conf`.

- When no `network.dns.servers` metadata is configured, DNS queries should resolve only against a known-safe default (e.g., 8.8.8.8, 1.1.1.1) or return EACCES, never silently use the host system resolver
- Add test that creates a VM with no DNS override and verifies queries do not use the host `/etc/resolv.conf`
- Add test that creates a VM with explicit DNS servers and verifies only those servers are queried
- `cargo test -p agent-os-sidecar` passes

## US-009: Replace fake Node binary in permission flag tests with real enforcement tests

**Priority:** Medium
**Why:** All current tests use `write_fake_node_binary()` which logs invocations instead of executing.

- Add at least 3 tests in `crates/execution/tests/permission_flags.rs` that use a real Node.js binary
- One test verifies that `--allow-fs-read` scoping actually prevents reading a file outside the allowed path
- One test verifies that missing `--allow-child-process` actually prevents `child_process.spawn` from working
- One test verifies that missing `--allow-worker` actually prevents Worker creation
- `cargo test -p agent-os-execution --test permission_flags -- --test-threads=1` passes
