import * as fs from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import * as esbuild from "esbuild";
import { getIsolateRuntimeSource } from "@secure-exec/core";

// Resolve this package's root for bridge source and compiled bundle.
const __dirname = path.dirname(fileURLToPath(import.meta.url));
const packageRoot = path.resolve(__dirname, "..");

// Cache the bridge code
let bridgeCodeCache: string | null = null;

/** Locate the bridge TypeScript source for on-demand compilation (dev only). */
function findBridgeSourcePath(): string | null {
	const candidates = [
		path.join(packageRoot, "src", "bridge", "index.ts"),
	];
	for (const candidate of candidates) {
		if (fs.existsSync(candidate)) return candidate;
	}
	return null;
}

/** Walk a directory tree and return the newest file modification time. */
function getLatestMtimeMs(dir: string): number {
	let latest = 0;
	for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
		const entryPath = path.join(dir, entry.name);
		if (entry.isDirectory()) {
			latest = Math.max(latest, getLatestMtimeMs(entryPath));
		} else if (entry.isFile()) {
			latest = Math.max(latest, fs.statSync(entryPath).mtimeMs);
		}
	}
	return latest;
}

/**
 * Auto-compile the bridge IIFE bundle from TypeScript source if stale.
 * Skips rebuilding when the existing bundle is newer than all source files.
 */
function ensureBridgeBundle(bridgePath: string): void {
	const sourcePath = findBridgeSourcePath();

	// Fall back to an existing bridge bundle when source is unavailable.
	if (!sourcePath) {
		if (fs.existsSync(bridgePath)) return;
		throw new Error(
			"bridge.js not found and source is unavailable. Run `pnpm -C packages/nodejs build:bridge`.",
		);
	}

	const shouldBuild = (() => {
		if (!fs.existsSync(bridgePath)) return true;
		const sourceDir = path.dirname(sourcePath);
		const sourceMtime = getLatestMtimeMs(sourceDir);
		const bundleMtime = fs.statSync(bridgePath).mtimeMs;
		return sourceMtime > bundleMtime;
	})();

	if (!shouldBuild) return;

	fs.mkdirSync(path.dirname(bridgePath), { recursive: true });
	const result = esbuild.buildSync({
		entryPoints: [sourcePath],
		bundle: true,
		format: "iife",
		globalName: "bridge",
		outfile: bridgePath,
	});
	if (result.errors.length > 0) {
		throw new Error(`Failed to build bridge.js: ${result.errors[0].text}`);
	}
}

/**
 * Get the raw compiled bridge.js code.
 * This is the IIFE that creates the global `bridge` object.
 */
export function getRawBridgeCode(): string {
	if (!bridgeCodeCache) {
		const bridgePath = path.join(packageRoot, "dist", "bridge.js");
		ensureBridgeBundle(bridgePath);
		bridgeCodeCache = fs.readFileSync(bridgePath, "utf8");
	}
	return bridgeCodeCache;
}

/**
 * Get isolate script code that publishes the compiled bridge to `globalThis.bridge`.
 */
export function getBridgeAttachCode(): string {
	return getIsolateRuntimeSource("bridgeAttach");
}
