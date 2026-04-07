import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, test, vi } from "vitest";
import { NativeSidecarKernelProxy } from "../src/sidecar/native-kernel-proxy.js";
import type {
	AuthenticatedSession,
	CreatedVm,
	NativeSidecarProcessClient,
} from "../src/sidecar/native-process-client.js";

describe("NativeSidecarKernelProxy execute payloads", () => {
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

		return { client, execute };
	}

	async function captureExecutePayload() {
		fixtureRoot = mkdtempSync(join(tmpdir(), "agent-os-allowed-builtins-"));
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
			commandGuestPaths: new Map(),
		});

		const proc = proxy.spawn("node", ["/workspace/entry.mjs"], {
			cwd: "/workspace",
			env: { HOME: "/workspace" },
		});
		const exitCode = await proc.wait();

		expect(exitCode).toBe(1);
		expect(execute).toHaveBeenCalledTimes(1);
		return execute.mock.calls[0]?.[2];
	}

	test("leaves internal AGENT_OS runtime env construction to the sidecar", async () => {
		await expect(captureExecutePayload()).resolves.toMatchObject({
			command: "node",
			args: ["/workspace/entry.mjs"],
			cwd: "/workspace",
			env: { HOME: "/workspace" },
		});
		await expect(captureExecutePayload()).resolves.not.toMatchObject({
			env: {
				AGENT_OS_ALLOWED_NODE_BUILTINS: expect.anything(),
			},
		});
	});

	test("exec forwards shell commands to the guest sh driver without TypeScript parsing", async () => {
		fixtureRoot = mkdtempSync(join(tmpdir(), "agent-os-shell-exec-"));
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
			commandGuestPaths: new Map([["sh", "/__agentos/commands/000/sh"]]),
		});

		await expect(proxy.exec("node /workspace/entry.mjs --flag")).resolves
			.toMatchObject({
				exitCode: 1,
			});
		expect(execute).toHaveBeenCalledTimes(1);
		expect(execute.mock.calls[0]?.[2]).toMatchObject({
			command: "sh",
			args: ["-c", "node /workspace/entry.mjs --flag"],
			cwd: "/workspace",
		});
	});

	test("exec rejects when the guest shell command is unavailable", async () => {
		fixtureRoot = mkdtempSync(join(tmpdir(), "agent-os-shell-missing-"));
		const { client } = createMockClient();

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
		});

		await expect(proxy.exec("node /workspace/entry.mjs")).rejects.toThrow(
			"native sidecar exec requires guest shell command 'sh'",
		);
	});
});
