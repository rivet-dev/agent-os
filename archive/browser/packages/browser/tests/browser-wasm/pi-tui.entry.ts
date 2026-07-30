import { Buffer as BufferPolyfill } from "buffer";
(globalThis as unknown as { Buffer?: unknown }).Buffer ??= BufferPolyfill;

import { Terminal } from "@xterm/xterm";
import {
	allowAll,
	createBrowserDriver,
	createBrowserNetworkAdapter,
	createBrowserRuntimeDriverFactory,
	type NetworkAdapter,
	type NodeRuntimeDriver,
	type PtyOpenResult,
} from "@rivet-dev/agentos-runtime-browser";
import {
	chatRequestToPrompt,
	createChromeLanguageModelSession,
	createMockLanguageModelSession,
	getChromeLanguageModelAvailability,
	type LanguageModelSession,
} from "../../src/chrome-llm-adapter.js";
import { createAgentOsConvergedSidecar } from "../../src/converged-sidecar.js";

const WASM_MODULE_URL = "/wasm/agentos_sidecar_browser.js";
const WASM_BINARY_URL = "/wasm/agentos_sidecar_browser_bg.wasm";
const PI_CLI_BUNDLE_URL = "/pi-cli.bundle.cjs";
const PI_PACKAGE_JSON_URL = "/pi-package.json";
const PI_THEME_URLS = ["/pi-theme-dark.json", "/pi-theme-light.json"] as const;
const MODEL_ORIGIN = "https://agentos-real-language-model.localhost";
const MODEL_BASE_URL = `${MODEL_ORIGIN}/v1`;
const MODEL_PROVIDER = "chrome-language-model";
const MODEL_ID = "chrome-language-model";
const BOOT_WINDOW_MS = 8_000;
const PROMPT_WINDOW_MS = 9 * 60_000;
const MODEL_CREATE_TIMEOUT_MS = 8 * 60_000;

interface PiTuiResult {
	started: boolean;
	masterFd?: number;
	slaveFd?: number;
	screen: string;
	output: string;
	error?: string;
	visibleText?: string;
	rawOutputChars?: number;
	rawOutputPreview?: string;
	execStatus?: string;
	modelAvailability?: string;
	modelRequests?: number;
	modelResponses?: string[];
	modelErrors?: string[];
	modelDownloadProgress?: number[];
	networkRequests?: string[];
	usedRealLanguageModel?: boolean;
	promptAnswered?: boolean;
}

declare global {
	interface Window {
		__piTui?: {
			start(): Promise<PiTuiResult>;
			ask(prompt: string): Promise<PiTuiResult>;
			screen(): string;
			dispose(): Promise<void>;
		};
	}
}

const terminalElement = document.getElementById("terminal");
const statusElement = document.getElementById("status");
if (!terminalElement) throw new Error("missing #terminal");

const terminal = new Terminal({
	cols: 100,
	rows: 32,
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
let started: Promise<PiTuiResult> | undefined;
let output = "";
let execError = "";
let execStatus = "running";
let modelAvailability = "unprobed";
let modelRequests = 0;
const modelResponses: string[] = [];
const modelErrors: string[] = [];
const modelDownloadProgress: number[] = [];
const networkRequests: string[] = [];
let languageModelSession: Promise<LanguageModelSession | null> | undefined;
const decoder = new TextDecoder();

// MANUAL-TESTING ESCAPE HATCH: when the page is loaded with `?mockModel=1` (or
// window.__AGENTOS_MOCK_MODEL is set), pi's model calls are answered by a clearly
// labeled fake reply instead of Chrome's real on-device model. This is for driving
// the real terminal + pi TUI by hand on hosts where window.LanguageModel cannot be
// provisioned. It is OFF by default and never used by the strict gates; when on,
// usedRealLanguageModel is reported false.
const useMockModel =
	new URLSearchParams(globalThis.location?.search ?? "").get("mockModel") ===
		"1" ||
	(globalThis as unknown as { __AGENTOS_MOCK_MODEL?: unknown })
		.__AGENTOS_MOCK_MODEL === true;
let usingMockModel = false;

function setStatus(value: string): void {
	if (statusElement) statusElement.textContent = value;
}

function delay(ms: number): Promise<void> {
	return new Promise((resolve) => setTimeout(resolve, ms));
}

function terminalScreen(): string {
	const buffer = terminal.buffer.active;
	const lines: string[] = [];
	for (let i = 0; i < buffer.length; i += 1) {
		lines.push(buffer.getLine(i)?.translateToString(true) ?? "");
	}
	return lines.join("\n");
}

function visibleText(value: string): string {
	return value
		.replace(/\u001b\[[0-9;?]*[ -/]*[@-~]/g, "")
		.replace(/\u001b\][^\u0007]*(?:\u0007|\u001b\\)/g, "")
		.replace(/[\u0000-\u001f\u007f-\u009f]/g, "")
		.trim();
}

function rawPreview(value: string): string {
	return value
		.slice(0, 1_000)
		.replace(/\u001b/g, "\\x1b")
		.replace(/\r/g, "\\r")
		.replace(/\n/g, "\\n")
		.replace(/\t/g, "\\t");
}

function bodyToString(body: unknown): string {
	if (body == null) return "";
	if (typeof body === "string") return body;
	if (body instanceof Uint8Array) return decoder.decode(body);
	if (body instanceof ArrayBuffer) return decoder.decode(new Uint8Array(body));
	return String(body);
}

async function getLanguageModelSession(): Promise<LanguageModelSession | null> {
	if (useMockModel) {
		usingMockModel = true;
		modelAvailability = "mock";
		return createMockLanguageModelSession();
	}
	if (!languageModelSession) {
		modelAvailability = await getChromeLanguageModelAvailability();
		const controller = new AbortController();
		const timeout = setTimeout(() => {
			controller.abort(
				new Error(
					`LanguageModel.create() timed out after ${MODEL_CREATE_TIMEOUT_MS}ms`,
				),
			);
		}, MODEL_CREATE_TIMEOUT_MS);
		languageModelSession = createChromeLanguageModelSession({
			allowDownload: true,
			signal: controller.signal,
			onDownloadProgress(progress) {
				modelDownloadProgress.push(progress);
			},
		})
			.catch((error) => {
				const message =
					error instanceof Error ? error.message : String(error);
				modelErrors.push(`Chrome LanguageModel session creation failed: ${message}`);
				return null;
			})
			.finally(() => clearTimeout(timeout));
	}
	const session = await languageModelSession;
	if (session) modelAvailability = "available";
	return session;
}

function isModelUrl(url: string): boolean {
	if (url.includes(new URL(MODEL_ORIGIN).hostname)) return true;
	try {
		const parsed = new URL(url);
		return parsed.origin === MODEL_ORIGIN && parsed.pathname.endsWith("/chat/completions");
	} catch {
		return false;
	}
}

function openAiSse(model: string, content: string): string {
	const id = "chatcmpl-chrome-language-model";
	const created = Math.floor(Date.now() / 1000);
	const chunk = (delta: Record<string, unknown>, finish_reason: string | null) =>
		`data: ${JSON.stringify({
			id,
			object: "chat.completion.chunk",
			created,
			model,
			choices: [{ index: 0, delta, finish_reason }],
		})}\n\n`;
	return [
		chunk({ role: "assistant" }, null),
		chunk({ content }, null),
		chunk({}, "stop"),
		"data: [DONE]\n\n",
	].join("");
}

function modelErrorResponse(url: string, message: string) {
	return {
		ok: false,
		status: 503,
		statusText: "Service Unavailable",
		headers: { "content-type": "application/json" },
		body: JSON.stringify({ error: { message } }),
		url,
		redirected: false,
	};
}

async function handleModelFetch(url: string, body: unknown) {
	modelRequests += 1;
	const session = await getLanguageModelSession();
	if (!session) {
		const error =
			`Chrome LanguageModel is not available (${modelAvailability}); no mock model was used` +
			(modelDownloadProgress.length
				? `; downloadProgress=${JSON.stringify(modelDownloadProgress)}`
				: "");
		modelErrors.push(error);
		return modelErrorResponse(url, error);
	}
	try {
		const requestBody = bodyToString(body);
		const request = JSON.parse(requestBody) as Parameters<typeof chatRequestToPrompt>[0];
		const text = await session.prompt(chatRequestToPrompt(request));
		modelResponses.push(text);
		return {
			ok: true,
			status: 200,
			statusText: "OK",
			headers: { "content-type": "text/event-stream" },
			body: openAiSse(String(request.model ?? MODEL_ID), text),
			url,
			redirected: false,
		};
	} catch (error) {
		const message = error instanceof Error ? error.message : String(error);
		modelErrors.push(message);
		return modelErrorResponse(url, message);
	}
}

function createRealModelNetworkAdapter(): NetworkAdapter {
	const defaultNetwork = createBrowserNetworkAdapter();
	return {
		async fetch(url, options) {
			networkRequests.push(`fetch ${url}`);
			if (isModelUrl(url)) return handleModelFetch(url, options?.body);
			return defaultNetwork.fetch(url, options);
		},
		async dnsLookup(hostname) {
			if (hostname === new URL(MODEL_ORIGIN).hostname) {
				return { address: "127.0.0.1", family: 4 };
			}
			return defaultNetwork.dnsLookup(hostname);
		},
		async httpRequest(url, options) {
			networkRequests.push(`httpRequest ${url}`);
			if (isModelUrl(url)) {
				const response = await handleModelFetch(url, options?.body);
				return {
					status: response.status,
					statusText: response.statusText,
					headers: response.headers,
					body: response.body,
					url: response.url,
				};
			}
			return defaultNetwork.httpRequest(url, options);
		},
	};
}

async function waitForExecutionId(timeoutMs = 5_000): Promise<string> {
	const deadline = Date.now() + timeoutMs;
	while (Date.now() < deadline) {
		if (executionId) return executionId;
		await delay(0);
	}
	throw new Error("timed out waiting for execution id");
}

async function waitFor(
	predicate: () => boolean,
	timeoutMs: number,
	intervalMs = 50,
): Promise<boolean> {
	const deadline = Date.now() + timeoutMs;
	while (Date.now() < deadline) {
		if (predicate()) return true;
		await delay(intervalMs);
	}
	return predicate();
}

async function pumpPty(): Promise<void> {
	if (!driver || !pty) return;
	pumpRunning = true;
	while (pumpRunning && driver && pty && executionId) {
		const bytes = await driver.readPty!(executionId, pty.masterFd, {
			timeoutMs: 10,
			maxBytes: 8192,
		});
		if (bytes?.byteLength) {
			const text = decoder.decode(bytes);
			output += text;
			terminal.write(text);
		} else {
			await delay(10);
		}
	}
}

function collectResult(error?: string, promptAnswered = false): PiTuiResult {
	const screen = terminalScreen();
	const visible = visibleText(`${screen}\n${output}`);
	return {
		started: Boolean(pty && executionId),
		masterFd: pty?.masterFd,
		slaveFd: pty?.slaveFd,
		screen,
		output,
		visibleText: visible,
		rawOutputChars: output.length,
		rawOutputPreview: rawPreview(output),
		execStatus,
		modelAvailability,
		modelRequests,
		modelResponses: [...modelResponses],
		modelErrors: [...modelErrors],
		modelDownloadProgress: [...modelDownloadProgress],
		networkRequests: [...networkRequests],
		usedRealLanguageModel: modelResponses.length > 0 && !usingMockModel,
		promptAnswered,
		error,
	};
}

async function start(): Promise<PiTuiResult> {
	if (started) return started;
	started = (async () => {
		setStatus("booting");
		const bundleResponse = await fetch(PI_CLI_BUNDLE_URL);
		if (!bundleResponse.ok) {
			return {
				started: false,
				screen: terminalScreen(),
				output,
				error: `pi CLI bundle not built (${bundleResponse.status})`,
			};
		}
		const rawCliBundle = (await bundleResponse.text()).replace(/^#![^\n]*\n/, "");
		const packageJsonResponse = await fetch(PI_PACKAGE_JSON_URL);
		if (!packageJsonResponse.ok) {
			return {
				started: false,
				screen: terminalScreen(),
				output,
				error: `pi package metadata not staged (${packageJsonResponse.status})`,
			};
		}
		const packageJson = await packageJsonResponse.text();
		const themeEntries = await Promise.all(
			PI_THEME_URLS.map(async (url) => {
				const response = await fetch(url);
				if (!response.ok) {
					throw new Error(`pi theme asset not staged: ${url} (${response.status})`);
				}
				return [url.replace(/^\/pi-theme-/, ""), await response.text()] as const;
			}),
		);
		const cliBundle = [
			"var __piFs = require('fs');",
			"var __agentosFetch = typeof globalThis.fetch === 'function' ? globalThis.fetch.bind(globalThis) : undefined;",
			"if (__agentosFetch) Object.defineProperty(globalThis, 'fetch', { configurable: true, get: function() { return __agentosFetch; }, set: function() {} });",
			"__piFs.mkdirSync('/root', { recursive: true });",
			"__piFs.mkdirSync('/root/.pi/agent', { recursive: true });",
			"__piFs.mkdirSync('/root/dist/modes/interactive/theme', { recursive: true });",
			`__piFs.writeFileSync('/root/package.json', ${JSON.stringify(packageJson)});`,
			`__piFs.writeFileSync('/root/.pi/agent/models.json', ${JSON.stringify(
				JSON.stringify({
					providers: {
						[MODEL_PROVIDER]: {
							baseUrl: MODEL_BASE_URL,
							apiKey: "sk-chrome-language-model",
							api: "openai-completions",
							models: [
								{
									id: MODEL_ID,
									name: "Chrome LanguageModel",
									api: "openai-completions",
									reasoning: false,
									input: ["text"],
									contextWindow: 8192,
									maxTokens: 1024,
									compat: {
										supportsStore: false,
										supportsDeveloperRole: false,
										supportsReasoningEffort: false,
										supportsUsageInStreaming: false,
										maxTokensField: "max_tokens",
										supportsStrictMode: false,
									},
								},
							],
						},
					},
				}),
			)});`,
			`__piFs.writeFileSync('/root/.pi/agent/settings.json', ${JSON.stringify(
				JSON.stringify({
					defaultProvider: MODEL_PROVIDER,
					defaultModel: MODEL_ID,
					enabledModels: [`${MODEL_PROVIDER}/${MODEL_ID}`],
					quietStartup: true,
				}),
			)});`,
			...themeEntries.map(
				([name, content]) =>
					`__piFs.writeFileSync('/root/dist/modes/interactive/theme/${name}', ${JSON.stringify(content)});`,
			),
			rawCliBundle,
		].join("\n");

		const system = await createBrowserDriver({
			filesystem: "memory",
			permissions: allowAll,
			networkAdapter: createRealModelNetworkAdapter(),
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
		void driver
			.exec(cliBundle, {
				filePath: "/root/pi-cli.cjs",
				persistent: true,
				timingMitigation: "off",
				env: {
					HOME: "/root",
					TERM: "xterm-256color",
					COLORTERM: "truecolor",
					PI_CODING_AGENT_DIR: "/root/.pi/agent",
				},
				onStart: (id) => {
					executionId = id;
				},
				stdioPty: {
					open: true,
					columns: 100,
					rows: 32,
					onOpen: resolvePty,
				},
			})
			.then((result) => {
				execStatus = `resolved:${JSON.stringify(result)}`;
			})
			.catch((error) => {
				execError = error instanceof Error ? error.stack || error.message : String(error);
				execStatus = `rejected:${execError}`;
			});
		pty = await ptyPromise;
		executionId = await waitForExecutionId();
		terminal.onData((data) => {
			void driver?.writePty?.(executionId, pty!.masterFd, data);
		});
		void pumpPty();
		terminal.focus();
		await delay(BOOT_WINDOW_MS);
		const screen = terminalScreen();
		const visible = visibleText(`${screen}\n${output}`);
		const diagnosticSuffix = ` (raw chars: ${output.length}, exec: ${execStatus}, raw preview: ${rawPreview(output)})`;
		setStatus("running");
		return collectResult(
			execError ||
				(visible.length === 0
					? `real pi CLI started on the PTY, but produced no visible TUI text before the ${BOOT_WINDOW_MS}ms boot window elapsed${diagnosticSuffix}`
					: undefined),
		);
	})().catch((error) => {
		setStatus("error");
		const message = error instanceof Error ? error.stack || error.message : String(error);
		terminal.write(`PI_TUI_BOOT_ERROR:${message}\r\n`);
		return collectResult(message);
	});
	return started;
}

async function ask(prompt: string): Promise<PiTuiResult> {
	const boot = await start();
	if (boot.error) return boot;
	if (!driver || !pty || !executionId) {
		return collectResult("pi TUI is not running");
	}
	const requestCount = modelRequests;
	const responseCount = modelResponses.length;
	const errorCount = modelErrors.length;
	await driver.writePty?.(executionId, pty.masterFd, `${prompt}\r`);
	const reachedModel = await waitFor(() => modelRequests > requestCount || Boolean(execError), PROMPT_WINDOW_MS);
	if (!reachedModel) {
		return collectResult(
			`typed prompt did not reach pi's model provider within ${PROMPT_WINDOW_MS}ms`,
		);
	}
	const answered = await waitFor(
		() => modelResponses.length > responseCount || modelErrors.length > errorCount || Boolean(execError),
		PROMPT_WINDOW_MS,
	);
	if (!answered) {
		return collectResult(`pi's model provider did not complete within ${PROMPT_WINDOW_MS}ms`);
	}
	const promptAnswered = modelResponses.length > responseCount;
	return collectResult(
		execError || (promptAnswered ? undefined : modelErrors.at(-1) ?? "pi model request failed"),
		promptAnswered,
	);
}

async function dispose(): Promise<void> {
	pumpRunning = false;
	if (driver && pty && executionId) {
		await driver.closePty?.(executionId, pty.masterFd).catch(() => {});
	}
	driver?.dispose?.();
}

window.__piTui = { start, ask, screen: terminalScreen, dispose };
setStatus("ready");
// NOTE: this shared entry does NOT auto-boot — the Playwright gates drive
// start()/ask() themselves. The Vite dev page boots via pi-tui-dev.ts so manual
// testing works for both the real model and ?mockModel=1.
