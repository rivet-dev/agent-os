import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, test, vi } from "vitest";
import { NativeSidecarKernelProxy } from "../../src/sidecar/native-kernel-proxy.js";
import type {
	AuthenticatedSession,
	CreatedVm,
	NativeSidecarProcessClient,
} from "../../src/sidecar/native-process-client.js";

describe("WASM command permission tiers", () => {
	let proxy: NativeSidecarKernelProxy | null = null;
	let fixtureRoot: string | null = null;

	afterEach(async () => {
		await proxy?.dispose();
		proxy = null;
		if (fixtureRoot) {
			rmSync(fixtureRoot, { recursive: true, force: true });
			fixtureRoot = null;
		}
	});

	function createMockClient() {
		let stopped = false;
		const execute = vi.fn(async () => {
			throw new Error("stop after capture");
		});
		const client = {
			waitForEvent: vi.fn(async () => {
				while (!stopped) {
					await new Promise((resolve) => setTimeout(resolve, 1));
				}
				throw new Error("mock stopped");
			}),
			execute,
			disposeVm: vi.fn(async () => {
				stopped = true;
			}),
			dispose: vi.fn(async () => {
				stopped = true;
			}),
		} as unknown as NativeSidecarProcessClient;

		return { client, execute };
	}

	test("propagates per-command WASM tiers into sidecar execute requests", async () => {
		fixtureRoot = mkdtempSync(join(tmpdir(), "agent-os-wasm-tiers-"));
		const { client, execute } = createMockClient();

		proxy = new NativeSidecarKernelProxy({
			client,
			session: {
				connectionId: "conn-1",
				sessionId: "session-1",
			} as AuthenticatedSession,
			vm: { vmId: "vm-1" } as CreatedVm,
			env: { HOME: "/workspace" },
			cwd: "/workspace",
			localMounts: [],
			commandGuestPaths: new Map([["grep", "/__agentos/commands/000/grep"]]),
			wasmCommandPermissions: { grep: "read-only" },
			hostPathMappings: [
				{
					guestPath: "/workspace",
					hostPath: fixtureRoot,
				},
			],
			nodeExecutionCwd: "/workspace",
		});

		const proc = proxy.spawn("grep", ["needle", "haystack.txt"], {
			cwd: "/workspace",
		});
		const exitCode = await proc.wait();

		expect(exitCode).toBe(1);
		expect(execute).toHaveBeenCalledTimes(1);
		expect(execute.mock.calls[0]?.[2]).toMatchObject({
			runtime: "web_assembly",
			entrypoint: "/__agentos/commands/000/grep",
			wasmPermissionTier: "read-only",
		});
	});
});
