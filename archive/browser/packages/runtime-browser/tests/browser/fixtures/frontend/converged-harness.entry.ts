// Converged in-browser harness entry (esbuild-bundled).
//
// Loads the real web-target wasm sidecar kernel on the main thread and drives it
// through the converged TypeScript stack (ConvergedExecutorSession +
// ConvergedSyncBridgeHandler), proving the converged path runs in a real browser
// — not just Node. The Playwright spec calls `window.__convergedHarness`.

import { ConvergedExecutorSession } from "../../../../src/converged-executor-session.js";
import { ConvergedModuleServicer } from "../../../../src/converged-module-servicer.js";
import { KernelBackedFilesystem } from "../../../../src/kernel-backed-filesystem.js";
import { createConvergedExecutionHostBridge } from "../../../../src/converged-execution-host-bridge.js";
import { convergedPermissionsPolicy } from "../../../../src/converged-permissions.js";
import { decodeBase64 } from "../../../../src/converged-base64.js";

const WASM_BASE = "/sidecar-wasm-web";
const WASM_MODULE_URL = `${WASM_BASE}/agentos_vm_browser.js`;
const WASM_BINARY_URL = `${WASM_BASE}/agentos_vm_browser_bg.wasm`;

declare global {
	interface Window {
		__convergedHarness?: {
			runFilesystem(): Promise<{
				readText: string;
				stat: { size: number; isDirectory: boolean };
				dir: Array<{ name: string; isDirectory: boolean }>;
			}>;
			runModuleResolution(): Promise<{
				relative: string | null;
				barePackage: string | null;
			}>;
			runNetLoopback(): Promise<{ received: string }>;
			runUdpLoopback(): Promise<{ received: string; remotePort: number }>;
			runDgramSocket(): Promise<{ received: string }>;
			runFsReadDenied(): Promise<{ wrote: boolean; readDenied: boolean }>;
			runNetPortDenied(): Promise<{
				deniedPortListen: boolean;
				allowedPortListen: boolean;
			}>;
		};
	}
}

async function loadSidecar(hostBridge?: unknown): Promise<{
	pushFrame: (frame: Uint8Array) => Uint8Array;
}> {
	// Marked external in the esbuild bundle so it loads from the served URL at
	// runtime (wasm-bindgen web output fetches its own .wasm).
	const wasmModule = await import(/* @vite-ignore */ WASM_MODULE_URL);
	await wasmModule.default(WASM_BINARY_URL);
	const sidecar = new wasmModule.BrowserSidecarWasm(hostBridge);
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

async function bootstrapSession() {
	const { pushFrame } = await loadSidecar();
	const session = new ConvergedExecutorSession({ pushFrame, codec: "bare" });
	session.bootstrap({
		runtime: "java_script",
		config: { permissions: { fs: "allow", network: "allow" } } as never,
	});
	return session;
}

window.__convergedHarness = {
	async runFilesystem() {
		const handler = (await bootstrapSession()).handlerForExecution("exec-1");
		handler.handle("fs.mkdir", ["/work"]);
		handler.handle("fs.writeFile", ["/work/a.txt", "browser-converged"]);
		handler.handle("fs.mkdir", ["/work/sub"]);
		const read = handler.handle("fs.readFile", ["/work/a.txt"]);
		const stat = handler.handle("fs.stat", ["/work/a.txt"]);
		const dir = handler.handle("fs.readDir", ["/work"]);
		return {
			readText:
				read.kind === 1 /* TEXT */ ? String(read.value) : "<non-text>",
			stat:
				stat.kind === 3 /* JSON */
					? (stat.value as { size: number; isDirectory: boolean })
					: { size: -1, isDirectory: false },
			dir:
				dir.kind === 3
					? (dir.value as Array<{ name: string; isDirectory: boolean }>)
					: [],
		};
	},

	async runModuleResolution() {
		const session = await bootstrapSession();
		const handler = session.handlerForExecution("exec-1");
		// Seed a package layout in the kernel filesystem.
		handler.handle("fs.mkdir", ["/app"]);
		handler.handle("fs.writeFile", ["/app/index.js", "require('./util')"]);
		handler.handle("fs.writeFile", ["/app/util.js", "module.exports = 1"]);
		handler.handle("fs.mkdir", ["/app/node_modules"]);
		handler.handle("fs.mkdir", ["/app/node_modules/left-pad"]);
		handler.handle("fs.writeFile", [
			"/app/node_modules/left-pad/package.json",
			JSON.stringify({ name: "left-pad", main: "index.js" }),
		]);
		handler.handle("fs.writeFile", [
			"/app/node_modules/left-pad/index.js",
			"module.exports = () => {}",
		]);

		const moduleServicer = new ConvergedModuleServicer(
			new KernelBackedFilesystem(session.transportForVm()),
		);
		const relative = await moduleServicer.handle("module.resolve", [
			"./util",
			"/app/index.js",
			"require",
		]);
		const barePackage = await moduleServicer.handle("module.resolve", [
			"left-pad",
			"/app/index.js",
			"require",
		]);
		const text = (value: { kind: number; value?: unknown }) =>
			value.kind === 1 ? String(value.value) : null;
		return {
			relative: text(relative),
			barePackage: text(barePackage),
		};
	},

	async runNetLoopback() {
		const handler = await bootstrapNetHandler();
		const json = (value: { kind: number; value?: unknown }) =>
			value.kind === 3 ? (value.value as Record<string, unknown>) : {};
		const listener = json(
			handler.handle("net.listen", [{ host: "127.0.0.1", port: 39555 }]),
		);
		const client = json(
			handler.handle("net.connect", [{ host: "127.0.0.1", port: 39555 }]),
		);
		const accepted = json(
			handler.handle("net.accept", [{ socketId: listener.socketId }]),
		);
		handler.handle("net.write", [
			{ socketId: client.socketId, data: new TextEncoder().encode("ping-loopback") },
		]);
		const read = json(
			handler.handle("net.read", [{ socketId: accepted.socketId }]),
		);
		const received =
			typeof read.data === "string"
				? new TextDecoder().decode(decodeBase64(read.data))
				: "";
		return { received };
	},

	async runUdpLoopback() {
		const handler = await bootstrapNetHandler();
		const json = (value: { kind: number; value?: unknown }) =>
			value.kind === 3 ? (value.value as Record<string, unknown>) : {};
		const receiver = json(
			handler.handle("net.udp_bind", [{ host: "127.0.0.1", port: 45611 }]),
		);
		const sender = json(
			handler.handle("net.udp_bind", [{ host: "127.0.0.1", port: 45612 }]),
		);
		handler.handle("net.send_to", [
			{
				socketId: sender.socketId,
				host: "127.0.0.1",
				port: 45611,
				data: new TextEncoder().encode("udp-datagram"),
			},
		]);
		const recv = json(
			handler.handle("net.recv_from", [{ socketId: receiver.socketId }]),
		);
		const received =
			typeof recv.data === "string"
				? new TextDecoder().decode(decodeBase64(recv.data))
				: "";
		return { received, remotePort: Number(recv.remotePort ?? -1) };
	},

	async runDgramSocket() {
		// Exercises the converged dgram bridge with the EXACT worker dgram.* op
		// shapes (positional args) -> kernel UDP, in real Chromium.
		const handler = await bootstrapNetHandler();
		const json = (value: { kind: number; value?: unknown }) =>
			value.kind === 3 ? (value.value as Record<string, unknown>) : {};
		const receiver = json(handler.handle("dgram.create", [{ type: "udp4" }]));
		handler.handle("dgram.bind", [
			receiver.socketId,
			{ port: 46811, address: "127.0.0.1" },
		]);
		const sender = json(handler.handle("dgram.create", [{ type: "udp4" }]));
		handler.handle("dgram.send", [
			sender.socketId,
			new TextEncoder().encode("dgram-bridge"),
			{ port: 46811, address: "127.0.0.1" },
		]);
		const message = handler.handle("dgram.recv", [receiver.socketId, 0]);
		const value =
			message.kind === 3
				? (message.value as { type?: string; data?: unknown } | null)
				: null;
		const received =
			value && value.type === "message" && typeof value.data === "string"
				? new TextDecoder().decode(decodeBase64(value.data))
				: "";
		return { received };
	},

	async runFsReadDenied() {
		// The kernel enforces a declarative deny-fs-read policy: writes succeed,
		// reads are denied. Validates convergedPermissionsPolicy end to end.
		const { pushFrame } = await loadSidecar();
		const session = new ConvergedExecutorSession({ pushFrame, codec: "bare" });
		session.bootstrap({
			runtime: "java_script",
			config: {
				permissions: convergedPermissionsPolicy({ denyFsRead: true }),
			} as never,
		});
		const handler = session.handlerForExecution("exec-perms");
		let wrote = false;
		try {
			handler.handle("fs.writeFile", ["/denied.txt", "data"]);
			wrote = true;
		} catch {
			wrote = false;
		}
		let readDenied = false;
		try {
			handler.handle("fs.readFile", ["/denied.txt"]);
		} catch {
			readDenied = true;
		}
		return { wrote, readDenied };
	},

	async runNetPortDenied() {
		// The kernel enforces a declarative deny-network-port policy: listening on
		// the denied port fails, another port succeeds.
		const host = createConvergedExecutionHostBridge();
		host.setNextExecutionId("exec-perms-net");
		const { pushFrame } = await loadSidecar(host.bridge);
		const session = new ConvergedExecutorSession({ pushFrame, codec: "bare" });
		session.bootstrap({
			runtime: "java_script",
			config: {
				permissions: convergedPermissionsPolicy({ denyNetworkPort: 39999 }),
			} as never,
		});
		session.registerExecution({ processId: "exec-perms-net", args: ["node"] });
		const handler = session.handlerForExecution("exec-perms-net");

		let deniedPortListen = false;
		try {
			handler.handle("net.listen", [{ host: "127.0.0.1", port: 39999 }]);
		} catch {
			deniedPortListen = true;
		}
		let allowedPortListen = false;
		try {
			handler.handle("net.listen", [{ host: "127.0.0.1", port: 40001 }]);
			allowedPortListen = true;
		} catch {
			allowedPortListen = false;
		}
		return { deniedPortListen, allowedPortListen };
	},
};

async function bootstrapNetHandler() {
	// Construct the sidecar WITH an execution host bridge so an execute wire
	// request can register a kernel process for net/udp syscalls.
	const host = createConvergedExecutionHostBridge();
	host.setNextExecutionId("exec-net");
	const { pushFrame } = await loadSidecar(host.bridge);
	const session = new ConvergedExecutorSession({ pushFrame, codec: "bare" });
	session.bootstrap({
		runtime: "java_script",
		config: {
			permissions: {
				fs: "allow",
				network: "allow",
				childProcess: "allow",
				process: "allow",
				env: "allow",
				binding: "allow",
			},
		} as never,
	});
	session.registerExecution({ processId: "exec-net", args: ["node"] });
	return session.handlerForExecution("exec-net");
}

const status = document.getElementById("harness-status");
if (status) {
	status.textContent = "ready";
}
