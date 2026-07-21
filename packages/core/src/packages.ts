import type { SoftwarePackageRef } from "./agentos-package.js";
import { existsSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { tryReadAgentosPackageManifest } from "./agentos-package.js";

// ── Software Descriptor Types ────────────────────────────────────────

/**
 * Input type for the `software` option. Software is a self-contained package
 * directory that the agentos sidecar materializes into the `/opt/agentos`
 * projection. Accepts a package-dir ref or an array of refs for meta-packages.
 */
export type SoftwareEntry = SoftwarePackageRef;
export type SoftwareInput = SoftwareEntry | SoftwareEntry[];

/** Host-to-VM path mapping for a software package's `/root/node_modules/<pkg>` mount. */
export interface SoftwareRoot {
	hostPath: string;
	vmPath: string;
}

// ── defineSoftware ───────────────────────────────────────────────────

/**
 * Define a software descriptor. A type-safe identity function that validates the
 * package-dir reference at compile time. The sidecar materializes the package into
 * the `/opt/agentos` projection.
 */
export function defineSoftware<T extends SoftwareEntry>(desc: T): T {
	return desc;
}

/**
 * Resolve the agent-SDK snapshot bundle (an esbuild IIFE at
 * `<dir>/dist/sdk-snapshot.js`) for the first snapshot-enabled agent package in
 * the software set. Returns its source so it can be evaluated once into the
 * per-sidecar V8 startup snapshot (`jsRuntime.snapshotUserlandCode`) and reused
 * across sessions. Returns `undefined` when no agent opts in (`agent.snapshot`)
 * or the bundle is absent -- the runtime then keeps the per-session import path.
 */
export function resolveAgentSnapshotBundle(
	software: SoftwareInput[],
): string | undefined {
	const descriptors = software.flat();
	for (const entry of descriptors) {
		// Only transition package DIRS are readable host-side (their
		// agentos-package.json is the toolchain-input manifest). Packed
		// `.aospkg` refs carry the bundle inside the mount tar; the sidecar
		// reads it from the packed vbare manifest's snapshotBundlePath.
		const path = entry.packagePath;
		if (path === undefined || !statIsDirectory(path)) continue;
		const manifest = tryReadAgentosPackageManifest(path);
		if (!manifest?.agent?.snapshot) continue;
		const bundlePath = join(path, "dist", "sdk-snapshot.js");
		if (existsSync(bundlePath)) {
			return readFileSync(bundlePath, "utf-8");
		}
	}
	return undefined;
}

function statIsDirectory(path: string): boolean {
	try {
		return statSync(path).isDirectory();
	} catch {
		return false;
	}
}
