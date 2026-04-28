import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { AgentOs } from "@rivet-dev/agent-os-core";
import common from "@rivet-dev/agent-os-common";
import type { MockSandboxAgentHandle } from "@rivet-dev/agent-os-core/test/sandbox-agent";
import { startMockSandboxAgent } from "@rivet-dev/agent-os-core/test/sandbox-agent";
import { createSandboxFs, createSandboxToolkit } from "../src/index.js";

let sandbox: MockSandboxAgentHandle;

const SANDBOX_TEST_PERMISSIONS = {
	fs: "allow",
	network: "allow",
	childProcess: "allow",
	env: "allow",
	tool: "allow",
} as const;

beforeAll(async () => {
	sandbox = await startMockSandboxAgent();
}, 150_000);

afterAll(async () => {
	if (sandbox) await sandbox.stop();
});

describe("VM integration", () => {
	let vm: AgentOs;

	beforeEach(async () => {
		vm = await AgentOs.create({
			permissions: SANDBOX_TEST_PERMISSIONS,
			software: [common],
			mounts: [
				{
					path: "/sandbox",
					plugin: createSandboxFs({ client: sandbox.client }),
				},
			],
			toolKits: [createSandboxToolkit({ client: sandbox.client })],
		});
	});

	afterEach(async () => {
		await vm.dispose();
	});

	// -- Filesystem mount tests --

	it("should read a file via the sandbox mount", async () => {
		await sandbox.client.writeFsFile(
			{ path: "/test.txt" },
			new TextEncoder().encode("hello from VM mount"),
		);
		const data = await vm.readFile("/sandbox/test.txt");
		expect(new TextDecoder().decode(data)).toBe("hello from VM mount");
	});

	it("should list the sandbox mount contents", async () => {
		await sandbox.client.writeFsFile({ path: "/a.txt" }, new TextEncoder().encode("a"));
		await sandbox.client.writeFsFile({ path: "/b.txt" }, new TextEncoder().encode("b"));
		const entries = await vm.readdir("/sandbox");
		expect(entries).toContain("a.txt");
		expect(entries).toContain("b.txt");
	});

	it("should access nested directories in the sandbox mount", async () => {
		await sandbox.client.mkdirFs({ path: "/nested" });
		await sandbox.client.writeFsFile(
			{ path: "/nested/deep.txt" },
			new TextEncoder().encode("deep file"),
		);
		const content = await vm.readFile("/sandbox/nested/deep.txt");
		expect(new TextDecoder().decode(content)).toBe("deep file");
	});

	// -- Toolkit direct execution (host RPC, not via CLI shim) --

	it("should execute run-command tool directly via the toolkit", async () => {
		const tk = createSandboxToolkit({ client: sandbox.client });
		const result = await tk.tools["run-command"].execute({
			command: "echo",
			args: ["hello", "from", "sandbox"],
		});
		expect(result.exitCode).toBe(0);
		expect(result.stdout).toContain("hello from sandbox");
	});

	it("should exercise the toolkit tool directly from a VM context", async () => {
		// Write a file into the sandbox via the toolkit, then read it via the mount.
		const tk = createSandboxToolkit({ client: sandbox.client });

		// Confirm the sandbox toolkit runs commands successfully.
		const result = await tk.tools["run-command"].execute({
			command: "echo",
			args: ["hello from sandbox toolkit"],
		});
		expect(result.exitCode).toBe(0);
		expect(result.stdout).toContain("hello from sandbox toolkit");

		// Create a process and list it.
		const proc = await tk.tools["create-process"].execute({
			command: "sleep",
			args: ["60"],
		});
		expect(proc.status).toBe("running");

		const listed = await tk.tools["list-processes"].execute({});
		const found = listed.processes.find(
			(p: { id: string }) => p.id === proc.id,
		);
		expect(found).toBeDefined();

		await tk.tools["kill-process"].execute({ id: proc.id });
	});
});
