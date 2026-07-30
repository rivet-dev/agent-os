// M4a gate: the REAL pi ACP adapter boots inside the browser converged executor
// (AGENTOS-WEB-ASYNC-AGENTS.md §10 M4a — "pi boots headless ... in the worker"). The
// same `@agentos-software/pi/dist/adapter.js` the native sidecar launches, esbuild-
// bundled (node:fs/path kept external so they route through the kernel; node:stream /
// node:module supplied by the executor's polyfills), runs as a guest in the converged
// node-stdlib executor with its stdio kernel-backed. We feed an ACP `initialize`
// request on stdin and assert pi answers with its agentInfo — proving the real pi
// binary boots and speaks ACP inside the kernel-sandboxed browser executor. (Reaching
// session/new additionally needs pi's node_modules mounted; the model is unwired here.)

import { Buffer as BufferPolyfill } from "buffer";
(globalThis as unknown as { Buffer?: unknown }).Buffer ??= BufferPolyfill;

import {
	allowAll,
	createBrowserDriver,
	createBrowserRuntimeDriverFactory,
} from "@rivet-dev/agentos-runtime-browser";
import { createAgentOsConvergedSidecar } from "../../src/converged-sidecar.js";

const WASM_MODULE_URL = "/wasm/agentos_sidecar_browser.js";
const WASM_BINARY_URL = "/wasm/agentos_sidecar_browser_bg.wasm";
const PI_BUNDLE_URL = "/pi-adapter.bundle.cjs";

declare global {
	interface Window {
		__piBoot?: {
			run(): Promise<{
				stdout: string;
				exitCode: number;
				acpId?: number;
				agentName?: string;
				error?: string;
			}>;
		};
	}
}

async function run(bundleUrl: string = PI_BUNDLE_URL): Promise<{ stdout: string; exitCode: number; acpId?: number; agentName?: string; error?: string }> {
	// Fetch the real pi adapter bundle (served as a static asset) and strip its shebang.
	const isCjs = bundleUrl.endsWith(".cjs");
	const filePath = isCjs ? "/root/pi/adapter.cjs" : "/root/pi/adapter.js";
	const bundleText = (await (await fetch(bundleUrl)).text()).replace(/^#![^\n]*\n/, "");
	// The full-SDK bundle reads a package.json next to its module path; seed the guest fs.
	const setupPrelude = isCjs
		? `var __fs=require('fs');function __mk(p){try{__fs.mkdirSync(p,{recursive:true});}catch(e){}}function __wr(p,c){try{__fs.writeFileSync(p,c);}catch(e){}}__mk('/root/pi');__mk('/root/.pi/agent');__wr('/root/pi/package.json',JSON.stringify({name:'pi-sdk-acp',version:'0.0.0'}));\n`
		: "";

	const system = await createBrowserDriver({ filesystem: "memory", permissions: allowAll });
	(system as { runtime?: unknown }).runtime = { process: {}, os: {} };
	const config = {
		rootFilesystem: { mode: "ephemeral", disableDefaultBaseLayer: false, lowers: [], bootstrapEntries: [] },
		permissions: { fs: "allow", network: "allow", childProcess: "allow", process: "allow", env: "allow", binding: "allow" },
	} as never;

	const factory = createBrowserRuntimeDriverFactory({
		workerUrl: new URL("/agentos-worker.js", window.location.href),
		convergedSidecar: createAgentOsConvergedSidecar(config, { moduleUrl: WASM_MODULE_URL, binaryUrl: WASM_BINARY_URL }),
	});

	const stdio: Array<{ channel?: string; message?: unknown; data?: unknown }> = [];
	const driver = factory.createRuntimeDriver({
		system,
		runtime: (system as { runtime: { process: unknown; os: unknown } }).runtime,
		onStdio: (event: unknown) => stdio.push(event as never),
	} as never);

	const initialize = `${JSON.stringify({ jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: 1, clientCapabilities: {} } })}\n`;
	const decoder = new TextDecoder();
	const collect = (channel: string) =>
		stdio
			.filter((e) => e.channel === channel)
			.map((e) => {
				const p = e.message ?? e.data;
				if (typeof p === "string") return p;
				if (p instanceof Uint8Array) return decoder.decode(p);
				return "";
			})
			.join("");
	const collectStdout = () => collect("stdout");

	const findReply = (): { acpId: number; agentName?: string } | null => {
		for (const line of collectStdout().split("\n")) {
			try {
				const msg = JSON.parse(line) as { id?: number; result?: { agentInfo?: { name?: string } } };
				if (msg.id === 1 && msg.result?.agentInfo) return { acpId: msg.id, agentName: msg.result.agentInfo.name };
			} catch {}
		}
		return null;
	};

	// Run pi as a PERSISTENT program: it gets the initialize request on stdin, processes
	// it asynchronously (WHATWG-stream pump → ndJsonStream → ACP), writes the reply, and
	// exits on stdin EOF. The persistent exec keeps the worker event loop alive for that
	// async I/O and resolves on pi's process.exit.
	let execError: string | undefined;
	const execResult = (await driver
		.exec(setupPrelude + bundleText, {
			filePath,
			env: { HOME: "/root", ANTHROPIC_BASE_URL: "http://127.0.0.1:1/stub", ANTHROPIC_API_KEY: "sk-stub" },
			stdin: initialize,
			persistent: true,
			onStdio: (event: unknown) => stdio.push(event as never),
		})
		.catch((error: unknown) => {
			execError = error instanceof Error ? error.stack || error.message : String(error);
			return undefined;
		})) as { errorMessage?: string } | undefined;
	if (!execError && execResult?.errorMessage) execError = `errorMessage=${execResult.errorMessage}`;

	const reply = findReply();
	const stderr = collect("stderr");
	const out: { stdout: string; exitCode: number; acpId?: number; agentName?: string; error?: string } = {
		stdout: collectStdout(),
		exitCode: reply ? 0 : -1,
	};
	if (reply) {
		out.acpId = reply.acpId;
		out.agentName = reply.agentName;
	} else {
		out.error = `no initialize reply. execError=${execError ?? ""} stderr=${stderr.slice(0, 1500)}`;
	}
	return out;
}

window.__piBoot = { run };

const status = document.getElementById("status");
if (status) status.textContent = "ready";
