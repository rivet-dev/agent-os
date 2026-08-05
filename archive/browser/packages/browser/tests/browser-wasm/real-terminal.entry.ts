import { Buffer as BufferPolyfill } from "buffer";
(globalThis as unknown as { Buffer?: unknown }).Buffer ??= BufferPolyfill;

import { Terminal } from "@xterm/xterm";
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

// Common external commands the interactive shell should resolve on PATH. brush
// handles its own builtins (cd, pwd, echo, export, ...); these are the real wasm
// coreutils + tools staged under /commands. Additive: the R3 gate only invokes
// /bin/echo, so a wider set keeps it green while making manual use real.
const SHELL_COMMANDS = [
	"ls", "cat", "echo", "pwd", "printf", "env", "printenv", "head", "tail",
	"wc", "grep", "egrep", "fgrep", "sed", "awk", "find", "sort", "uniq", "cut",
	"tr", "rev", "tac", "mkdir", "rmdir", "rm", "mv", "cp", "ln", "touch", "stat",
	"readlink", "realpath", "basename", "dirname", "du", "df", "date", "seq",
	"sleep", "true", "false", "yes", "test", "expr", "tee", "xargs", "which",
	"whoami", "id", "uname", "hostname", "tree", "diff", "od", "base64", "md5sum",
	"sha256sum", "cksum", "rg", "fd", "jq", "yq", "tar", "gzip", "gunzip",
];

// proc_spawn resolves a command by basename (/usr/bin/ls -> the "ls" module), but
// brush still PATH-searches the VM filesystem for an executable file before it
// spawns. Create an executable stub at /usr/bin/<name> for each registered command
// so bare `ls`/`cat`/... resolve (absolute /bin/<cmd> already worked). Runs in the
// guest before the brush bootstrap IIFE; both share the kernel fs.
const PATH_STUB_SCRIPT = `
try {
	const __fs = require("fs");
	try { __fs.mkdirSync("/usr/bin", { recursive: true }); } catch (e) {}
	for (const __name of ${JSON.stringify(SHELL_COMMANDS)}) {
		try { __fs.writeFileSync("/usr/bin/" + __name, "", { mode: 0o755 }); } catch (e) {}
		try { __fs.chmodSync("/usr/bin/" + __name, 0o755); } catch (e) {}
	}
} catch (e) {}
`;

const GUEST =
	PATH_STUB_SCRIPT +
	createWasiCommandBootstrapScript({
		commandSource: "/commands/sh",
		command: "sh",
		commands: Object.fromEntries(
			SHELL_COMMANDS.map((name) => [name, `/commands/${name}`]),
		),
		env: SHELL_ENV,
		cwd: "/",
		preopens: { "/": "/" },
		bootMessage: "REAL_TERMINAL_BOOT",
		errorMessagePrefix: "REAL_TERMINAL_ERROR:",
	});

declare global {
	interface Window {
		__realTerminal?: {
			start(): Promise<{ masterFd: number; slaveFd: number }>;
			screen(): string;
			dispose(): Promise<void>;
		};
	}
}

const terminalElement = document.getElementById("terminal");
const statusElement = document.getElementById("status");
if (!terminalElement) {
	throw new Error("missing #terminal");
}

const terminal = new Terminal({
	cols: 100,
	rows: 31,
	convertEol: true,
	cursorBlink: true,
	fontFamily:
		"ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, Liberation Mono, monospace",
	fontSize: 13,
	theme: {
		background: "#101214",
		foreground: "#e8edf2",
		cursor: "#7dd3fc",
		selectionBackground: "#334155",
	},
});
terminal.open(terminalElement);

let driver: NodeRuntimeDriver | undefined;
let executionId = "";
let pty: PtyOpenResult | undefined;
let pumpRunning = false;
let started: Promise<{ masterFd: number; slaveFd: number }> | undefined;
const decoder = new TextDecoder();

function setStatus(value: string): void {
	if (statusElement) statusElement.textContent = value;
}

function delay(ms: number): Promise<void> {
	return new Promise((resolve) => setTimeout(resolve, ms));
}

async function waitForExecutionId(timeoutMs = 5_000): Promise<string> {
	const deadline = Date.now() + timeoutMs;
	while (Date.now() < deadline) {
		if (executionId) return executionId;
		await delay(0);
	}
	throw new Error("timed out waiting for execution id");
}

async function pumpPty(): Promise<void> {
	if (!driver || !pty) return;
	pumpRunning = true;
	while (pumpRunning && driver && pty && executionId) {
		const bytes = await driver.readPty!(executionId, pty.masterFd, {
			timeoutMs: 10,
			maxBytes: 4096,
		});
		if (bytes?.byteLength) {
			terminal.write(decoder.decode(bytes));
		} else {
			await delay(10);
		}
	}
}

function terminalScreen(): string {
	const buffer = terminal.buffer.active;
	const lines: string[] = [];
	for (let i = 0; i < buffer.length; i += 1) {
		lines.push(buffer.getLine(i)?.translateToString(true) ?? "");
	}
	return lines.join("\n");
}

async function start(): Promise<{ masterFd: number; slaveFd: number }> {
	if (started) return started;
	started = (async () => {
		setStatus("booting");
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
		driver = factory.createRuntimeDriver({
			system,
			runtime: (system as { runtime: { process: unknown; os: unknown } }).runtime,
		} as never);

		let resolvePty!: (opened: PtyOpenResult) => void;
		const ptyPromise = new Promise<PtyOpenResult>((resolve) => {
			resolvePty = resolve;
		});
		void driver.exec(GUEST, {
			filePath: "/r3-real-terminal-ui.js",
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
		pty = await ptyPromise;
		executionId = await waitForExecutionId();
		terminal.onData((data) => {
			void driver?.writePty?.(executionId, pty!.masterFd, data);
		});
		void pumpPty();
		terminal.focus();
		setStatus("running");
		return { masterFd: pty.masterFd, slaveFd: pty.slaveFd };
	})().catch((error) => {
		setStatus("error");
		terminal.write(
			`REAL_TERMINAL_UI_ERROR:${error instanceof Error ? error.stack || error.message : String(error)}\r\n`,
		);
		throw error;
	});
	return started;
}

async function dispose(): Promise<void> {
	pumpRunning = false;
	if (driver && pty && executionId) {
		await driver.closePty?.(executionId, pty.masterFd).catch(() => {});
	}
	driver?.dispose?.();
}

window.__realTerminal = {
	start,
	screen: terminalScreen,
	dispose,
};

setStatus("ready");
