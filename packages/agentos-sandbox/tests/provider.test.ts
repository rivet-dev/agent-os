import { SandboxAgent, type SandboxProvider } from "sandbox-agent";
import { describe, expect, test, vi } from "vitest";
import { sandboxAgentProvider } from "../src/provider.js";

describe("sandboxAgentProvider", () => {
	test("starts a fresh client and destroys its backend on disposal", async () => {
		const destroySandbox = vi.fn(async () => {});
		const runProcess = vi.fn(async () => ({ stdout: "ok", exitCode: 0 }));
		const client = {
			baseUrl: "https://sandbox.example",
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
		await first.dispose?.();
		await second.dispose?.();
		expect(destroySandbox).toHaveBeenCalledTimes(2);

		start.mockRestore();
	});
});
