import { expect, test } from "@playwright/test";

// Real ACP request/response round-trip through the converged Agent OS wasm sidecar
// in Chromium: a wire ExtEnvelope (ACP get_session_state for a missing session)
// reaches BrowserAcpExtension -> AcpCore and returns the ACP error response —
// proving the ACP extension is live end-to-end in the browser (no agent execution
// required for this path).

test("ACP get_session_state round-trips through the wasm sidecar in Chromium", async ({
	page,
}) => {
	await page.goto("/");
	await expect(page.locator("#status")).toHaveText("ready");

	const result = await page.evaluate(() => window.__agentosAcp.run());

	// Authentication handshake succeeded over the wire.
	expect(result.authResp.payloadType).toBe("authenticated");
	// The ACP ext request produced a response frame carrying an ACP ext payload.
	expect(result.acpResp.frameType).toBe("response");
	expect(result.acpResp.payloadType).toBe("ext_result");
	// AcpCore answered get_session_state for a missing session with its error.
	expect(result.acpResp.acpTag).toBe("AcpErrorResponse");
	expect(result.acpResp.acpMessage ?? "").toContain("unknown ACP session");
});

declare global {
	interface Window {
		__agentosAcp: {
			run(): Promise<{
				authResp: { frameType: string; payloadType?: string };
				acpResp: {
					frameType: string;
					payloadType?: string;
					acpTag?: string;
					acpMessage?: string;
					rejected?: { code?: string; message?: string };
				};
			}>;
		};
	}
}
