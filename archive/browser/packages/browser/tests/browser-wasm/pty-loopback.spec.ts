import { expect, test } from "@playwright/test";

// T2 gate (AGENTOS-WEB-PTY-TERMINAL.md): kernel PTY through the converged browser path.
// The agent answers session/prompt by driving a full pseudo-terminal loopback through
// mid-turn pty.* syscalls — pty.open, raw-mode pty.tcsetattr, pty.write to the master,
// pty.read from the slave (input line discipline), pty.write to the slave, pty.read
// from the master (output line discipline), pty.close — all serviced by the new
// guest_pty dispatcher in the wasm sidecar over the same pushFrame path as net.*/fs.*.
// The reply "ECHO:ping-pty" round-tripping master→slave→master proves the kernel's real
// PtyManager + line discipline are reachable from the browser converged executor.

test("session/prompt drives a kernel PTY loopback via mid-turn pty.* syscalls", async ({
	page,
}) => {
	await page.goto("/pty-loopback.html");
	await expect(page.locator("#status")).toHaveText("ready", { timeout: 20_000 });

	const result = await page.evaluate(() => window.__ptyLoopback!.run());

	expect(result.sidecarId).toBeTruthy();
	expect(result.payloadType).toBe("ext_result");
	expect(result.acpTag).toBe("AcpSessionCreatedResponse");
	expect(result.sessionId).toBe("pty-loopback-session");
	// "ping-pty" written to the master came back through the slave; the agent's
	// "ECHO:" reply written to the slave came back through the master.
	expect(result.promptContent).toBe("ECHO:ping-pty");
});

declare global {
	interface Window {
		__ptyLoopback?: {
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
