// R2 gate: a real guest process has stdio bound to a real kernel PTY slave.
// The host receives the PTY master fd, reads the guest's stdout from the master,
// writes terminal input to the master, and the guest receives it on process.stdin.

import { Buffer as BufferPolyfill } from "buffer";
(globalThis as unknown as { Buffer?: unknown }).Buffer ??= BufferPolyfill;

import {
	allowAll,
	createBrowserDriver,
	createBrowserRuntimeDriverFactory,
	type NodeRuntimeDriver,
	type PtyOpenResult,
} from "@rivet-dev/agentos-runtime-browser";
import { createAgentOsConvergedSidecar } from "../../src/converged-sidecar.js";

const WASM_MODULE_URL = "/wasm/agentos_sidecar_browser.js";
const WASM_BINARY_URL = "/wasm/agentos_sidecar_browser_bg.wasm";

const GUEST = `
	process.stdin.setRawMode(true);
	let resizeCount = 0;
	let stdoutResizeCount = 0;
	const stdoutResizeHandler = () => {
		stdoutResizeCount += 1;
		process.stdout.write("STDOUT_RESIZE:" + JSON.stringify({
			count: stdoutResizeCount,
			columns: process.stdout.columns,
			rows: process.stdout.rows,
		}) + "\\n");
	};
	process.stdout.on("resize", stdoutResizeHandler);
	process.kill(process.pid, "SIGWINCH");
	process.stdout.removeListener("resize", stdoutResizeHandler);
	process.on("SIGWINCH", () => {
		resizeCount += 1;
		process.stdout.write("RESIZE:" + JSON.stringify({
			count: resizeCount,
			columns: process.stdout.columns,
			rows: process.stdout.rows,
		}) + "\\n");
	});
	process.stdout.write("READY:" + JSON.stringify({
		stdinTTY: process.stdin.isTTY,
		stdoutTTY: process.stdout.isTTY,
		stderrTTY: process.stderr.isTTY,
		columns: process.stdout.columns,
		rows: process.stdout.rows,
	}) + "\\n");
	process.stdin.on("data", (chunk) => {
		const bytes = chunk instanceof Uint8Array ? chunk : new TextEncoder().encode(String(chunk));
		const text = new TextDecoder().decode(bytes);
		process.stdout.write("GOT:" + text);
		process.exit(0);
	});
	process.stdin.resume();
`;

declare global {
	interface Window {
		__ptyStdio?: {
			run(): Promise<{
				exitCode: number;
				masterFd?: number;
				slaveFd?: number;
				ready?: {
					stdinTTY?: boolean;
					stdoutTTY?: boolean;
					stderrTTY?: boolean;
					columns?: number;
					rows?: number;
				};
				resized?: {
					count?: number;
					columns?: number;
					rows?: number;
				};
				stdoutResized?: {
					count?: number;
					columns?: number;
					rows?: number;
				};
				output: string;
				error?: string;
			}>;
		};
	}
}

function decode(bytes: Uint8Array | null): string {
	return bytes ? new TextDecoder().decode(bytes) : "";
}

async function readUntil(
	driver: NodeRuntimeDriver,
	executionId: string,
	fd: number,
	pattern: string,
): Promise<string> {
	let output = "";
	const deadline = Date.now() + 10_000;
	while (!output.includes(pattern) && Date.now() < deadline) {
		output += decode(await driver.readPty!(executionId, fd, { timeoutMs: 10 }));
		if (!output.includes(pattern)) {
			await new Promise((resolve) => setTimeout(resolve, 10));
		}
	}
	if (!output.includes(pattern)) {
		throw new Error(`timed out waiting for ${pattern}; saw ${JSON.stringify(output)}`);
	}
	return output;
}

async function run() {
	const system = await createBrowserDriver({
		filesystem: "memory",
		permissions: allowAll,
	});
	(system as { runtime?: unknown }).runtime = { process: {}, os: {} };
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
	const driver = factory.createRuntimeDriver({
		system,
		runtime: (system as { runtime: { process: unknown; os: unknown } }).runtime,
	} as never);

	let executionId = "";
	let resolvePty!: (pty: PtyOpenResult) => void;
	const ptyPromise = new Promise<PtyOpenResult>((resolve) => {
		resolvePty = resolve;
	});
	const execPromise = driver.exec(GUEST, {
		filePath: "/r2-pty-stdio.js",
		persistent: true,
		onStart: (id) => {
			executionId = id;
		},
		stdioPty: {
			open: true,
			columns: 100,
			rows: 31,
			onOpen: resolvePty,
		},
	});

	try {
		const pty = await ptyPromise;
		const readyText = await readUntil(driver, executionId, pty.masterFd, "READY:");
		const readyLine =
			readyText
				.split("\n")
				.find((line) => line.startsWith("READY:"))
				?.slice("READY:".length) ?? "{}";
		const ready = JSON.parse(readyLine) as {
			stdinTTY?: boolean;
			stdoutTTY?: boolean;
			stderrTTY?: boolean;
			columns?: number;
			rows?: number;
		};
		await driver.resizePty!(executionId, pty.masterFd, { columns: 132, rows: 43 });
		const resizedText = await readUntil(driver, executionId, pty.masterFd, "RESIZE:");
		const resizedLine =
			resizedText
				.split("\n")
				.find((line) => line.startsWith("RESIZE:"))
				?.slice("RESIZE:".length) ?? "{}";
		const resized = JSON.parse(resizedLine) as {
			count?: number;
			columns?: number;
			rows?: number;
		};
		const stdoutResizedLine =
			readyText
				.split("\n")
				.find((line) => line.startsWith("STDOUT_RESIZE:"))
				?.slice("STDOUT_RESIZE:".length) ?? "{}";
		const stdoutResized = JSON.parse(stdoutResizedLine) as {
			count?: number;
			columns?: number;
			rows?: number;
		};
		await driver.writePty!(executionId, pty.masterFd, "terminal-input");
		const echoed = await readUntil(driver, executionId, pty.masterFd, "GOT:terminal-input");
		const result = await execPromise;
		await driver.closePty?.(executionId, pty.masterFd);
		return {
			exitCode: result.code ?? result.exitCode ?? -1,
			masterFd: pty.masterFd,
			slaveFd: pty.slaveFd,
			ready,
			resized,
			stdoutResized,
			output: readyText + resizedText + echoed,
		};
	} catch (error) {
		return {
			exitCode: -1,
			output: "",
			error: error instanceof Error ? error.stack || error.message : String(error),
		};
	}
}

window.__ptyStdio = { run };

const status = document.getElementById("status");
if (status) status.textContent = "ready";
