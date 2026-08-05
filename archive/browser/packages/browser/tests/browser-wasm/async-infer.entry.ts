// M4 gate: the async-inference transport end-to-end in Chromium
// (AGENTOS-WEB-ASYNC-AGENTS.md §6). An ASYNC agent answers a session/prompt by making
// a mid-turn `host.inference` syscall; the kernel DEFERS it to the main thread (the
// chrome-llm host-callback), the guest parks in its SAB shim, the main thread runs the
// on-device model (a deterministic mock here, the real `LanguageModel` in the Nano
// smoke), writes the OpenAI-shaped reply to the completion channel, and the reactor
// unblocks the guest. Proves: guest → deferred syscall → host-callback → completion
// channel → guest, the one async hop pi reaches through the in-sandbox proxy.

import { Buffer as BufferPolyfill } from "buffer";
(globalThis as unknown as { Buffer?: unknown }).Buffer ??= BufferPolyfill;

import { KernelWorkerRelay, runSessionPromptGate } from "./async-harness.js";
import type { LanguageModelSession } from "../../src/chrome-llm-adapter.js";

// The deterministic on-device model for the CI gate (the `llmock` precedent): a fixed
// sentinel so the whole inference path is assertable end-to-end. The real Nano model
// is dropped in via createChromeLanguageModelSession() for the best-effort smoke.
const SENTINEL = "PONG_FROM_CHROME_LLM";
const mockModel: LanguageModelSession = { prompt: async () => SENTINEL };

(globalThis as unknown as { __asyncInfer: unknown }).__asyncInfer = {
	async run() {
		const relay = new KernelWorkerRelay("/async-kernel.worker.js", mockModel);
		return runSessionPromptGate(relay, {
			agentType: "async-infer",
			adapterEntrypoint: "/bin/async-infer-agent",
			promptText: "ping",
		});
	},
};

const status = document.getElementById("status");
if (status) status.textContent = "ready";
