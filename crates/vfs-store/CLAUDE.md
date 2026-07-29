# agentos-vfs

- `agentos-vfs` contains concrete backend adapters for agentos deployments: S3, host-disk metadata/block stores, and bridge/callback-backed stores.
- Keep policy decisions, trusted configuration validation, mount descriptor parsing, and sidecar lifecycle wiring in the sidecar plugin layer.
- Generic filesystem algorithms and in-memory stores belong in `vfs`.
