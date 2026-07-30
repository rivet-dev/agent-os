import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const sourcePath = fileURLToPath(
	new URL("../src/test-runtime.ts", import.meta.url),
);

describe("sidecar build invalidation", () => {
	it("tracks every local crate that can change the sidecar binary", () => {
		const source = readFileSync(sourcePath, "utf8");
		for (const crate of [
			"acp-protocol",
			"driver-tokio",
			"executor-contract",
			"executor-node-v8",
			"executor-python-v8-pyodide",
			"executor-v8-runtime",
			"executor-wasm-abi",
			"executor-wasm-v8",
			"executor-wasm-wasmtime",
			"resource-accounting",
			"rivetkit-ars-client",
			"sidecar",
			"sidecar-protocol",
			"vfs-core",
			"vfs-storage",
			"vm",
			"vm-config",
			"vm-host-interface",
			"vm-kernel",
		]) {
			expect(source).toContain(`path.join(REPO_ROOT, "crates/${crate}")`);
		}
		for (const input of [
			"packages/build-tools/bridge-src",
			"packages/build-tools/package.json",
			"packages/build-tools/scripts/build-v8-bridge.mjs",
			"packages/core/fixtures/base-filesystem.json",
			"pnpm-lock.yaml",
		]) {
			expect(source).toContain(`path.join(REPO_ROOT, "${input}")`);
		}
	});
});
