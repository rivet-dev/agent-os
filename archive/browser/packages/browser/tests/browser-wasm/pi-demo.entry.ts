// Interactive demo: the real pi agent answering a prompt entirely in the browser
// (AGENTOS-WEB-ASYNC-AGENTS.md §6/M4b). A visible UI over the shared pi turn runner —
// boots pi in the converged executor, runs a full ACP turn for the user's prompt, and
// shows the model answer. Driven by the agent-browser verify CLI for screenshots.

import { Buffer as BufferPolyfill } from "buffer";
(globalThis as unknown as { Buffer?: unknown }).Buffer ??= BufferPolyfill;

import { type PiTurnResult, runPiTurn } from "./pi-runner.js";

const $ = (id: string) => document.getElementById(id);

async function run(prompt: string): Promise<PiTurnResult> {
	const stepFor = (s: string): HTMLElement | null => {
		if (/Boot/.test(s)) return $("s-boot");
		if (/initialize|session\/new|session\/prompt/.test(s)) return null;
		return null;
	};
	const answerEl = $("answer");
	const statusEl = $("status");
	const tierEl = $("tier");
	if (answerEl) answerEl.textContent = "";
	if (tierEl) tierEl.textContent = "";
	// Progressive step highlighting based on status text.
	const result = await runPiTurn({
		prompt,
		onStatus: (s) => {
			if (statusEl) statusEl.textContent = s;
			const el = stepFor(s);
			if (el) el.classList.add("done");
			if (/Booting/.test(s)) $("s-boot")?.classList.add("done");
			if (/Running pi turn/.test(s)) {
				$("s-boot")?.classList.add("done");
				$("s-init")?.classList.add("done");
				$("s-new")?.classList.add("done");
				$("s-prompt")?.classList.add("done");
			}
		},
	});
	if (result.error) {
		if (statusEl) statusEl.textContent = "Error";
		if (answerEl) answerEl.textContent = `Error: ${result.error}`;
	} else {
		if (statusEl) statusEl.textContent = "";
		if (answerEl) answerEl.textContent = result.answer ?? "(no answer)";
		if (tierEl) tierEl.textContent = "✓ answered by pi running in-browser, via the on-device model";
	}
	return result;
}

(globalThis as unknown as { __piDemo: unknown }).__piDemo = { run };

const button = $("run") as HTMLButtonElement | null;
const input = $("prompt") as HTMLInputElement | null;
button?.addEventListener("click", async () => {
	if (button) button.disabled = true;
	try {
		await run(input?.value || "What is 2+2?");
	} finally {
		if (button) button.disabled = false;
	}
});

const status = $("status");
if (status) status.textContent = "ready";
