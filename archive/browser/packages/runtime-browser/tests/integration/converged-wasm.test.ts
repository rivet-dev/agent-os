// Integration test: drives the converged TypeScript executor stack
// (ConvergedExecutorSession + ConvergedSyncBridgeHandler + the fs/net bridges)
// against the REAL wasm sidecar kernel (archive/browser/crates/sidecar-browser built with
// `pnpm build:sidecar-wasm`). This is the end-to-end proof that guest syscalls
// route through the kernel over the wire, replacing the legacy in-process TS
// kernel.
//
// Skips (rather than fails) when the wasm package has not been built, so the
// default unit run stays fast; CI builds it first.

import { createRequire } from "node:module";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";
import { describe, expect, it } from "vitest";
import type { CreateVmConfig } from "@rivet-dev/agentos-runtime-core/vm-config";
import { ConvergedExecutorSession } from "../../src/converged-executor-session.js";
import {
	SYNC_BRIDGE_KIND_JSON,
	SYNC_BRIDGE_KIND_NONE,
	SYNC_BRIDGE_KIND_TEXT,
} from "../../src/sync-bridge.js";

const here = path.dirname(fileURLToPath(import.meta.url));
const pkgEntry = path.resolve(
	here,
	"../../.cache/sidecar-wasm/agentos_vm_browser.js",
);
const pkgBuilt = existsSync(pkgEntry);

const PERMISSIVE_CONFIG = {
	permissions: { fs: "allow", network: "allow" },
} as unknown as CreateVmConfig;

function loadSidecar(): { pushFrame: (frame: Uint8Array) => Uint8Array } {
	const require = createRequire(import.meta.url);
	const pkg = require(pkgEntry);
	const sidecar = new pkg.BrowserSidecarWasm(undefined);
	return {
		pushFrame: (frame: Uint8Array) => {
			const response = sidecar.pushFrame(frame);
			if (!(response instanceof Uint8Array)) {
				throw new Error("wasm sidecar returned no response frame");
			}
			return response;
		},
	};
}

describe.skipIf(!pkgBuilt)("converged executor over the real wasm kernel", () => {
	function bootstrappedHandler() {
		const session = new ConvergedExecutorSession({
			pushFrame: loadSidecar().pushFrame,
			codec: "bare",
		});
		session.bootstrap({ runtime: "java_script", config: PERMISSIVE_CONFIG });
		return session.handlerForExecution("exec-1");
	}

	it("round-trips guest filesystem syscalls through the kernel", () => {
		const handler = bootstrappedHandler();

		expect(handler.handle("fs.mkdir", ["/work"])).toEqual({
			kind: SYNC_BRIDGE_KIND_NONE,
		});
		expect(
			handler.handle("fs.writeFile", ["/work/a.txt", "converged-wasm"]),
		).toEqual({ kind: SYNC_BRIDGE_KIND_NONE });

		expect(handler.handle("fs.readFile", ["/work/a.txt"])).toEqual({
			kind: SYNC_BRIDGE_KIND_TEXT,
			value: "converged-wasm",
		});

		const stat = handler.handle("fs.stat", ["/work/a.txt"]);
		expect(stat.kind).toBe(SYNC_BRIDGE_KIND_JSON);
		expect(stat.kind === SYNC_BRIDGE_KIND_JSON ? stat.value : null).toMatchObject(
			{ size: 14, isDirectory: false },
		);
	});

	it("expands readdir into typed entries via per-entry lstat", () => {
		const handler = bootstrappedHandler();
		handler.handle("fs.mkdir", ["/dir"]);
		handler.handle("fs.writeFile", ["/dir/file.txt", "x"]);
		handler.handle("fs.mkdir", ["/dir/sub"]);

		const listing = handler.handle("fs.readDir", ["/dir"]);
		expect(listing.kind).toBe(SYNC_BRIDGE_KIND_JSON);
		const entries = (listing.kind === SYNC_BRIDGE_KIND_JSON
			? (listing.value as Array<{ name: string; isDirectory: boolean }>)
			: []
		).sort((a, b) => a.name.localeCompare(b.name));
		expect(entries).toEqual([
			{ name: "file.txt", isDirectory: false, isSymbolicLink: false },
			{ name: "sub", isDirectory: true, isSymbolicLink: false },
		]);
	});

	it("carries a guest_kernel_call (net) ArrayBuffer payload through the BARE wire", () => {
		// Full net loopback needs a started execution (execution_id -> kernel pid);
		// here we prove the harder-to-cover bit: the BARE frame + opaque
		// ArrayBuffer `payload` of a guest_kernel_call round-trips through the real
		// wasm decoder and routes to the execution lookup (which rejects an unknown
		// execution) rather than failing to decode.
		const session = new ConvergedExecutorSession({
			pushFrame: loadSidecar().pushFrame,
			codec: "bare",
		});
		session.bootstrap({ runtime: "java_script", config: PERMISSIVE_CONFIG });
		const handler = session.handlerForExecution("no-such-execution");
		expect(() =>
			handler.handle("net.connect", [{ host: "127.0.0.1", port: 9 }]),
		).toThrow(/execution|guest_kernel/i);
	});

	it("enforces the kernel permission policy (deny-all config)", () => {
		const session = new ConvergedExecutorSession({
			pushFrame: loadSidecar().pushFrame,
			codec: "bare",
		});
		// Empty config => no policy => deny-all (S5).
		session.bootstrap({
			runtime: "java_script",
			config: {} as unknown as CreateVmConfig,
		});
		const handler = session.handlerForExecution("exec-1");
		expect(() => handler.handle("fs.writeFile", ["/blocked", "x"])).toThrow(
			/EACCES|denied|policy/,
		);
	});
});
