// Nightly: projects the complete registry command bundle.
import { resolve } from "node:path";
import codex from "@agentos-software/codex";
import { afterEach, describe, expect, test } from "vitest";
import { AgentOs } from "../src/agent-os.js";
import { moduleAccessMounts } from "./helpers/node-modules-mount.js";
import { REGISTRY_SOFTWARE } from "./helpers/registry-commands.js";

const MODULE_ACCESS_CWD = resolve(import.meta.dirname, "..");

describe("Codex agent availability", () => {
	const cleanups = new Set<() => Promise<void>>();

	afterEach(async () => {
		for (const stop of cleanups) {
			await stop();
		}
		cleanups.clear();
	});

	test("codex package registers a runnable ACP agent", async () => {
		const vm = await AgentOs.create({
			mounts: moduleAccessMounts(MODULE_ACCESS_CWD),
			software: [
				codex,
				...REGISTRY_SOFTWARE.filter(
					(pkg) => !pkg.packagePath.includes("/software/codex-cli/"),
				),
			],
		});
		cleanups.add(async () => {
			await vm.dispose();
		});

		expect(await vm.listAgents()).toEqual(
			expect.arrayContaining([
				expect.objectContaining({ id: "codex", installed: true }),
			]),
		);
		let stdout = "";
		let stderr = "";
		const { pid } = vm.spawn("codex-acp", [], {
			onStdout: (data: Uint8Array) => {
				stdout += new TextDecoder().decode(data);
			},
			onStderr: (data: Uint8Array) => {
				stderr += new TextDecoder().decode(data);
			},
		});
		const waitForResponse = async (id: number) => {
			const deadline = Date.now() + 10_000;
			while (Date.now() < deadline) {
				for (const line of stdout.split("\n")) {
					if (!line.trim()) continue;
					const response = JSON.parse(line) as {
						id?: number;
						result?: Record<string, unknown>;
						error?: { code: number; message: string };
					};
					if (response.id === id) return response;
				}
				await new Promise((resolve) => setTimeout(resolve, 10));
			}
			throw new Error(
				`ACP response ${id} timed out.\nstdout:\n${stdout}\nstderr:\n${stderr}`,
			);
		};
		const send = async (message: Record<string, unknown>) => {
			await vm.writeProcessStdin(pid, `${JSON.stringify(message)}\n`);
		};
		await send({
			jsonrpc: "2.0",
			id: 1,
			method: "initialize",
			params: { protocolVersion: 1, clientCapabilities: {} },
		});
		const initialized = await waitForResponse(1);
		expect(initialized.error).toBeUndefined();
		expect(initialized.result?.agentCapabilities).toMatchObject({
			sessionCapabilities: { close: {} },
		});
		await send({
			jsonrpc: "2.0",
			id: 2,
			method: "session/new",
			params: { cwd: "/workspace", mcpServers: [] },
		});
		const created = await waitForResponse(2);
		expect(created.error).toBeUndefined();
		const directSessionId = (
			created.result as { sessionId?: string } | undefined
		)?.sessionId;
		expect(directSessionId).toEqual(expect.any(String));
		await send({
			jsonrpc: "2.0",
			id: 3,
			method: "session/close",
			params: { sessionId: directSessionId },
		});
		const closed = await waitForResponse(3);
		expect(closed.error, stderr).toBeUndefined();
		expect(closed.result).toEqual({});

		const sessionId = "codex-availability";
		await vm.openSession({ sessionId, agent: "codex" });
		await vm.unloadSession({ sessionId });
	});
});
