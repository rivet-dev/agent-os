// M5 demo (AGENTOS-WEB-ASYNC-AGENTS.md §6): an ACP agent answering a real prompt in
// the browser via Chrome's on-device model, through the proven in-sandbox proxy path.
// This is the leaner-agent demo (the full pi boot is a follow-on): the async-proxy
// agent runs the pi↔proxy↔inference loopback round-trip, and the proxy's host.inference
// host-callback is served by the REAL Chrome `LanguageModel` when available, falling
// back to a deterministic offline reply so the demo is verifiable even without Gemini
// Nano (the mock tier gates CI; real Nano is best-effort). Driven by the agent-browser
// verify CLI (verify-demo.mjs).

import { Buffer as BufferPolyfill } from "buffer";
(globalThis as unknown as { Buffer?: unknown }).Buffer ??= BufferPolyfill;

import { KernelWorkerRelay, runSessionPromptGate } from "./async-harness.js";
import {
	createChromeLanguageModelSession,
	type LanguageModelSession,
} from "../../src/chrome-llm-adapter.js";

interface DemoResult {
	tier: "chrome-local" | "offline-mock";
	answer?: string;
	error?: string;
}

// The offline fallback model: echoes the (flattened) prompt so a verifier can confirm
// the real question traversed the whole stack, even with no on-device model present.
const offlineMock: LanguageModelSession = {
	prompt: async (input: string) => `(offline) received your prompt → ${input}`,
};

async function run(promptText: string): Promise<DemoResult> {
	const real = await createChromeLanguageModelSession();
	const tier: DemoResult["tier"] = real ? "chrome-local" : "offline-mock";
	const relay = new KernelWorkerRelay("/async-kernel.worker.js", real ?? offlineMock);
	try {
		const result = await runSessionPromptGate(relay, {
			agentType: "async-proxy",
			adapterEntrypoint: "/bin/async-proxy-agent",
			promptText,
		});
		return { tier, answer: result.promptContent };
	} catch (error) {
		return { tier, error: error instanceof Error ? error.message : String(error) };
	}
}

(globalThis as unknown as { __agentDemo: unknown }).__agentDemo = { run };

// Minimal UI for headed/manual use; the verify CLI drives window.__agentDemo.run directly.
const input = document.getElementById("prompt") as HTMLInputElement | null;
const runButton = document.getElementById("run") as HTMLButtonElement | null;
const answerEl = document.getElementById("answer");
const tierEl = document.getElementById("tier");
runButton?.addEventListener("click", async () => {
	if (answerEl) answerEl.textContent = "…thinking";
	const out = await run(input?.value || "Say hello in three words.");
	if (tierEl) tierEl.textContent = `inference: ${out.tier}`;
	if (answerEl) answerEl.textContent = out.error ? `error: ${out.error}` : (out.answer ?? "(no answer)");
});

const status = document.getElementById("status");
if (status) status.textContent = "ready";
