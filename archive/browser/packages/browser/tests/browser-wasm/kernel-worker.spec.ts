import { expect, test } from "@playwright/test";

// M2a: the Agent OS converged kernel boots INSIDE a dedicated worker and answers
// real wire frames through a main-thread async relay — the structural inversion the
// async-agent executor needs (spec §3.1). authenticate + an ACP get_session_state
// round-trip prove the wasm sidecar (kernel + AcpCore) runs in the worker, not on
// the main thread.

test("the agentos kernel boots in a worker and answers wire frames via the relay", async ({
	page,
}) => {
	await page.goto("/kernel-worker.html");
	await expect(page.locator("#status")).toHaveText("ready", { timeout: 20_000 });

	const result = await page.evaluate(() => window.__kernelWorker!.run());

	expect(result.sidecarId).toBeTruthy();
	expect(result.authResp.payloadType).toBe("authenticated");
	expect(result.acpResp.payloadType).toBe("ext_result");
	expect(result.acpResp.acpTag).toBe("AcpErrorResponse");
	expect(result.acpResp.acpMessage ?? "").toContain("unknown ACP session");
});

declare global {
	interface Window {
		__kernelWorker?: {
			run(): Promise<{
				sidecarId: string;
				authResp: { payloadType?: string };
				acpResp: { payloadType?: string; acpTag?: string; acpMessage?: string };
			}>;
		};
	}
}
