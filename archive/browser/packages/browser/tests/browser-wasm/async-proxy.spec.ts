import { expect, test } from "@playwright/test";

// M4 gate: the in-sandbox OpenAI proxy end-to-end in real Chromium
// (AGENTOS-WEB-ASYNC-AGENTS.md §6). An async agent runs a full pi↔proxy↔inference
// round-trip in one turn: a loopback HTTP POST of an OpenAI chat-completions request →
// the proxy forwards the body to on-device inference via the host.inference
// host-callback (mock sentinel) → an HTTP 200 carrying the OpenAI reply → the client
// extracts the assistant message. The prompt content is that message, proving HTTP over
// loopback + the deferred inference host-callback compose into the path pi will drive.

test("session/prompt runs the pi↔proxy↔inference loopback round-trip (in-sandbox OpenAI proxy)", async ({
	page,
}) => {
	await page.goto("/async-proxy.html");
	await expect(page.locator("#status")).toHaveText("ready", { timeout: 20_000 });

	const result = await page.evaluate(() => window.__asyncProxy!.run());

	expect(result.sidecarId).toBeTruthy();
	expect(result.payloadType).toBe("ext_result");
	expect(result.acpTag).toBe("AcpSessionCreatedResponse");
	expect(result.sessionId).toBe("async-proxy-session");
	// The assistant message flowed: HTTP request → proxy → host.inference → HTTP 200 →
	// client → extracted content.
	expect(result.promptContent).toBe("PONG_FROM_CHROME_LLM");
});

declare global {
	interface Window {
		__asyncProxy?: {
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
