import { expect, test } from "@playwright/test";

// M4 gate: the async-inference transport end-to-end in real Chromium
// (AGENTOS-WEB-ASYNC-AGENTS.md §6). An ASYNC agent answers a session/prompt by making
// a mid-turn `host.inference` syscall; the kernel DEFERS it to the main thread (the
// chrome-llm host-callback), the guest parks in its SAB shim, the main thread runs the
// on-device model (a deterministic mock sentinel here), writes the OpenAI-shaped reply
// to the completion channel, and the reactor unblocks the guest. The prompt content
// is the model's reply, proving the whole deferred-syscall → host-callback →
// completion-channel path ran without the kernel worker block-waiting on the async hop.

test("session/prompt answered via a DEFERRED host.inference syscall (chrome-llm host-callback)", async ({
	page,
}) => {
	await page.goto("/async-infer.html");
	await expect(page.locator("#status")).toHaveText("ready", { timeout: 20_000 });

	const result = await page.evaluate(() => window.__asyncInfer!.run());

	expect(result.sidecarId).toBeTruthy();
	expect(result.payloadType).toBe("ext_result");
	expect(result.acpTag).toBe("AcpSessionCreatedResponse");
	expect(result.sessionId).toBe("async-infer-session");
	// The model reply (mock sentinel) flowed: guest deferred syscall → main-thread
	// chrome-llm adapter → completion channel → guest → echoed as the prompt content.
	expect(result.promptContent).toBe("PONG_FROM_CHROME_LLM");
});

declare global {
	interface Window {
		__asyncInfer?: {
			run(): Promise<{
				sidecarId: string;
				payloadType?: string;
				acpTag?: string;
				sessionId?: string;
				promptContent?: string;
			}>;
		};
	}
}
