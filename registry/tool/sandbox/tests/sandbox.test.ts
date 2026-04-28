import { afterAll, afterEach, beforeAll, describe, expect, it } from "vitest";
import { AgentOs } from "@rivet-dev/agent-os-core";
import type { MockSandboxAgentHandle } from "@rivet-dev/agent-os-core/test/sandbox-agent";
import { startMockSandboxAgent } from "@rivet-dev/agent-os-core/test/sandbox-agent";
import { createSandboxFs, createSandboxToolkit } from "../src/index.js";

let sandbox: MockSandboxAgentHandle;
let vm: AgentOs | null = null;

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

afterEach(async () => {
	if (vm) {
		await vm.dispose();
		vm = null;
	}
});

describe("@rivet-dev/agent-os-sandbox", () => {
	// -----------------------------------------------------------------------
	// Mount helper tests
	// -----------------------------------------------------------------------
	describe("mount helper", () => {
		it("should serialize a native sandbox_agent mount descriptor", () => {
			const mount = createSandboxFs({
				client: sandbox.client,
				basePath: sandbox.path("scoped"),
				timeoutMs: 12_345,
				maxFullReadBytes: 4096,
			});

			expect(mount).toMatchObject({
				id: "sandbox_agent",
				config: {
					basePath: sandbox.path("scoped"),
					timeoutMs: 12_345,
					maxFullReadBytes: 4096,
				},
			});
			expect(mount.config.baseUrl).toMatch(/^https?:\/\//);
		});

		it("should support basePath scoping when mounted into AgentOs", async () => {
			await sandbox.client.writeFsFile(
				{ path: "/scoped/scoped-file.txt" },
				new TextEncoder().encode("scoped"),
			);
			vm = await AgentOs.create({
				permissions: SANDBOX_TEST_PERMISSIONS,
				mounts: [
					{
						path: "/sandbox",
						plugin: createSandboxFs({
							client: sandbox.client,
							basePath: "/scoped",
						}),
					},
				],
			});

			const content = await vm.readFile("/sandbox/scoped-file.txt");
			expect(new TextDecoder().decode(content)).toBe("scoped");
		});
	});

	// -----------------------------------------------------------------------
	// Toolkit tests
	// -----------------------------------------------------------------------
	describe("toolkit", () => {
		it("should have the correct name and tools", () => {
			const tk = createSandboxToolkit({ client: sandbox.client });
			expect(tk.name).toBe("sandbox");
			expect(Object.keys(tk.tools)).toContain("run-command");
			expect(Object.keys(tk.tools)).toContain("create-process");
			expect(Object.keys(tk.tools)).toContain("list-processes");
			expect(Object.keys(tk.tools)).toContain("stop-process");
			expect(Object.keys(tk.tools)).toContain("kill-process");
			expect(Object.keys(tk.tools)).toContain("get-process-logs");
			expect(Object.keys(tk.tools)).toContain("send-input");
		});

		it("run-command: should execute and return output", async () => {
			const tk = createSandboxToolkit({ client: sandbox.client });
			const result = await tk.tools["run-command"].execute({
				command: "echo",
				args: ["hello", "sandbox"],
			});
			expect(result.exitCode).toBe(0);
			expect(result.stdout).toContain("hello sandbox");
		});

		it("run-command: should capture stderr on failure", async () => {
			const tk = createSandboxToolkit({ client: sandbox.client });
			const result = await tk.tools["run-command"].execute({
				command: "ls",
				args: ["/nonexistent-path-xyz"],
			});
			expect(result.exitCode).not.toBe(0);
			expect(result.stderr.length).toBeGreaterThan(0);
		});

		it("run-command: should respect cwd", async () => {
			const tk = createSandboxToolkit({ client: sandbox.client });
			const result = await tk.tools["run-command"].execute({
				command: "pwd",
				cwd: sandbox.rootDir,
			});
			expect(result.exitCode).toBe(0);
			expect(result.stdout.trim()).toBe(sandbox.rootDir);
		});

		it("run-command: should pass env vars", async () => {
			const tk = createSandboxToolkit({ client: sandbox.client });
			const result = await tk.tools["run-command"].execute({
				command: "sh",
				args: ["-c", "echo $MY_VAR"],
				env: { MY_VAR: "test-value" },
			});
			expect(result.exitCode).toBe(0);
			expect(result.stdout.trim()).toBe("test-value");
		});

		it("create-process + list-processes + kill-process", async () => {
			const tk = createSandboxToolkit({ client: sandbox.client });

			const created = await tk.tools["create-process"].execute({
				command: "sleep",
				args: ["300"],
			});
			expect(created.id).toBeTruthy();
			expect(created.status).toBe("running");

			const listed = await tk.tools["list-processes"].execute({});
			const found = listed.processes.find(
				(p: { id: string }) => p.id === created.id,
			);
			expect(found).toBeDefined();
			expect(found!.status).toBe("running");

			const killed = await tk.tools["kill-process"].execute({
				id: created.id,
			});
			expect(killed.status).toBe("exited");
		});

		it("stop-process: should gracefully stop a process", async () => {
			const tk = createSandboxToolkit({ client: sandbox.client });

			const created = await tk.tools["create-process"].execute({
				command: "sleep",
				args: ["300"],
			});
			expect(created.status).toBe("running");

			const stopped = await tk.tools["stop-process"].execute({
				id: created.id,
			});
			expect(stopped.status).toBe("exited");
		});

		it("get-process-logs: should retrieve decoded process output", async () => {
			const tk = createSandboxToolkit({ client: sandbox.client });

			// Create a process that produces output.
			const proc = await tk.tools["create-process"].execute({
				command: "sh",
				args: ["-c", "echo log-output-a && echo log-output-b"],
			});

			// Give the process time to finish writing.
			await new Promise((resolve) => setTimeout(resolve, 1000));

			// The toolkit should decode base64 logs automatically.
			const logs = await tk.tools["get-process-logs"].execute({
				id: proc.id,
			});
			const combined = logs.logs
				.map((l: { data: string }) => l.data)
				.join("");
			expect(combined).toContain("log-output-a");
			expect(combined).toContain("log-output-b");
		});

		it("send-input: should send stdin data to an interactive process", async () => {
			const tk = createSandboxToolkit({ client: sandbox.client });

			// Start an interactive process via the SDK directly since
			// create-process doesn't expose the interactive flag.
			const proc = await sandbox.client.createProcess({
				command: "cat",
				interactive: true,
			});

			// Send input via the toolkit tool.
			await tk.tools["send-input"].execute({
				id: proc.id,
				data: "hello from stdin\n",
			});

			// Give it time to echo.
			await new Promise((resolve) => setTimeout(resolve, 500));

			const logs = await tk.tools["get-process-logs"].execute({
				id: proc.id,
			});
			const combined = logs.logs
				.map((l: { data: string }) => l.data)
				.join("");
			expect(combined).toContain("hello from stdin");

			// Clean up.
			await tk.tools["kill-process"].execute({ id: proc.id });
		});

		it("fs + toolkit integration: write via fs, read via run-command", async () => {
			const tk = createSandboxToolkit({ client: sandbox.client });
			const integratedPath = "/integrated-test.txt";

			await sandbox.client.writeFsFile(
				{ path: integratedPath },
				new TextEncoder().encode("integration works"),
			);

			const result = await tk.tools["run-command"].execute({
				command: "cat",
				args: [integratedPath],
			});
			expect(result.exitCode).toBe(0);
			expect(result.stdout).toBe("integration works");
		});

		it("fs + toolkit integration: write via run-command, read via fs", async () => {
			const tk = createSandboxToolkit({ client: sandbox.client });
			const shellPath = "/shell-wrote.txt";

			const result = await tk.tools["run-command"].execute({
				command: "sh",
				args: ["-c", "echo 'written by shell' > shell-wrote.txt"],
				cwd: sandbox.rootDir,
			});
			expect(result.exitCode).toBe(0);

			const content = await sandbox.client.readFsFile({
				path: shellPath,
			});
			expect(new TextDecoder().decode(content).trim()).toBe("written by shell");
		});
	});
});
