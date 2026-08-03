import { SandboxAgent, type SandboxProvider } from "sandbox-agent";
import { describe, expect, test, vi } from "vitest";
import { sandboxAgentProvider } from "../src/provider.js";

describe("sandboxAgentProvider", () => {
	test("starts a fresh client and destroys its backend on disposal", async () => {
		const destroySandbox = vi.fn(async () => {});
		const runProcess = vi.fn(async () => ({ stdout: "ok", exitCode: 0 }));
		const awaitHealthy = vi.fn(async () => {});
		const fetcher = vi.fn(
			async (_input: RequestInfo | URL, _init?: RequestInit) =>
				new Response("ok"),
		);
		const client = {
			baseUrl: "https://sandbox.example",
			token: "current-token",
			defaultHeaders: { "x-sandbox-provider": "test" },
			fetcher,
			awaitHealthy,
			destroySandbox,
			runProcess,
		};
		const start = vi
			.spyOn(SandboxAgent, "start")
			.mockResolvedValue(client as never);
		const backend = { name: "test" } as SandboxProvider;
		const provider = sandboxAgentProvider(backend);

		const first = await provider.start();
		const second = await provider.start();
		expect(start).toHaveBeenNthCalledWith(1, { sandbox: backend });
		expect(start).toHaveBeenNthCalledWith(2, { sandbox: backend });
		await expect(first.runProcess({ command: "echo" })).resolves.toEqual({
			stdout: "ok",
			exitCode: 0,
		});
		await first.request?.("/v1/fs/stat?path=%2F", {
			headers: { range: "bytes=0-3" },
		});
		expect(awaitHealthy).toHaveBeenCalledTimes(1);
		expect(fetcher).toHaveBeenCalledTimes(1);
		const [requestUrl, requestInit] = fetcher.mock.calls[0] ?? [];
		expect(String(requestUrl)).toBe(
			"https://sandbox.example/v1/fs/stat?path=%2F",
		);
		const headers = new Headers(requestInit?.headers);
		expect(headers.get("authorization")).toBe("Bearer current-token");
		expect(headers.get("x-sandbox-provider")).toBe("test");
		expect(headers.get("range")).toBe("bytes=0-3");
		await first.dispose?.();
		await second.dispose?.();
		expect(destroySandbox).toHaveBeenCalledTimes(2);

		start.mockRestore();
	});
});
