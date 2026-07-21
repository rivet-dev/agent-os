// Bundled live converged runtime harness.
//
// Creates a real BrowserRuntimeDriver with the converged sidecar option (so the
// guest's sync-bridge syscalls are serviced by the wasm kernel), runs a real
// guest that performs filesystem I/O, and reports its stdout/exit. This is the
// end-to-end proof of the live converged executor path (slice 2n) in Chromium.

import {
	allowAll,
	createBrowserDriver,
	createBrowserRuntimeDriverFactory,
} from "../../../../src/index.js";
import { rootFilesystemConfigFromVfs } from "../../../../src/root-filesystem-from-vfs.js";
import { createConvergedExecutionHostBridge } from "../../../../src/converged-execution-host-bridge.js";

const WASM_MODULE_URL = "/sidecar-wasm-web/agentos_native_sidecar_browser.js";
const WASM_BINARY_URL = "/sidecar-wasm-web/agentos_native_sidecar_browser_bg.wasm";

declare global {
	interface Window {
		__convergedRuntimeHarness?: {
			run(): Promise<{
				stdout: string;
				exitCode: number;
				error?: string;
				raw?: unknown;
			}>;
			runRequire(): Promise<{
				stdout: string;
				exitCode: number;
				error?: string;
			}>;
			runDgram(): Promise<{
				stdout: string;
				exitCode: number;
				error?: string;
			}>;
			runBroadFs(): Promise<{
				stdout: string;
				exitCode: number;
				error?: string;
			}>;
		};
	}
}

async function loadSidecar(): Promise<{
	pushFrame: (frame: Uint8Array) => Uint8Array;
	setNextExecutionId: (executionId: string) => void;
}> {
	const wasmModule = await import(/* @vite-ignore */ WASM_MODULE_URL);
	await wasmModule.default(WASM_BINARY_URL);
	const host = createConvergedExecutionHostBridge();
	const sidecar = new wasmModule.BrowserSidecarWasm(host.bridge);
	return {
		pushFrame: (frame: Uint8Array) => {
			const response = sidecar.pushFrame(frame);
			if (!(response instanceof Uint8Array)) {
				throw new Error("wasm sidecar returned no response frame");
			}
			return response;
		},
		setNextExecutionId: host.setNextExecutionId,
	};
}

const FS_GUEST_CODE = [
	"const fs = require('fs');",
	"fs.mkdirSync('/work', { recursive: true });",
	"fs.writeFileSync('/work/x.txt', 'converged-live');",
	"process.stdout.write(fs.readFileSync('/work/x.txt', 'utf8'));",
].join("\n");

// Writes a module to the kernel fs then require()s it back — exercising
// converged module resolution (kernel-backed resolver) from a running guest.
const REQUIRE_GUEST_CODE = [
	"const fs = require('fs');",
	"fs.mkdirSync('/app', { recursive: true });",
	"fs.writeFileSync('/app/dep.js', 'module.exports = 21 * 2;');",
	"const dep = require('/app/dep.js');",
	"process.stdout.write(String(dep));",
].join("\n");

// A real guest doing UDP loopback through dgram — its dgram.* syscalls hit the
// wasm kernel via the converged driver (which lazily registers the execution).
// Exercises a broad fs conformance surface (write/append/stat/readdir/rename/
// exists/read) through the converged kernel from a running guest.
const BROAD_FS_GUEST_CODE = [
	"const fs = require('fs');",
	"fs.mkdirSync('/d', { recursive: true });",
	"fs.writeFileSync('/d/a.txt', 'hello world');",
	"const size = fs.statSync('/d/a.txt').size;",
	"fs.renameSync('/d/a.txt', '/d/b.txt');",
	"const entries = fs.readdirSync('/d');",
	"const exists = fs.existsSync('/d/b.txt');",
	"const content = fs.readFileSync('/d/b.txt', 'utf8');",
	"process.stdout.write(JSON.stringify({ content, size, entries, exists }));",
].join("\n");

const DGRAM_GUEST_CODE = [
	"const dgram = require('dgram');",
	"const sock = dgram.createSocket('udp4');",
	"sock.on('message', (msg) => { process.stdout.write(new TextDecoder().decode(msg)); sock.close(); });",
	"sock.bind(46911, '127.0.0.1', () => {",
	"  sock.send(new TextEncoder().encode('live-dgram'), 46911, '127.0.0.1');",
	"});",
].join("\n");

async function execConvergedGuest(
	code: string,
): Promise<{ stdout: string; exitCode: number; error?: string }> {
	const system = await createBrowserDriver({
		filesystem: "memory",
		permissions: allowAll,
	});
	(system as { runtime?: unknown }).runtime = { process: {}, os: {} };

	const config = {
		rootFilesystem: await rootFilesystemConfigFromVfs(
			(system as { filesystem: Parameters<typeof rootFilesystemConfigFromVfs>[0] })
				.filesystem,
		),
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
		convergedSidecar: { loadSidecar, config },
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

window.__convergedRuntimeHarness = {
	run: () => execConvergedGuest(FS_GUEST_CODE),
	runRequire: () => execConvergedGuest(REQUIRE_GUEST_CODE),
	runDgram: () => execConvergedGuest(DGRAM_GUEST_CODE),
	runBroadFs: () => execConvergedGuest(BROAD_FS_GUEST_CODE),
};

const status = document.getElementById("harness-status");
if (status) {
	status.textContent = "ready";
}
