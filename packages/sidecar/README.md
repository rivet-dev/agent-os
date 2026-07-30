# @rivet-dev/agentos-sidecar

Platform-specific resolver for the agentOS sidecar binary.

The compiled `agentos-sidecar` binary ships inside one of the
`@rivet-dev/agentos-sidecar-<platform>` packages, which this package declares as
optional dependencies. npm installs only the package matching the current
`os`/`cpu`/`libc` at install time.

```js
const { getSidecarPath } = require("@rivet-dev/agentos-sidecar");

const binaryPath = getSidecarPath();
```

Set `AGENTOS_SIDECAR_BIN` to an absolute path to override resolution (useful
for development or custom builds).

Supported platforms: `linux-x64-gnu`, `linux-arm64-gnu`, `darwin-arm64`, `darwin-x64`.
