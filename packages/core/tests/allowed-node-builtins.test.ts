import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, test, vi } from "vitest";
import type { AgentOsOptions } from "../src/index.js";
import { NativeSidecarKernelProxy } from "../src/sidecar/native-kernel-proxy.js";
import type {
	AuthenticatedSession,
	CreatedVm,
	NativeSidecarProcessClient,
} from "../src/sidecar/native-process-client.js";

describe("AgentOsOptions.allowedNodeBuiltins", () => {
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

	test("overrides the native sidecar Node builtin allowlist for guest executions", async () => {
		const options: AgentOsOptions = {
			allowedNodeBuiltins: ["worker_threads"],
		};
		fixtureRoot = mkdtempSync(join(tmpdir(), "agent-os-allowed-builtins-"));

		let stopped = false;
		const execute = vi.fn(
			async (
				_session: AuthenticatedSession,
				_vm: CreatedVm,
				_execution: { env?: Record<string, string> },
			) => {
				throw new Error("stop after capture");
			},
		);
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
			commandGuestPaths: new Map(),
			hostPathMappings: [
				{
					guestPath: "/workspace",
					hostPath: fixtureRoot,
				},
			],
			allowedNodeBuiltins: options.allowedNodeBuiltins,
			nodeExecutionCwd: "/workspace",
		});

		const proc = proxy.spawn("node", ["/workspace/entry.mjs"], {
			cwd: "/workspace",
			env: { HOME: "/workspace" },
		});
		const exitCode = await proc.wait();

		expect(exitCode).toBe(1);
		expect(execute).toHaveBeenCalledTimes(1);
		expect(execute.mock.calls[0]?.[2]?.env?.AGENT_OS_ALLOWED_NODE_BUILTINS).toBe(
			JSON.stringify(options.allowedNodeBuiltins),
		);
	});
});
