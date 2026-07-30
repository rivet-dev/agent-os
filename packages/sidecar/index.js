"use strict";

// Platform-specific resolver for the prebuilt `agentos-sidecar` binary. The
// binary itself ships inside one of the `@rivet-dev/agentos-sidecar-<platform>`
// packages, declared as optionalDependencies so npm only installs the one
// matching the current `os`/`cpu`/`libc` at install time.
//
// Resolution priority:
//   1. `AGENTOS_SIDECAR_BIN` env var (absolute path override).
//   2. A `agentos-sidecar` binary placed next to this package (dev builds).
//   3. A cargo build output under the repo `target/{release,debug}/` (dev).
//   4. The platform-specific `@rivet-dev/agentos-sidecar-<platform>` package.

const { existsSync } = require("node:fs");
const { join, dirname } = require("node:path");

const BINARY_NAME = "agentos-sidecar";

// No runtime chmod: the platform packages are published with `npm publish`,
// which preserves the binary's 0755 executable bit (pnpm publish would strip
// it to 0644). This mirrors how @rivetkit/engine-cli ships rivet-engine. See
// the "Native Binary Distribution" section in CLAUDE.md.

function getPlatformPackageName() {
	const { platform, arch } = process;
	switch (platform) {
		case "linux":
			if (arch === "x64") return "@rivet-dev/agentos-sidecar-linux-x64-gnu";
			if (arch === "arm64") return "@rivet-dev/agentos-sidecar-linux-arm64-gnu";
			break;
		case "darwin":
			if (arch === "x64") return "@rivet-dev/agentos-sidecar-darwin-x64";
			if (arch === "arm64") return "@rivet-dev/agentos-sidecar-darwin-arm64";
			break;
		default:
			break;
	}
	return null;
}

function getSidecarPath() {
	const override = process.env.AGENTOS_SIDECAR_BIN;
	if (override) {
		if (!existsSync(override)) {
			throw new Error(
				`AGENTOS_SIDECAR_BIN is set to ${override} but the file does not exist`,
			);
		}
		return override;
	}

	const localBinary = join(__dirname, BINARY_NAME);
	if (existsSync(localBinary)) {
		return localBinary;
	}

	for (const profile of ["release", "debug"]) {
		const candidate = join(__dirname, "..", "..", "target", profile, BINARY_NAME);
		if (existsSync(candidate)) {
			return candidate;
		}
	}

	const platformPkg = getPlatformPackageName();
	if (!platformPkg) {
		throw new Error(
			`@rivet-dev/agentos-sidecar: unsupported platform ${process.platform}/${process.arch}. ` +
				"The Agent OS sidecar currently supports linux and darwin on x64 and arm64. " +
				"Set AGENTOS_SIDECAR_BIN to a local agentos-sidecar binary to override.",
		);
	}

	let pkgJsonPath;
	try {
		pkgJsonPath = require.resolve(`${platformPkg}/package.json`);
	} catch {
		throw new Error(
			`@rivet-dev/agentos-sidecar: platform package ${platformPkg} is not installed.\n` +
				"This usually means the platform is unsupported or optionalDependencies were\n" +
				`skipped during install. Try: npm install --include=optional ${platformPkg}\n` +
				"Or set AGENTOS_SIDECAR_BIN to a local agentos-sidecar binary.",
		);
	}

	return join(dirname(pkgJsonPath), BINARY_NAME);
}

module.exports = { getSidecarPath };
