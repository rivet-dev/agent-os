// Live converged runtime harness for Agent OS (esbuild-bundled).
//
// Builds a real BrowserRuntimeDriver from @rivet-dev/agentos-runtime-browser with the Agent OS
// converged sidecar (createAgentOsConvergedSidecar), runs a real guest that does
// filesystem I/O, and reports stdout/exit. Proves a real Agent OS guest runs in
// Chromium with its fs.* syscalls routed through the converged SharedArrayBuffer
// sync-bridge to the Agent OS wasm kernel (the same kernel that carries the ACP
// extension). The Playwright spec calls window.__agentosConvergedRuntime.

import {
	allowAll,
	createBrowserDriver,
	createBrowserRuntimeDriverFactory,
} from "@rivet-dev/agentos-runtime-browser";
import { createAgentOsConvergedSidecar } from "../../src/converged-sidecar.js";

// Served by serve.mjs at /wasm/ (the agentos wasm-bindgen web output). Passed to
// the converged sidecar loader so it resolves the glue + binary at runtime.
const WASM_MODULE_URL = "/wasm/agentos_sidecar_browser.js";
const WASM_BINARY_URL = "/wasm/agentos_sidecar_browser_bg.wasm";

declare global {
	interface Window {
		__agentosConvergedRuntime?: {
			runFs(): Promise<{ stdout: string; exitCode: number; error?: string }>;
			runRequire(): Promise<{
				stdout: string;
				exitCode: number;
				error?: string;
			}>;
		};
	}
}

const FS_GUEST_CODE = [
	"const fs = require('fs');",
	"fs.mkdirSync('/work', { recursive: true });",
	"fs.writeFileSync('/work/x.txt', 'agentos-converged');",
	"process.stdout.write(fs.readFileSync('/work/x.txt', 'utf8'));",
].join("\n");

const REQUIRE_GUEST_CODE = [
	"const fs = require('fs');",
	"fs.mkdirSync('/app', { recursive: true });",
	"fs.writeFileSync('/app/dep.js', 'module.exports = 21 * 2;');",
	"const dep = require('/app/dep.js');",
	"process.stdout.write(String(dep));",
].join("\n");

async function execConvergedGuest(
	code: string,
): Promise<{ stdout: string; exitCode: number; error?: string }> {
	const system = await createBrowserDriver({
		filesystem: "memory",
		permissions: allowAll,
	});
	(system as { runtime?: unknown }).runtime = { process: {}, os: {} };

	// New-caller shape: provide the kernel RootFilesystemConfig directly (the
	// guest creates its own dirs/files, so an empty ephemeral root is enough).
	const config = {
		rootFilesystem: {
			mode: "ephemeral",
			disableDefaultBaseLayer: false,
			lowers: [],
			bootstrapEntries: [],
		},
		permissions: {
			fs: "allow",
			network: "allow",
			childProcess: "allow",
			process: "allow",
			env: "allow",
			binding: "allow",
		},
	} as never;

	const factory = createBrowserRuntimeDriverFactory({
		workerUrl: new URL("/agentos-worker.js", window.location.href),
		convergedSidecar: createAgentOsConvergedSidecar(config, {
			moduleUrl: WASM_MODULE_URL,
			binaryUrl: WASM_BINARY_URL,
		}),
	});

	const stdio: Array<{ channel?: string; message?: unknown; data?: unknown }> =
		[];
	const driver = factory.createRuntimeDriver({
		system,
		runtime: (system as { runtime: { process: unknown; os: unknown } }).runtime,
		onStdio: (event: unknown) => stdio.push(event as never),
	} as never);

	try {
		const result = (await driver.exec(code, {
			onStdio: (event: unknown) => stdio.push(event as never),
		})) as { code?: number; exitCode?: number };
		const decoder = new TextDecoder();
		const stdout = stdio
			.filter((event) => event.channel === "stdout")
			.map((event) => {
				const payload = event.message ?? event.data;
				if (typeof payload === "string") return payload;
				if (payload instanceof Uint8Array) return decoder.decode(payload);
				return "";
			})
			.join("");
		return { stdout, exitCode: result.code ?? result.exitCode ?? -1 };
	} catch (error) {
		return {
			stdout: "",
			exitCode: -1,
			error:
				error instanceof Error ? error.stack || error.message : String(error),
		};
	}
}

window.__agentosConvergedRuntime = {
	runFs: () => execConvergedGuest(FS_GUEST_CODE),
	runRequire: () => execConvergedGuest(REQUIRE_GUEST_CODE),
};

const status = document.getElementById("status");
if (status) status.textContent = "ready";
