import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { CONFORMANCE_AGENT_NAME } from "@rivet-dev/agentos-test-harness/agent-os-conformance-fixture";
import { describe, expect, test } from "vitest";
import {
	actorBytes,
	actorHandle,
	createActorHandle,
	startActorRuntime,
} from "./helpers/actor-runtime.js";

const RUN_E2E = process.env.AGENTOS_ACTOR_E2E === "1";

async function eventually<T>(
	read: () => T | Promise<T>,
	accept: (value: T) => boolean,
	timeoutMs = 15_000,
): Promise<T> {
	const deadline = Date.now() + timeoutMs;
	let value = await read();
	while (!accept(value) && Date.now() < deadline) {
		await new Promise((resolve) => setTimeout(resolve, 50));
		value = await read();
	}
	if (!accept(value)) throw new Error("condition did not become true");
	return value;
}

describe.skipIf(!RUN_E2E)("agentOS real Rivet actor", () => {
	test("enforces onBeforeConnect and emits live VM lifecycle events", async () => {
		const storagePath = mkdtempSync(join(tmpdir(), "agentos-actor-hooks-e2e-"));
		const runtime = await startActorRuntime(storagePath);
		try {
			const actorKey = `hooks-${Date.now()}`;
			const rejected = actorHandle(runtime.endpoint, actorKey, {
				authToken: "rejected",
			});
			await expect(rejected.echo("not-authorized")).rejects.toThrow();
			expect((await rejected.fetch("/fetch/not-a-preview-token")).status).toBe(
				500,
			);

			const handle = actorHandle(runtime.endpoint, actorKey);
			const connection = handle.connect();
			const booted: unknown[] = [];
			const shutdown: Array<{ reason: string }> = [];
			connection.on("vmBooted", (event: unknown) => booted.push(event));
			connection.on("vmShutdown", (event: { reason: string }) =>
				shutdown.push(event),
			);
			await connection.ready;
			expect(await connection.echo("authorized")).toBe("authorized");
			expect(await connection.getBeforeConnectCount()).toBeGreaterThanOrEqual(
				2,
			);

			expect(await connection.exists("/")).toBe(true);
			await eventually(
				() => booted,
				(events) => events.length === 1,
			);
			await connection.sleepActor();
			await eventually(
				() => shutdown,
				(events) => events.some((event) => event.reason === "sleep"),
			);
			expect(await connection.exists("/")).toBe(true);
			await eventually(
				() => booted,
				(events) => events.length === 2,
			);
			await connection.dispose();
		} finally {
			await runtime.stop();
			rmSync(storagePath, { recursive: true, force: true });
		}
	}, 180_000);

	test("proxies, revokes, expires, and bounds signed preview URLs", async () => {
		const storagePath = mkdtempSync(join(tmpdir(), "agentos-preview-e2e-"));
		const runtime = await startActorRuntime(storagePath);
		try {
			const actorKey = `preview-${Date.now()}`;
			const handle = actorHandle(runtime.endpoint, actorKey);
			const connection = handle.connect();
			const output: string[] = [];
			connection.on("processOutput", (event: { data: unknown }) =>
				output.push(new TextDecoder().decode(actorBytes(event.data))),
			);
			await connection.ready;
			const port = 31_338;
			const server = await connection.spawn("node", [
				"-e",
				`const http = require("http"); http.createServer((req, res) => { let body = ""; req.on("data", chunk => body += chunk); req.on("end", () => { res.setHeader("content-type", "application/json"); res.end(JSON.stringify({ method: req.method, url: req.url, body, marker: req.headers["x-preview-marker"] })); }); }).listen(${port}, "0.0.0.0", () => console.log("preview-ready")); setInterval(() => {}, 1000);`,
			]);
			await eventually(
				() => output,
				(lines) => lines.some((line) => line.includes("preview-ready")),
			);

			const preview = await connection.createPreviewUrl(port, 60);
			const unauthenticated = actorHandle(runtime.endpoint, actorKey, {
				authToken: "rejected",
			});
			const response = await unauthenticated.fetch(
				`${preview.path}/nested?q=1`,
				{
					method: "POST",
					headers: { "x-preview-marker": "yes" },
					body: "preview-body",
				},
			);
			const responseBody = await response.text();
			expect(
				response.status,
				JSON.stringify({
					body: responseBody,
					headers: Object.fromEntries(response.headers),
				}),
			).toBe(200);
			expect(response.headers.get("access-control-allow-origin")).toBe("*");
			expect(JSON.parse(responseBody)).toEqual({
				method: "POST",
				url: "/nested?q=1",
				body: "preview-body",
				marker: "yes",
			});

			await connection.expirePreviewUrl(preview.token);
			expect((await unauthenticated.fetch(preview.path)).status).toBe(403);

			const short = await connection.createPreviewUrl(port, 0.05);
			await new Promise((resolve) =>
				setTimeout(resolve, Math.max(1, short.expiresAt - Date.now() + 25)),
			);
			expect((await unauthenticated.fetch(short.path)).status).toBe(403);

			const active: Array<{ token: string }> = [];
			for (let index = 0; index < 8; index += 1) {
				active.push(await connection.createPreviewUrl(port, 60));
			}
			await expect(connection.createPreviewUrl(port, 60)).rejects.toMatchObject(
				{
					code: "agentos_preview_token_limit",
					message:
						"preview token limit 8 reached; raise preview.maxActiveTokens to allow more",
				},
			);
			await connection.expirePreviewUrl(active[0].token);
			const replacement = await connection.createPreviewUrl(port, 60);
			active.push(replacement);
			await Promise.all(
				active
					.slice(1)
					.map((token) => connection.expirePreviewUrl(token.token)),
			);
			await connection.killProcess(server.pid);
			await connection.waitProcess(server.pid);
			await connection.dispose();
		} finally {
			await runtime.stop();
			rmSync(storagePath, { recursive: true, force: true });
		}
	}, 180_000);

	test("persists direct-UDS filesystem chunks across sleep and engine restart", async () => {
		const storagePath = mkdtempSync(join(tmpdir(), "agentos-actor-e2e-"));
		const actorKey = `persistence-${Date.now()}`;
		let runtime: Awaited<ReturnType<typeof startActorRuntime>> | undefined;
		try {
			runtime = await startActorRuntime(storagePath);
			let handle = await createActorHandle(runtime.endpoint, actorKey, {
				workspace: "actor-input",
			});

			expect(await handle.echo("custom-action-ok")).toBe("custom-action-ok");
			expect(await handle.getCreationInput()).toEqual({
				workspace: "actor-input",
			});
			expect(await handle.getCreationInputs()).toEqual({
				createState: { workspace: "actor-input" },
				onCreate: { workspace: "actor-input" },
			});
			expect(await handle.getWakeCount()).toBe(1);
			await handle.mkdir("/persist");
			await handle.writeFile("/persist/message.txt", "survives sleep");
			const large = new Uint8Array(2 * 1024 * 1024 + 17);
			for (let index = 0; index < large.length; index += 1) {
				large[index] = index % 251;
			}
			await handle.writeFile("/persist/chunked.bin", large);

			const storage = await handle.inspectAgentOsStorage();
			expect(storage.tables).toEqual([
				"agentos_fs_blocks",
				"agentos_fs_metadata_chunks",
				"agentos_fs_metadata_heads",
				"agentos_fs_schema_version",
			]);
			expect(storage.metadataCount).toBe(1);
			expect(storage.metadataChunkCount).toBeGreaterThan(0);
			expect(storage.metadataChunkBytes).toBeGreaterThan(0);
			expect(storage.blockCount).toBeGreaterThan(0);
			expect(storage.blockBytes).toBeGreaterThan(0);

			await handle.sleepActor();
			await new Promise((resolveDelay) => setTimeout(resolveDelay, 1_000));
			expect(await handle.getWakeCount()).toBe(2);
			expect(
				new TextDecoder().decode(
					actorBytes(await handle.readFile("/persist/message.txt")),
				),
			).toBe("survives sleep");
			expect(actorBytes(await handle.readFile("/persist/chunked.bin"))).toEqual(
				large,
			);

			const restartPort = Number(new URL(runtime.endpoint).port);
			await runtime.stop();
			runtime = await startActorRuntime(storagePath, restartPort);
			handle = actorHandle(runtime.endpoint, actorKey);
			expect(await handle.getCreationInput()).toEqual({
				workspace: "actor-input",
			});
			expect(
				new TextDecoder().decode(
					actorBytes(await handle.readFile("/persist/message.txt")),
				),
			).toBe("survives sleep");
			expect(actorBytes(await handle.readFile("/persist/chunked.bin"))).toEqual(
				large,
			);
			expect(await handle.getWakeCount()).toBe(3);
		} finally {
			await runtime?.stop();
			rmSync(storagePath, { recursive: true, force: true });
		}
	}, 180_000);

	test("persists durable sessions and history and restores ACP across RivetKit sleep with default persistence", async () => {
		const storagePath = mkdtempSync(
			join(tmpdir(), "agentos-session-sleep-e2e-"),
		);
		const tracePath = join(storagePath, "acp-trace.jsonl");
		const previousTracePath = process.env.AGENT_OS_ACP_TRACE_PATH;
		process.env.AGENT_OS_ACP_TRACE_PATH = tracePath;
		let runtime: Awaited<ReturnType<typeof startActorRuntime>> | undefined;
		try {
			runtime = await startActorRuntime(storagePath);
			const actorKey = `session-sleep-${Date.now()}`;
			const handle = actorHandle(runtime.endpoint, actorKey);
			const connection = handle.connect();
			const shutdown: Array<{ reason: string }> = [];
			connection.on("vmShutdown", (event: { reason: string }) =>
				shutdown.push(event),
			);
			await connection.ready;

			const sessionId = "sleep-persistence";
			await connection.openSession({
				sessionId,
				agent: CONFORMANCE_AGENT_NAME,
				skipOsInstructions: true,
			});
			const firstPrompt = await connection.prompt({
				sessionId,
				content: [{ type: "text", text: "before-sleep" }],
			});
			expect(JSON.stringify(firstPrompt.message)).toContain(
				"echo:before-sleep",
			);

			const beforeList = await connection.listSessions();
			const beforeSession = beforeList.sessions.find(
				(session: { sessionId: string }) => session.sessionId === sessionId,
			);
			if (!beforeSession)
				throw new Error("opened session missing from catalog");
			expect(beforeSession).toMatchObject({
				sessionId,
				agent: CONFORMANCE_AGENT_NAME,
			});
			const beforeHistory = await connection.readHistory({ sessionId });
			expect(beforeHistory.events).toEqual(
				expect.arrayContaining([
					expect.objectContaining({
						sessionId,
						type: "agent_message_chunk",
						content: expect.objectContaining({ text: "echo:before-sleep" }),
					}),
				]),
			);

			await connection.sleepActor();
			await eventually(
				() => shutdown,
				(events) => events.some((event) => event.reason === "sleep"),
			);

			// The first storage-only call wakes the actor and must recover the same
			// catalog without starting an ACP adapter.
			const afterList = await connection.listSessions();
			expect(await connection.getWakeCount()).toBe(2);
			expect(existsSync(tracePath)).toBe(false);
			const storageOnlyProcesses = await connection.allProcesses();
			expect(
				storageOnlyProcesses.some((process: unknown) =>
					JSON.stringify(process).includes("conformance-agent-acp"),
				),
			).toBe(false);
			const afterSession = afterList.sessions.find(
				(session: { sessionId: string }) => session.sessionId === sessionId,
			);
			if (!afterSession) throw new Error("session missing after actor wake");
			expect(afterSession).toMatchObject({
				sessionId,
				agent: CONFORMANCE_AGENT_NAME,
				createdAt: beforeSession.createdAt,
				latestSequence: beforeSession.latestSequence,
			});
			const afterHistory = await connection.readHistory({ sessionId });
			expect(afterHistory).toEqual(beforeHistory);

			// The adapter process was disposed by sleep. Prompting the durable public
			// ID must transparently restore its private ACP session through session/load.
			const restoredPrompt = await connection.prompt({
				sessionId,
				content: [{ type: "text", text: "after-sleep" }],
			});
			expect(JSON.stringify(restoredPrompt.message)).toContain(
				"echo:after-sleep",
			);
			const trace = readFileSync(tracePath, "utf8")
				.trim()
				.split("\n")
				.filter(Boolean)
				.map((line) => JSON.parse(line));
			expect(trace).toEqual(
				expect.arrayContaining([
					expect.objectContaining({
						method: "session/load",
						response: expect.objectContaining({
							result: expect.objectContaining({
								sessionId: expect.any(String),
							}),
						}),
					}),
				]),
			);
			const restoredHistory = await connection.readHistory({ sessionId });
			expect(restoredHistory.events.length).toBeGreaterThan(
				beforeHistory.events.length,
			);
			expect(JSON.stringify(restoredHistory.events)).toContain(
				"echo:before-sleep",
			);
			expect(JSON.stringify(restoredHistory.events)).toContain(
				"echo:after-sleep",
			);

			await connection.deleteSession({ sessionId });
			await connection.dispose();
		} finally {
			await runtime?.stop();
			if (previousTracePath === undefined) {
				delete process.env.AGENT_OS_ACP_TRACE_PATH;
			} else {
				process.env.AGENT_OS_ACP_TRACE_PATH = previousTracePath;
			}
			rmSync(storagePath, { recursive: true, force: true });
		}
	}, 180_000);
});
