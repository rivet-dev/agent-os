import { expect, test } from "@playwright/test";

// M4 groundwork gate: loopback TCP driven from inside the async-agent executor in real
// Chromium (AGENTOS-WEB-ASYNC-AGENTS.md §6). The agent answers session/prompt by
// running a full loopback handshake (listen/connect/accept/write/read on 127.0.0.1)
// through mid-turn net.* syscalls, serviced inline by the reactor through the kernel
// socket table, and echoes the bytes it read. Proves net.* routes through the same
// pushFrame path as fs.*, de-risking the in-sandbox HTTP proxy guest.

test("session/prompt drives a loopback TCP handshake via mid-turn net.* syscalls", async ({
	page,
}) => {
	await page.goto("/async-loopback.html");
	await expect(page.locator("#status")).toHaveText("ready", { timeout: 20_000 });

	const result = await page.evaluate(() => window.__asyncLoopback!.run());

	expect(result.sidecarId).toBeTruthy();
	expect(result.payloadType).toBe("ext_result");
	expect(result.acpTag).toBe("AcpSessionCreatedResponse");
	expect(result.sessionId).toBe("async-loopback-session");
	// The bytes written over the loopback socket came back through net.read.
	expect(result.promptContent).toBe("ping-loopback");
});

declare global {
	interface Window {
		__asyncLoopback?: {
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
