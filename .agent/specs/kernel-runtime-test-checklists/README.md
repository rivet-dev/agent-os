# Kernel Runtime Test Checklists

This folder holds subsystem-by-subsystem test checklists derived from
`docs-internal/kernel-runtime-subsystem-map.md`.

Each document answers one question: what tests still need to be written to
give that subsystem strong coverage?

`Suggested test homes` may point either to an existing test suite or to a
sensible new file to add when the current layout does not already have a tight
fit.

## Tree

- Kernel / bridge
  - [shared-bridge-contract.md](./shared-bridge-contract.md)
  - [kernel-vm-and-syscall-surface.md](./kernel-vm-and-syscall-surface.md)
  - [vfs-and-filesystem-substrate.md](./vfs-and-filesystem-substrate.md)
  - [pseudo-filesystems-dev-and-proc.md](./pseudo-filesystems-dev-and-proc.md)
  - [process-command-fd-pipe-pty-readiness.md](./process-command-fd-pipe-pty-readiness.md)
  - [permissions-resource-limits-and-user-identity.md](./permissions-resource-limits-and-user-identity.md)
- Execution
  - [execution-runtime-common-layer.md](./execution-runtime-common-layer.md)
  - [javascript-runtime-host-path.md](./javascript-runtime-host-path.md)
  - [loader-materialization-and-builtin-interception.md](./loader-materialization-and-builtin-interception.md)
  - [guest-bridge-bundles-and-fetch-shims.md](./guest-bridge-bundles-and-fetch-shims.md)
  - [python-pyodide-runtime.md](./python-pyodide-runtime.md)
  - [wasm-runtime.md](./wasm-runtime.md)
  - [execution-v8-client-transport-and-ipc.md](./execution-v8-client-transport-and-ipc.md)
  - [v8-isolate-runtime-daemon.md](./v8-isolate-runtime-daemon.md)
- Native sidecar
  - [sidecar-transport-protocol-and-state-machine.md](./sidecar-transport-protocol-and-state-machine.md)
  - [sidecar-dispatch-hub.md](./sidecar-dispatch-hub.md)
  - [sidecar-vm-lifecycle-and-layering.md](./sidecar-vm-lifecycle-and-layering.md)
  - [sidecar-guest-filesystem-api.md](./sidecar-guest-filesystem-api.md)
  - [sidecar-shadow-root-reconciliation.md](./sidecar-shadow-root-reconciliation.md)
  - [sidecar-tool-virtualization.md](./sidecar-tool-virtualization.md)
  - [sidecar-process-runtime-dispatch.md](./sidecar-process-runtime-dispatch.md)
  - [sidecar-networking-policy-and-socket-transports.md](./sidecar-networking-policy-and-socket-transports.md)
  - [sidecar-tls-http-http2-planes.md](./sidecar-tls-http-http2-planes.md)
  - [sidecar-builtin-service-rpcs.md](./sidecar-builtin-service-rpcs.md)
  - [mount-plugin-bridge-and-permission-glue.md](./mount-plugin-bridge-and-permission-glue.md)
  - [acp-agent-session-layer.md](./acp-agent-session-layer.md)
  - [browser-side-sidecar-variant.md](./browser-side-sidecar-variant.md)
- Mount plugins
  - [first-party-mount-plugins.md](./first-party-mount-plugins.md)
