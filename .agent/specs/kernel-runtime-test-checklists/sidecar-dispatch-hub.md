# Native Sidecar Dispatch Hub Test Checklist

Source files:
- `crates/sidecar/src/service.rs`

Suggested test homes:
- `crates/sidecar/tests/service.rs`
- `crates/sidecar/tests/security_audit.rs`
- `crates/sidecar/tests/session_isolation.rs`
- `crates/sidecar/tests/crash_isolation.rs`

## Checklist

### Request routing

- [ ] Add tests that every public request variant routes to exactly one handler branch and returns a stable unsupported-method error otherwise.
- [ ] Add tests that `Authenticate`, `OpenSession`, `CreateVm`, `CreateSession`, `SessionRequest`, `GetSessionState`, `CloseAgentSession`, `DisposeVm`, `BootstrapRootFilesystem`, `ConfigureVm`, `RegisterToolkit`, `CreateLayer`, `SealLayer`, `ImportSnapshot`, `ExportSnapshot`, `CreateOverlay`, `GuestFilesystemCall`, `SnapshotRootFilesystem`, `Execute`, `WriteStdin`, `CloseStdin`, `KillProcess`, `FindListener`, `FindBoundUdp`, `GetSignalState`, `GetZombieTimerCount`, `HostFilesystemCall`, `PermissionRequest`, and `PersistenceLoad` each have a dedicated handler path.
- [ ] Add tests that malformed request payloads are rejected before any state mutation occurs.
- [ ] Add tests that request ordering dependencies are enforced, such as requiring a VM before creating contexts or processes and requiring a session before VM-scoped operations.

### Ownership and policy enforcement

- [ ] Add tests that callers cannot act on VMs, contexts, sessions, sockets, or processes they do not own.
- [ ] Add tests that ownership transfer or nested ownership scopes update all descendant objects consistently.
- [ ] Add tests that permission policy evaluation happens before side effects for filesystem, runtime, networking, and ACP actions.
- [ ] Add tests that cross-connection access to session-owned or VM-owned resources is rejected with the same error shape as direct ownership violations.

### Auditing and orchestration

- [ ] Add tests that every security-relevant deny path emits the intended audit/log/event record exactly once.
- [ ] Add tests that ACP-specific service-layer orchestration remains consistent when a request races with process exit or connection close.
- [ ] Add tests that handler panics or internal errors are captured as protocol-safe failures and do not poison subsequent requests on the same connection.
- [ ] Add tests that request-idempotent operations such as close, kill, and delete stay idempotent under retries.
