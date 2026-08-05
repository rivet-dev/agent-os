import { expect, test } from "@playwright/test";

// M3 gate: a full ACP create_session against an ASYNC agent running in its own
// worker, driven by the resumable path through the in-worker kernel. The agent
// replies on the event loop (not synchronously), so this could NOT work under the
// old synchronous AcpCore (the kernel would block inside pushFrame); the resumable
// begin/feed state machine + the drive loop make it work. Proves the re-entrancy
// fix end-to-end in real Chromium.

test("create_session drives an ASYNC agent (in its own worker) to a created session via the resumable path", async ({
	page,
}) => {
	await page.goto("/async-agent.html");
	await expect(page.locator("#status")).toHaveText("ready", { timeout: 20_000 });

	const result = await page.evaluate(() => window.__asyncAgent!.run());

	expect(result.sidecarId).toBeTruthy();
	expect(result.payloadType).toBe("ext_result");
	expect(result.acpTag, JSON.stringify(result, null, 2)).toBe(
		"AcpSessionCreatedResponse",
	);
	expect(result.sessionId).toBe("async-echo-session");
	// HARDENED: the agent made an fs syscall MID-PROMPT, serviced inline by the
	// reactor (the case that re-enters pushFrame under the synchronous path), and
	// echoed the file content back — proving inline mid-turn syscall servicing.
	expect(result.promptContent).toBe("hello-from-mid-turn-syscall");
});

declare global {
	interface Window {
		__asyncAgent?: {
			run(): Promise<{
				sidecarId: string;
				payloadType?: string;
				acpTag?: string;
				sessionId?: string;
				acpCode?: string;
				acpMessage?: string;
				promptContent?: string;
			}>;
		};
	}
}
