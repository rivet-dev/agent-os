// M4 gate: the in-sandbox OpenAI proxy end-to-end in Chromium
// (AGENTOS-WEB-ASYNC-AGENTS.md §6). An async agent stands up both the "pi" HTTP client
// and the loopback proxy in one turn: the client POSTs an OpenAI chat-completions
// request over loopback; the proxy forwards the body to on-device inference via the
// `host.inference` host-callback (mock sentinel here) and returns an HTTP 200; the
// client reads the assistant message back. Proves HTTP-over-loopback + the inference
// host-callback compose — the exact path pi drives, minus pi running as its own guest.

import { Buffer as BufferPolyfill } from "buffer";
(globalThis as unknown as { Buffer?: unknown }).Buffer ??= BufferPolyfill;

import { KernelWorkerRelay, runSessionPromptGate } from "./async-harness.js";
import type { LanguageModelSession } from "../../src/chrome-llm-adapter.js";

const SENTINEL = "PONG_FROM_CHROME_LLM";
const mockModel: LanguageModelSession = { prompt: async () => SENTINEL };

(globalThis as unknown as { __asyncProxy: unknown }).__asyncProxy = {
	async run() {
		const relay = new KernelWorkerRelay("/async-kernel.worker.js", mockModel);
		return runSessionPromptGate(relay, {
			agentType: "async-proxy",
			adapterEntrypoint: "/bin/async-proxy-agent",
			promptText: "ping",
		});
	},
};

const status = document.getElementById("status");
if (status) status.textContent = "ready";
