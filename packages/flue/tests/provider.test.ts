import { createHash } from "node:crypto";
import { beforeEach, describe, expect, it, vi } from "vitest";

const clientMocks = vi.hoisted(() => ({ createClient: vi.fn() }));

vi.mock("@rivet-dev/agentos/client", () => ({
	createClient: clientMocks.createClient,
}));

import { AgentOSFlueConfigurationError, agentOSSandbox } from "../src/index.js";

function makeRegistry() {
	return {
		startAndWait: vi.fn(async () => {}),
	};
}

function makeHarness() {
	const keyCalls: string[][] = [];
	const connection = {
		ready: Promise.resolve(),
		exec: vi.fn(async () => ({ exitCode: 0, stdout: "ok", stderr: "" })),
		readFile: vi.fn(async () => new TextEncoder().encode("hello")),
		writeFile: vi.fn(async () => {}),
		stat: vi.fn(async () => ({
			mode: 0o100644,
			size: 5,
			isDirectory: false,
			isSymbolicLink: false,
			mtimeMs: 1234,
		})),
		readdir: vi.fn(async () => ["a.txt"]),
		exists: vi.fn(async () => true),
		mkdir: vi.fn(async () => {}),
		remove: vi.fn(async () => {}),
	};
	const connect = vi.fn(() => connection);
	const getOrCreate = vi.fn(
		(key: string[], _options?: { params?: unknown }) => {
			keyCalls.push(key);
			return { connect };
		},
	);
	clientMocks.createClient.mockReturnValue({ vm: { getOrCreate } });
	return { connect, connection, getOrCreate, keyCalls };
}

describe("agentOSSandbox", () => {
	beforeEach(() => clientMocks.createClient.mockReset());

	it("reconnects to the same actor without retaining session environments", async () => {
		const registry = makeRegistry();
		const harness = makeHarness();
		const sandbox = agentOSSandbox({ actor: "vm", registry });
		const first = await sandbox.createSessionEnv({ id: "ticket-1" });
		const second = await sandbox.createSessionEnv({ id: "ticket-1" });

		expect(first).not.toBe(second);
		expect(registry.startAndWait).toHaveBeenCalledTimes(2);
		expect(harness.connect).toHaveBeenCalledTimes(2);
		expect(harness.keyCalls).toEqual([
			[
				"flue",
				"sandbox",
				createHash("sha256").update("ticket-1").digest("hex"),
			],
			[
				"flue",
				"sandbox",
				createHash("sha256").update("ticket-1").digest("hex"),
			],
		]);
		expect(harness.getOrCreate).toHaveBeenCalledTimes(2);
		expect(clientMocks.createClient).toHaveBeenCalledOnce();
		expect(clientMocks.createClient).toHaveBeenCalledWith();
	});

	it("forwards actor connection parameters", async () => {
		const harness = makeHarness();
		await agentOSSandbox({
			actor: "vm",
			registry: makeRegistry(),
			params: { authToken: "allowed" },
		}).createSessionEnv({ id: "ticket-auth" });

		expect(harness.getOrCreate).toHaveBeenCalledWith(expect.any(Array), {
			params: { authToken: "allowed" },
		});
	});

	it("maps Flue shell and filesystem operations to agentOS", async () => {
		const harness = makeHarness();
		const env = await agentOSSandbox({
			actor: "vm",
			registry: makeRegistry(),
		}).createSessionEnv({ id: "ticket-2" });

		await expect(env.readFile("note.txt")).resolves.toBe("hello");
		await expect(env.stat("note.txt")).resolves.toEqual({
			isFile: true,
			isDirectory: false,
			isSymbolicLink: false,
			size: 5,
			mtime: new Date(1234),
		});
		await expect(env.exec("printf ok", { timeoutMs: 500 })).resolves.toEqual({
			exitCode: 0,
			stdout: "ok",
			stderr: "",
		});
		expect(harness.connection.exec).toHaveBeenCalledWith("printf ok", {
			cwd: "/workspace",
			env: undefined,
			timeout: 500,
			captureStdio: true,
		});
	});

	it("implements force removal without hiding other failures", async () => {
		const harness = makeHarness();
		const env = await agentOSSandbox({
			actor: "vm",
			registry: makeRegistry(),
		}).createSessionEnv({ id: "ticket-3" });

		harness.connection.exists.mockResolvedValueOnce(false);
		await expect(env.rm("missing", { force: true })).resolves.toBeUndefined();
		expect(harness.connection.remove).not.toHaveBeenCalled();

		harness.connection.exists.mockResolvedValue(true);
		harness.connection.remove.mockRejectedValueOnce(
			new Error("permission denied"),
		);
		await expect(env.rm("protected", { force: true })).rejects.toThrow(
			"permission denied",
		);
	});

	it("reports a missing actor as a configuration error", async () => {
		clientMocks.createClient.mockReturnValue({});
		const sandbox = agentOSSandbox({ actor: "vm", registry: makeRegistry() });
		await expect(
			sandbox.createSessionEnv({ id: "ticket-4" }),
		).rejects.toBeInstanceOf(AgentOSFlueConfigurationError);
	});
});
