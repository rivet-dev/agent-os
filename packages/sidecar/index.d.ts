/**
 * Resolve the absolute path to the prebuilt `agentos-sidecar` binary for the
 * current platform.
 *
 * Resolution priority:
 *   1. `AGENTOS_SIDECAR_BIN` env var (absolute path override).
 *   2. A `agentos-sidecar` binary placed next to this package (dev builds).
 *   3. A cargo build output under the repo `target/{release,debug}/` (dev).
 *   4. The platform-specific `@rivet-dev/agentos-sidecar-<platform>` package.
 *
 * @throws if the platform is unsupported or no binary can be found.
 */
export function getSidecarPath(): string;
