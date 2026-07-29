import common from "@agentos-software/common";
import { AgentOs } from "@rivet-dev/agentos-core";
import type { MockSandboxAgentHandle } from "@rivet-dev/agentos-core/test/sandbox-agent";
import { startMockSandboxAgent } from "@rivet-dev/agentos-core/test/sandbox-agent";
import {
	afterAll,
	afterEach,
	beforeAll,
	beforeEach,
	describe,
	expect,
	it,
} from "vitest";
import { createSandboxBindings } from "../src/index.js";

let sandbox: MockSandboxAgentHandle;

const SANDBOX_TEST_PERMISSIONS = {
	fs: "allow",
	network: "allow",
	childProcess: "allow",
	env: "allow",
	binding: "allow",
} as const;

beforeAll(async () => {
	sandbox = await startMockSandboxAgent();
}, 150_000);

afterAll(async () => {
	if (sandbox) await sandbox.stop();
});

describe("VM integration", () => {
	let vm!: AgentOs;
	let providerStarts: number;
	let providerDisposals: number;

	beforeEach(async () => {
		providerStarts = 0;
		providerDisposals = 0;
		vm = await AgentOs.create({
			permissions: SANDBOX_TEST_PERMISSIONS,
			software: [common],
			sandbox: {
				mountPath: "/sandbox",
				provider: {
					start: async () => {
						providerStarts += 1;
						return new Proxy(sandbox.client, {
							get(target, property) {
								if (property === "dispose") {
									return () => {
										providerDisposals += 1;
									};
								}
								const value = Reflect.get(target, property, target);
								return typeof value === "function" ? value.bind(target) : value;
							},
						}) as never;
					},
				},
			},
		});
		expect(providerStarts).toBe(1);
	}, 150_000);

	afterEach(async () => {
		if (vm) await vm.dispose();
		expect(providerDisposals).toBe(1);
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
		await sandbox.client.writeFsFile(
			{ path: "/a.txt" },
			new TextEncoder().encode("a"),
		);
		await sandbox.client.writeFsFile(
			{ path: "/b.txt" },
			new TextEncoder().encode("b"),
		);
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

	// -- Bindings direct execution (host RPC, not via CLI shim) --

	it("should execute the run-command binding directly via the binding collection", async () => {
		const tk = createSandboxBindings({ client: sandbox.client });
		const result = await tk.bindings["run-command"].execute({
			command: "echo",
			args: ["hello", "from", "sandbox"],
		});
		expect(result.exitCode).toBe(0);
		expect(result.stdout).toContain("hello from sandbox");
	});

	it("should exercise the binding collection directly from a VM context", async () => {
		// Write a file into the sandbox via the binding collection, then read it via the mount.
		const tk = createSandboxBindings({ client: sandbox.client });

		// Confirm the sandbox binding collection runs commands successfully.
		const result = await tk.bindings["run-command"].execute({
			command: "echo",
			args: ["hello from sandbox binding collection"],
		});
		expect(result.exitCode).toBe(0);
		expect(result.stdout).toContain("hello from sandbox binding collection");

		// Create a process and list it.
		const proc = await tk.bindings["create-process"].execute({
			command: "sleep",
			args: ["60"],
		});
		expect(proc.status).toBe("running");

		const listed = await tk.bindings["list-processes"].execute({});
		const found = listed.processes.find(
			(p: { id: string }) => p.id === proc.id,
		);
		expect(found).toBeDefined();

		await tk.bindings["kill-process"].execute({ id: proc.id });
	});
});
