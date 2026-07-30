import { Buffer as BufferPolyfill } from "buffer";
(globalThis as unknown as { Buffer?: unknown }).Buffer ??= BufferPolyfill;

import {
	allowAll,
	createBrowserDriver,
	createBrowserRuntimeDriverFactory,
	createWasiCommandBootstrapScript,
	type NodeRuntimeDriver,
	type PtyOpenResult,
} from "@rivet-dev/agentos-runtime-browser";
import { createAgentOsConvergedSidecar } from "../../src/converged-sidecar.js";

const WASM_MODULE_URL = "/wasm/agentos_sidecar_browser.js";
const WASM_BINARY_URL = "/wasm/agentos_sidecar_browser_bg.wasm";

const SHELL_ENV = {
	HOME: "/",
	PATH: "/bin:/usr/bin",
	TERM: "xterm-256color",
};

const GUEST = createWasiCommandBootstrapScript({
	commandSource: "/commands/sh",
	command: "sh",
	commands: {
		cat: "/commands/cat",
		echo: "/commands/echo",
		ls: "/commands/ls",
		wc: "/commands/wc",
	},
	env: SHELL_ENV,
	cwd: "/",
	preopens: { "/": "/" },
	bootMessage: "REAL_SHELL_BOOT",
	bytesMessagePrefix: "REAL_SHELL_BYTES:",
	startMessage: "REAL_SHELL_START",
	errorMessagePrefix: "REAL_SHELL_ERROR:",
});

declare global {
	interface Window {
		__browserRealShell?: {
			run(): Promise<{
				exitCode: number;
				masterFd?: number;
				slaveFd?: number;
				shellFetched: boolean;
				started: boolean;
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
	timeoutMs = 10_000,
): Promise<string> {
	let output = "";
	const deadline = Date.now() + timeoutMs;
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

async function readUntilOrExit(
	driver: NodeRuntimeDriver,
	executionId: string,
	fd: number,
	pattern: string,
	execPromise: Promise<unknown>,
	timeoutMs = 10_000,
): Promise<string> {
	let settled = false;
	let execSummary = "";
	execPromise.then(
		(value) => {
			settled = true;
			execSummary = JSON.stringify(value);
		},
		(error) => {
			settled = true;
			execSummary = error instanceof Error ? error.stack || error.message : String(error);
		},
	);
	let output = "";
	const deadline = Date.now() + timeoutMs;
	while (!output.includes(pattern) && Date.now() < deadline) {
		output += decode(await driver.readPty!(executionId, fd, { timeoutMs: 10 }));
		if (output.includes(pattern)) return output;
		if (settled) {
			throw new Error(
				`execution completed before ${pattern}; exec=${execSummary}; output=${JSON.stringify(output)}`,
			);
		}
		await new Promise((resolve) => setTimeout(resolve, 10));
	}
	if (!output.includes(pattern)) {
		throw new Error(`timed out waiting for ${pattern}; saw ${JSON.stringify(output)}`);
	}
	return output;
}

async function readUntilAllOrExit(
	driver: NodeRuntimeDriver,
	executionId: string,
	fd: number,
	patterns: string[],
	execPromise: Promise<unknown>,
	timeoutMs = 10_000,
): Promise<string> {
	let settled = false;
	let execSummary = "";
	execPromise.then(
		(value) => {
			settled = true;
			execSummary = JSON.stringify(value);
		},
		(error) => {
			settled = true;
			execSummary = error instanceof Error ? error.stack || error.message : String(error);
		},
	);
	let output = "";
	const deadline = Date.now() + timeoutMs;
	const hasPatternsInOrder = () => {
		let offset = 0;
		for (const pattern of patterns) {
			const index = output.indexOf(pattern, offset);
			if (index === -1) return false;
			offset = index + pattern.length;
		}
		return true;
	};
	while (!hasPatternsInOrder() && Date.now() < deadline) {
		output += decode(await driver.readPty!(executionId, fd, { timeoutMs: 10 }));
		if (hasPatternsInOrder()) return output;
		if (settled) {
			throw new Error(
				`execution completed before ${patterns.join(", ")}; exec=${execSummary}; output=${JSON.stringify(output)}`,
			);
		}
		await new Promise((resolve) => setTimeout(resolve, 10));
	}
	if (!hasPatternsInOrder()) {
		throw new Error(`timed out waiting for ${patterns.join(", ")}; saw ${JSON.stringify(output)}`);
	}
	return output;
}

async function waitForExecutionId(getExecutionId: () => string, timeoutMs = 5_000): Promise<string> {
	const deadline = Date.now() + timeoutMs;
	while (Date.now() < deadline) {
		const id = getExecutionId();
		if (id) return id;
		await new Promise((resolve) => setTimeout(resolve, 0));
	}
	throw new Error("timed out waiting for execution id");
}

async function run() {
	const shellProbe = await fetch("/commands/sh", { method: "HEAD" });
	const system = await createBrowserDriver({
		filesystem: "memory",
		permissions: allowAll,
		useDefaultNetwork: true,
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
		filePath: "/r3-browser-real-shell.js",
		persistent: true,
		timingMitigation: "off",
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

	let output = "";
	try {
		const pty = await ptyPromise;
		executionId = await waitForExecutionId(() => executionId);
		output = await readUntilOrExit(
			driver,
			executionId,
			pty.masterFd,
			"sh-0.4$ ",
			execPromise,
		);
		const echoCommand = "/bin/echo browser-brush-''ok";
		await driver.writePty!(executionId, pty.masterFd, `${echoCommand}\r`);
		const echoOutput = await readUntilAllOrExit(
			driver,
			executionId,
			pty.masterFd,
			[echoCommand, "browser-brush-ok", "sh-0.4$ "],
			execPromise,
		);
		if (!echoOutput.includes("browser-brush-ok")) {
			throw new Error(`echo output missing from shell transcript: ${JSON.stringify(echoOutput)}`);
		}
		output += echoOutput;
		const pipeCommand = "/bin/echo browser-pipe-ok | /bin/wc -c";
		await driver.writePty!(executionId, pty.masterFd, `${pipeCommand}\r`);
		const pipeOutput = await readUntilAllOrExit(
			driver,
			executionId,
			pty.masterFd,
			[pipeCommand, "16", "sh-0.4$ "],
			execPromise,
		);
		if (!pipeOutput.includes("16")) {
			throw new Error(`pipeline output missing from shell transcript: ${JSON.stringify(pipeOutput)}`);
		}
		output += pipeOutput;
		const catCommand = "/bin/echo browser-cat-ok-via-''cat | /bin/cat";
		await driver.writePty!(executionId, pty.masterFd, `${catCommand}\r`);
		const catOutput = await readUntilAllOrExit(
			driver,
			executionId,
			pty.masterFd,
			[catCommand, "browser-cat-ok-via-cat", "sh-0.4$ "],
			execPromise,
		);
		if (!catOutput.includes("browser-cat-ok-via-cat")) {
			throw new Error(`cat output missing from shell transcript: ${JSON.stringify(catOutput)}`);
		}
		output += catOutput;
		const redirectCommand = "/bin/echo browser-file-''ok > /tmp/browser-file.txt";
		await driver.writePty!(executionId, pty.masterFd, `${redirectCommand}\r`);
		const redirectOutput = await readUntilAllOrExit(
			driver,
			executionId,
			pty.masterFd,
			[redirectCommand, "sh-0.4$ "],
			execPromise,
		);
		output += redirectOutput;
		const redirectedCatCommand = "/bin/cat /tmp/browser-file.txt";
		await driver.writePty!(executionId, pty.masterFd, `${redirectedCatCommand}\r`);
		const redirectedCatOutput = await readUntilAllOrExit(
			driver,
			executionId,
			pty.masterFd,
			[redirectedCatCommand, "browser-file-ok", "sh-0.4$ "],
			execPromise,
		);
		if (!redirectedCatOutput.includes("browser-file-ok")) {
			throw new Error(`redirected file output missing from shell transcript: ${JSON.stringify(redirectedCatOutput)}`);
		}
		output += redirectedCatOutput;
		const lsCommand = "/bin/ls /";
		await driver.writePty!(executionId, pty.masterFd, `${lsCommand}\r`);
		const lsOutput = await readUntilAllOrExit(
			driver,
			executionId,
			pty.masterFd,
			[lsCommand, "etc", "sh-0.4$ "],
			execPromise,
		);
		if (!lsOutput.includes("etc")) {
			throw new Error(`ls output missing expected root entry: ${JSON.stringify(lsOutput)}`);
		}
		output += lsOutput;
		const ctrlCInput = "partial-browser-ctrl-c";
		await driver.writePty!(executionId, pty.masterFd, `${ctrlCInput}\u0003`);
		const ctrlCOutput = await readUntilAllOrExit(
			driver,
			executionId,
			pty.masterFd,
			[ctrlCInput, "^C", "sh-0.4$ "],
			execPromise,
		);
		if (!ctrlCOutput.includes("^C")) {
			throw new Error(`Ctrl-C marker missing from shell transcript: ${JSON.stringify(ctrlCOutput)}`);
		}
		output += ctrlCOutput;
		const afterCtrlCCommand = "/bin/echo browser-after-ctrl-''c";
		await driver.writePty!(executionId, pty.masterFd, `${afterCtrlCCommand}\r`);
		const afterCtrlCOutput = await readUntilAllOrExit(
			driver,
			executionId,
			pty.masterFd,
			[afterCtrlCCommand, "browser-after-ctrl-c", "sh-0.4$ "],
			execPromise,
		);
		if (!afterCtrlCOutput.includes("browser-after-ctrl-c")) {
			throw new Error(`shell did not accept command after Ctrl-C: ${JSON.stringify(afterCtrlCOutput)}`);
		}
		output += afterCtrlCOutput;
		await driver.closePty?.(executionId, pty.masterFd);
		driver.dispose?.();
		return {
			exitCode: 0,
			masterFd: pty.masterFd,
			slaveFd: pty.slaveFd,
			shellFetched: shellProbe.ok,
			started: true,
			output,
		};
	} catch (error) {
		driver.dispose?.();
		return {
			exitCode: -1,
			shellFetched: shellProbe.ok,
			started: false,
			output,
			error: error instanceof Error ? error.stack || error.message : String(error),
		};
	}
}

window.__browserRealShell = { run };

const status = document.getElementById("status");
if (status) status.textContent = "ready";
