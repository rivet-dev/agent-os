// Shared driver for a full pi ACP turn in the browser converged executor
// (AGENTOS-WEB-ASYNC-AGENTS.md §10 M4b). The real full pi SDK runs UNMODIFIED as a guest;
// the host (this module, on the main thread) drives it as a proper external ACP client
// over STREAMING STDIN: write a request, read the reply off stdout, write the next —
// initialize → session/new → session/prompt → a model answer. pi-ai's global fetch
// reaches the model endpoint (ANTHROPIC_BASE_URL), which answers with an Anthropic SSE.
// Used by both the CI gate (pi-prompt) and the interactive demo (pi-demo).

import {
	allowAll,
	createBrowserDriver,
	createBrowserRuntimeDriverFactory,
} from "@rivet-dev/agentos-runtime-browser";
import { createAgentOsConvergedSidecar } from "../../src/converged-sidecar.js";

const WASM_MODULE_URL = "/wasm/agentos_sidecar_browser.js";
const WASM_BINARY_URL = "/wasm/agentos_sidecar_browser_bg.wasm";
const PI_BUNDLE_URL = "/pi-adapter.bundle.cjs";

export interface PiTurnResult {
	answer?: string;
	stdout: string;
	error?: string;
}

export interface PiTurnOptions {
	prompt: string;
	onStatus?: (status: string) => void;
	timeoutMs?: number;
}

interface AcpMessage {
	id?: number;
	method?: string;
	result?: { sessionId?: string };
	params?: { update?: { sessionUpdate?: string; content?: { text?: string } } };
}

export async function runPiTurn(opts: PiTurnOptions): Promise<PiTurnResult> {
	const status = opts.onStatus ?? (() => {});
	status("Loading pi…");
	const bundleResponse = await fetch(PI_BUNDLE_URL);
	if (!bundleResponse.ok) {
		return {
			stdout: "",
			error: `pi adapter bundle not built (${PI_BUNDLE_URL} returned ${bundleResponse.status})`,
		};
	}
	const bundleText = (await bundleResponse.text()).replace(/^#![^\n]*\n/, "");
	const baseUrl = window.location.origin;

	status("Booting the kernel + executor…");
	const system = await createBrowserDriver({ filesystem: "memory", permissions: allowAll, useDefaultNetwork: true });
	(system as { runtime?: unknown }).runtime = { process: {}, os: {} };
	const config = {
		rootFilesystem: { mode: "ephemeral", disableDefaultBaseLayer: false, lowers: [], bootstrapEntries: [] },
		permissions: { fs: "allow", network: "allow", childProcess: "allow", process: "allow", env: "allow", binding: "allow" },
	} as never;
	const factory = createBrowserRuntimeDriverFactory({
		workerUrl: new URL("/agentos-worker.js", window.location.href),
		convergedSidecar: createAgentOsConvergedSidecar(config, { moduleUrl: WASM_MODULE_URL, binaryUrl: WASM_BINARY_URL }),
	});

	const decoder = new TextDecoder();
	const stdio: Array<{ channel?: string; message?: unknown; data?: unknown }> = [];
	const driver = factory.createRuntimeDriver({
		system,
		runtime: (system as { runtime: { process: unknown; os: unknown } }).runtime,
		onStdio: (event: unknown) => stdio.push(event as never),
	} as never) as unknown as {
		exec: (code: string, opts: unknown) => Promise<{ errorMessage?: string }>;
		writeStdin: (executionId: string, data: string) => void;
		endStdin: (executionId: string) => void;
	};

	// ACP client state. We drive pi by reacting to its stdout responses.
	let executionId = "";
	let answer = "";
	let sentNew = false;
	let sentPrompt = false;
	let done = false;
	let processedLines = 0;
	const send = (obj: unknown) => driver.writeStdin(executionId, `${JSON.stringify(obj)}\n`);

	const handleLine = (line: string): void => {
		let msg: AcpMessage;
		try {
			msg = JSON.parse(line) as AcpMessage;
		} catch {
			return;
		}
		if (msg.id === 1 && msg.result && !sentNew) {
			sentNew = true;
			send({ jsonrpc: "2.0", id: 2, method: "session/new", params: { cwd: "/root", mcpServers: [] } });
		}
		if (msg.id === 2 && msg.result?.sessionId && !sentPrompt) {
			sentPrompt = true;
			send({ jsonrpc: "2.0", id: 3, method: "session/prompt", params: { sessionId: msg.result.sessionId, prompt: [{ type: "text", text: opts.prompt }] } });
		}
		if (msg.method === "session/update" && msg.params?.update?.sessionUpdate === "agent_message_chunk") {
			const text = msg.params.update.content?.text;
			if (text) answer += text;
		}
		if (msg.id === 3) done = true;
	};

	const collectStdout = (): string =>
		stdio.filter((e) => e.channel === "stdout").map((e) => { const p = e.message ?? e.data; return typeof p === "string" ? p : p instanceof Uint8Array ? decoder.decode(p) : ""; }).join("");
	const pumpLines = (): void => {
		const lines = collectStdout().split("\n");
		for (; processedLines < lines.length - 1; processedLines += 1) {
			const line = lines[processedLines].trim();
			if (line) handleLine(line);
		}
	};

	// The SDK reads a package.json next to its module path; seed it before pi evaluates.
	const setupPrelude =
		`var __fs=require('fs');function __mk(p){try{__fs.mkdirSync(p,{recursive:true});}catch(e){}}function __wr(p,c){try{__fs.writeFileSync(p,c);}catch(e){}}__mk('/root/pi');__mk('/root/.pi/agent');__wr('/root/pi/package.json',JSON.stringify({name:'pi-sdk-acp',version:'0.0.0'}));\n`;

	status("Running pi turn: initialize → session/new → session/prompt…");
	let execError: string | undefined;
	let execDone = false;
	let execResult: { errorMessage?: string } | undefined;
	// onStart fires (with the execution id) right before the exec message is posted, so
	// awaiting it guarantees: id is known, and our first write-stdin is queued AFTER the
	// exec message (pi sets up its stdin listener first, then receives initialize).
	let onStarted: () => void = () => {};
	const started = new Promise<void>((resolve) => { onStarted = resolve; });
	const execPromise = driver
		.exec(setupPrelude + bundleText, {
			filePath: "/root/pi/adapter.cjs",
			env: { HOME: "/root", ANTHROPIC_BASE_URL: baseUrl, ANTHROPIC_API_KEY: "sk-stub" },
			persistent: true,
			streamingStdin: true,
			onStart: (id: string) => { executionId = id; onStarted(); },
			onStdio: (event: unknown) => stdio.push(event as never),
		})
		.then((result) => {
			execDone = true;
			execResult = result;
			if (result?.errorMessage) execError = result.errorMessage;
			return result;
		})
		.catch((error: unknown) => {
			execDone = true;
			execError = error instanceof Error ? error.stack || error.message : String(error);
			return undefined;
		});

	await started;
	// Kick off the ACP handshake.
	send({ jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: 1, clientCapabilities: {} } });

	const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
	const deadline = Date.now() + (opts.timeoutMs ?? 40_000);
	while (!done && Date.now() < deadline && execError === undefined && !execDone) {
		pumpLines();
		await sleep(120);
	}
	pumpLines();
	// Let pi exit cleanly.
	try { driver.endStdin(executionId); } catch {}
	void execPromise;

	if (done) status("Done.");
	const out: PiTurnResult = { stdout: collectStdout(), answer: done ? answer : undefined };
	if (!done) out.error = `no answer. execDone=${execDone} execResult=${JSON.stringify(execResult ?? null)} execError=${execError ?? ""} stdout=${collectStdout().slice(0, 800)} stderr=${stdio.filter((e) => e.channel === "stderr").map((e) => { const p = e.message ?? e.data; return typeof p === "string" ? p : p instanceof Uint8Array ? decoder.decode(p) : ""; }).join("").slice(0, 800)}`;
	return out;
}
