import { type ChildProcess, spawn } from "node:child_process";
import { once } from "node:events";
import {
	cpSync,
	existsSync,
	mkdirSync,
	mkdtempSync,
	rmSync,
	symlinkSync,
	writeFileSync,
} from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import {
	ACTOR_E2E_NAMESPACE,
	ACTOR_E2E_TOKEN,
	type ActorRuntimeHandle,
	ensureActorE2ESidecarBinary,
	startActorRuntime,
} from "../../agentos/tests/helpers/actor-runtime.js";

const RUN_E2E = process.env.AGENTOS_ACTOR_E2E === "1";
const packageRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const workspaceRoot = resolve(packageRoot, "../..");
const exampleRoot = join(workspaceRoot, "examples/flue");
const rivetFlueRoot = process.env.AGENTOS_FLUE_RIVET_PACKAGE;
const flueForkRoot = process.env.AGENTOS_FLUE_FORK_ROOT;
const engineBinary = process.env.RIVET_ENGINE_BINARY;
const MAX_LOG_BYTES = 1024 * 1024;

describe.skipIf(!RUN_E2E)("Flue + Rivet + agentOS real actor E2E", () => {
	let runtime: ActorRuntimeHandle;
	let storagePath: string;
	let fixtureRoot: string;
	let sidecarBinary: string;
	let server: RunningServer | undefined;

	beforeAll(async () => {
		if (!rivetFlueRoot || !flueForkRoot || !engineBinary) {
			throw new Error(
				"Set AGENTOS_FLUE_RIVET_PACKAGE, AGENTOS_FLUE_FORK_ROOT, and RIVET_ENGINE_BINARY to matching local Rivet and Flue builds.",
			);
		}
		if (!existsSync(engineBinary)) {
			throw new Error(`RIVET_ENGINE_BINARY does not exist: ${engineBinary}`);
		}
		sidecarBinary = ensureActorE2ESidecarBinary();
		storagePath = mkdtempSync(join(tmpdir(), "agentos-flue-combined-e2e-"));
		fixtureRoot = join(storagePath, "example");
		createFixture(fixtureRoot, rivetFlueRoot, flueForkRoot);
		await run(
			process.execPath,
			[join(flueForkRoot, "packages/cli/bin/flue.mjs"), "build"],
			fixtureRoot,
		);
		runtime = await startActorRuntime(storagePath, { engineOnly: true });
	}, 180_000);

	afterAll(async () => {
		await server?.stop();
		await runtime?.stop();
		if (storagePath) rmSync(storagePath, { recursive: true, force: true });
	}, 30_000);

	it("persists filesystem and exec output after reconnecting through the native Flue router", async () => {
		const poolName = `agentos-flue-${Date.now()}`;
		await configurePool(runtime.endpoint, poolName);
		const port = await getFreePort();
		const instanceId = `combined-${Date.now()}`;

		server = startServer(
			fixtureRoot,
			runtime.endpoint,
			poolName,
			port,
			sidecarBinary,
		);
		await waitForServer(port, server);
		await postPrompt(port, instanceId, "write the persistent file", server);
		await waitForHistory(
			port,
			instanceId,
			["persisted-through-agentos", "The persistent file was written."],
			server,
		);

		// Drop every Flue-side connection and registry resource. Starting the
		// generated server again must reconnect the same Flue context to the
		// same agentOS actor and its actor-owned filesystem.
		await server.stop();
		server = startServer(
			fixtureRoot,
			runtime.endpoint,
			poolName,
			port,
			sidecarBinary,
		);
		await waitForServer(port, server);
		const restoredHistory = await waitForHistory(
			port,
			instanceId,
			["persisted-through-agentos", "The persistent file was written."],
			server,
		);
		const sseAbort = new AbortController();
		const sse = await openConversationSse(
			`http://127.0.0.1:${port}/agents/assistant/${instanceId}?view=updates&offset=${encodeURIComponent(restoredHistory.offset)}&live=sse`,
			sseAbort.signal,
			server,
		);
		const livePrompt = "read the persistent file";
		const finalText = "The persistent file survived reconnecting.";
		try {
			await waitForSseUpToDate(sse, server);
			await postPrompt(port, instanceId, livePrompt, server);
			const liveItems = await readLiveTurn(sse, finalText, server);
			expectLiveTurnOrdering(liveItems, livePrompt, finalText);
		} finally {
			sseAbort.abort();
			await sse.reader.cancel().catch(() => {});
		}
		const history = await waitForHistory(
			port,
			instanceId,
			["persisted-through-agentos", finalText],
			server,
		);

		expect(JSON.stringify(history)).toContain("cat persisted.txt");
	}, 180_000);
});

function createFixture(
	root: string,
	localRivetFlueRoot: string,
	localFlueForkRoot: string,
): void {
	cpSync(exampleRoot, root, {
		recursive: true,
		filter: (source) =>
			!["dist", ".turbo", "node_modules"].includes(
				source.split("/").at(-1) ?? "",
			),
	});
	writeFileSync(
		join(root, "tsconfig.json"),
		JSON.stringify({
			compilerOptions: {
				lib: ["ESNext", "DOM"],
				module: "ESNext",
				moduleResolution: "Bundler",
				strict: true,
				target: "ES2023",
				types: ["node"],
			},
			include: ["**/*.ts"],
		}),
	);
	writeDeterministicAgent(root);
	const modules = join(root, "node_modules");
	mkdirSync(join(modules, "@flue"), { recursive: true });
	mkdirSync(join(modules, "@rivet-dev"), { recursive: true });

	link(join(localFlueForkRoot, "packages/cli"), join(modules, "@flue/cli"));
	link(
		join(localFlueForkRoot, "packages/runtime"),
		join(modules, "@flue/runtime"),
	);
	copyPublishedPackage(localRivetFlueRoot, join(modules, "@rivet-dev/flue"));
	copyPublishedPackage(packageRoot, join(modules, "@rivet-dev/agentos-flue"));
	copyPublishedPackage(
		join(workspaceRoot, "packages/agentos"),
		join(modules, "@rivet-dev/agentos"),
	);
	link(
		join(workspaceRoot, "packages/core"),
		join(modules, "@rivet-dev/agentos-core"),
	);
	link(
		join(localRivetFlueRoot, "node_modules/rivetkit"),
		join(modules, "rivetkit"),
	);
}

function writeDeterministicAgent(root: string): void {
	writeFileSync(
		join(root, "agents/assistant.ts"),
		`import { createAgent, registerProvider } from "@flue/runtime";
import { fauxAssistantMessage, registerFauxProvider } from "@flue/runtime/adapter-kit";
import { agentOSSandbox } from "@rivet-dev/agentos-flue";
import { registry } from "../actors.js";

// This fixture tests Flue's authored public HTTP/SSE route. The route-free
// example uses the Flue run command, which enables temporary local exposure.
export const route = async (_context, next) => next();

const provider = registerFauxProvider({ provider: "agentos-flue-e2e" });
const respond = (context) => {
    const latestUser = context.messages.findLast((message) => message.role === "user");
    const userText = latestUser?.role === "user"
      ? typeof latestUser.content === "string"
        ? latestUser.content
        : latestUser.content.map((block) => block.type === "text" ? block.text : "").join("")
      : "";
    if (context.messages.at(-1)?.role !== "user") {
      return fauxAssistantMessage(userText.includes("read")
        ? "The persistent file survived reconnecting."
        : "The persistent file was written.");
    }
    const reading = userText.includes("read");
    return fauxAssistantMessage({
      type: "toolCall",
      id: reading ? "read-persistent-file" : "write-persistent-file",
      name: "bash",
      arguments: {
        command: reading
          ? "cat persisted.txt"
          : "printf 'persisted-through-agentos' > persisted.txt && cat persisted.txt",
      },
    }, { stopReason: "toolUse" });
};
provider.setResponses(Array.from({ length: 8 }, () => respond));
const model = provider.getModel();
registerProvider(model.provider, { api: provider.api, baseUrl: model.baseUrl });

export default createAgent(() => ({
  model: model.provider + "/" + model.id,
  sandbox: agentOSSandbox({ actor: "vm", registry }),
}));
`,
	);
}

function copyPublishedPackage(source: string, target: string): void {
	if (!existsSync(join(source, "dist"))) {
		throw new Error(`Build local package before E2E: ${source}`);
	}
	mkdirSync(target, { recursive: true });
	cpSync(join(source, "dist"), join(target, "dist"), { recursive: true });
	cpSync(join(source, "package.json"), join(target, "package.json"));
}

function link(source: string, target: string): void {
	if (!existsSync(source)) throw new Error(`Missing local package: ${source}`);
	symlinkSync(source, target, "dir");
}

async function configurePool(
	endpoint: string,
	poolName: string,
): Promise<void> {
	const headers = { Authorization: `Bearer ${ACTOR_E2E_TOKEN}` };
	const datacentersResponse = await fetch(
		`${endpoint}/datacenters?namespace=${ACTOR_E2E_NAMESPACE}`,
		{ headers },
	);
	if (!datacentersResponse.ok) {
		throw new Error(
			`failed to list E2E datacenters: ${datacentersResponse.status}`,
		);
	}
	const datacenter = (
		(await datacentersResponse.json()) as {
			datacenters: Array<{ name: string }>;
		}
	).datacenters[0]?.name;
	if (!datacenter) throw new Error("E2E Engine returned no datacenter");
	const response = await fetch(
		`${endpoint}/runner-configs/${poolName}?namespace=${ACTOR_E2E_NAMESPACE}`,
		{
			method: "PUT",
			headers: { ...headers, "content-type": "application/json" },
			body: JSON.stringify({ datacenters: { [datacenter]: { normal: {} } } }),
		},
	);
	if (!response.ok) {
		throw new Error(`failed to configure E2E pool: ${await response.text()}`);
	}
}

interface RunningServer {
	child: ChildProcess;
	logs(): string;
	stop(): Promise<void>;
}

interface ConversationHistory {
	offset: string;
	[key: string]: unknown;
}

interface ConversationSse {
	reader: ReadableStreamDefaultReader<Uint8Array>;
	state: {
		buffer: string;
		decoder: TextDecoder;
	};
}

interface FlueSseEvent {
	event: string;
	data: unknown;
}

function startServer(
	root: string,
	endpoint: string,
	poolName: string,
	port: number,
	sidecarBinary: string,
): RunningServer {
	const child = spawn(process.execPath, [join(root, "dist/server.mjs")], {
		cwd: root,
		env: {
			...process.env,
			AGENTOS_SIDECAR_BIN: sidecarBinary,
			FLUE_MODE: "local",
			PORT: String(port),
			RIVET_ENDPOINT: endpoint,
			RIVET_NAMESPACE: ACTOR_E2E_NAMESPACE,
			RIVET_POOL: poolName,
			RIVET_TOKEN: ACTOR_E2E_TOKEN,
			RIVETKIT_ENGINE_SPAWN: "never",
		},
		stdio: ["ignore", "pipe", "pipe"],
	});
	let output = "";
	for (const stream of [child.stdout, child.stderr]) {
		stream?.setEncoding("utf8");
		stream?.on("data", (chunk: string) => {
			output = (output + chunk).slice(-MAX_LOG_BYTES);
		});
	}
	return {
		child,
		logs: () => output,
		async stop() {
			if (child.exitCode !== null || child.signalCode !== null) return;
			child.kill("SIGTERM");
			await Promise.race([
				once(child, "exit"),
				new Promise((resolveStop) => setTimeout(resolveStop, 10_000)),
			]);
			if (child.exitCode === null && child.signalCode === null) {
				child.kill("SIGKILL");
				await once(child, "exit");
			}
		},
	};
}

async function waitForServer(
	port: number,
	server: RunningServer,
): Promise<void> {
	await retry(async () => {
		if (server.child.exitCode !== null) {
			throw new Error(`Flue server exited\n${server.logs()}`);
		}
		const response = await fetch(`http://127.0.0.1:${port}/`);
		return response.status < 500;
	}, server);
}

async function postPrompt(
	port: number,
	instanceId: string,
	body: string,
	server: RunningServer,
): Promise<void> {
	await retry(async () => {
		const response = await fetch(
			`http://127.0.0.1:${port}/agents/assistant/${instanceId}`,
			{
				method: "POST",
				headers: { "content-type": "application/json" },
				body: JSON.stringify({ kind: "user", body }),
			},
		);
		if (response.status === 202) return true;
		if (response.status < 500) {
			throw new Error(
				`prompt failed: ${response.status} ${await response.text()}`,
			);
		}
		return false;
	}, server);
}

async function waitForHistory(
	port: number,
	instanceId: string,
	expected: string[],
	server: RunningServer,
): Promise<ConversationHistory> {
	let history: ConversationHistory | undefined;
	await retry(
		async () => {
			const response = await fetch(
				`http://127.0.0.1:${port}/agents/assistant/${instanceId}?view=history`,
			);
			if (!response.ok) return false;
			const candidate = (await response.json()) as {
				offset?: unknown;
				[key: string]: unknown;
			};
			if (typeof candidate.offset !== "string") {
				throw new Error(
					`history response omitted its offset: ${JSON.stringify(candidate)}`,
				);
			}
			history = candidate as ConversationHistory;
			const text = JSON.stringify(history);
			return expected.every((value) => text.includes(value));
		},
		server,
		120,
	);
	if (!history) throw new Error(`history was not loaded\n${server.logs()}`);
	return history;
}

async function openConversationSse(
	url: string,
	signal: AbortSignal,
	server: RunningServer,
): Promise<ConversationSse> {
	const connectAbort = new AbortController();
	const timeout = setTimeout(
		() => connectAbort.abort(new Error("Flue SSE connection timed out")),
		10_000,
	);
	let response: Response;
	try {
		response = await fetch(url, {
			signal: AbortSignal.any([signal, connectAbort.signal]),
		});
	} finally {
		clearTimeout(timeout);
	}
	if (!response.ok) {
		throw new Error(
			`Flue SSE failed: ${response.status} ${await response.text()}\n${server.logs()}`,
		);
	}
	if (!response.headers.get("content-type")?.startsWith("text/event-stream")) {
		throw new Error(
			`Flue SSE returned ${response.headers.get("content-type")}\n${server.logs()}`,
		);
	}
	if (!response.body) {
		throw new Error(`Flue SSE response omitted its body\n${server.logs()}`);
	}
	return {
		reader: response.body.getReader(),
		state: { buffer: "", decoder: new TextDecoder() },
	};
}

async function waitForSseUpToDate(
	sse: ConversationSse,
	server: RunningServer,
): Promise<void> {
	while (true) {
		const event = await readSseEvent(sse, server);
		if (
			event.event === "control" &&
			isRecord(event.data) &&
			event.data.upToDate === true
		) {
			return;
		}
	}
}

async function readLiveTurn(
	sse: ConversationSse,
	finalText: string,
	server: RunningServer,
): Promise<Record<string, unknown>[]> {
	const items: Record<string, unknown>[] = [];
	while (true) {
		const event = await readSseEvent(sse, server);
		if (event.event !== "data") continue;
		if (!Array.isArray(event.data) || !event.data.every(isRecord)) {
			throw new Error(
				`Flue SSE data event was not an object array: ${JSON.stringify(event.data)}`,
			);
		}
		items.push(...event.data);
		const hasFinal = finalTextIndex(items, finalText) >= 0;
		const hasSettlement = items.some(
			(item) =>
				item.type === "submission-settled" && item.outcome === "completed",
		);
		if (hasFinal && hasSettlement) return items;
	}
}

function expectLiveTurnOrdering(
	items: Record<string, unknown>[],
	userText: string,
	finalText: string,
): void {
	const serialized = items.map((item) => JSON.stringify(item));
	const userIndex = items.findIndex(
		(item, index) =>
			item.type === "message-appended" &&
			serialized[index]?.includes(`"role":"user"`) &&
			serialized[index]?.includes(userText),
	);
	const toolInputIndex = items.findIndex(
		(item, index) =>
			item.type === "tool-input" &&
			serialized[index]?.includes("cat persisted.txt"),
	);
	const toolOutputIndex = items.findIndex(
		(item, index) =>
			item.type === "tool-output" &&
			serialized[index]?.includes("persisted-through-agentos"),
	);
	const finalIndex = finalTextIndex(items, finalText);
	const detail = JSON.stringify(items, null, 2);
	expect(userIndex, detail).toBeGreaterThanOrEqual(0);
	expect(toolInputIndex, detail).toBeGreaterThan(userIndex);
	expect(toolOutputIndex, detail).toBeGreaterThan(toolInputIndex);
	expect(finalIndex, detail).toBeGreaterThan(toolOutputIndex);
}

function finalTextIndex(
	items: Record<string, unknown>[],
	finalText: string,
): number {
	let text = "";
	for (const [index, item] of items.entries()) {
		if (item.type !== "message-delta" || typeof item.delta !== "string") {
			continue;
		}
		text += item.delta;
		if (text.includes(finalText)) return index;
	}
	return -1;
}

async function readSseEvent(
	sse: ConversationSse,
	server: RunningServer,
): Promise<FlueSseEvent> {
	const deadline = Date.now() + 30_000;
	while (Date.now() < deadline) {
		const boundary = sse.state.buffer.indexOf("\n\n");
		if (boundary !== -1) {
			const raw = sse.state.buffer.slice(0, boundary);
			sse.state.buffer = sse.state.buffer.slice(boundary + 2);
			if (raw.startsWith(":")) continue;
			const event = raw.match(/^event:\s*(.+)$/m)?.[1];
			const data = raw
				.split("\n")
				.filter((line) => line.startsWith("data:"))
				.map((line) => line.slice("data:".length).trimStart())
				.join("\n");
			if (event && data) {
				return { event, data: JSON.parse(data) as unknown };
			}
			continue;
		}
		const result = await readWithTimeout(
			sse.reader,
			deadline - Date.now(),
			server,
		);
		if (result.done) {
			throw new Error(`Flue SSE ended before the live turn\n${server.logs()}`);
		}
		sse.state.buffer += sse.state.decoder
			.decode(result.value, { stream: true })
			.replaceAll("\r\n", "\n");
	}
	throw new Error(`Timed out reading Flue SSE event\n${server.logs()}`);
}

async function readWithTimeout(
	reader: ReadableStreamDefaultReader<Uint8Array>,
	timeoutMs: number,
	server: RunningServer,
) {
	let timeout: ReturnType<typeof setTimeout> | undefined;
	try {
		return await Promise.race([
			reader.read(),
			new Promise<never>((_, reject) => {
				timeout = setTimeout(
					() =>
						reject(
							new Error(`Timed out reading Flue SSE event\n${server.logs()}`),
						),
					timeoutMs,
				);
			}),
		]);
	} finally {
		clearTimeout(timeout);
	}
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === "object" && value !== null && !Array.isArray(value);
}

async function retry(
	runAttempt: () => Promise<boolean>,
	server: RunningServer,
	attempts = 60,
): Promise<void> {
	let lastError: unknown;
	for (let attempt = 0; attempt < attempts; attempt++) {
		try {
			if (await runAttempt()) return;
		} catch (error) {
			lastError = error;
		}
		await new Promise((resolveDelay) => setTimeout(resolveDelay, 500));
	}
	throw new Error(
		`E2E condition timed out: ${String(lastError ?? "not ready")}\n${server.logs()}`,
	);
}

async function run(
	command: string,
	args: string[],
	cwd: string,
): Promise<void> {
	const child = spawn(command, args, {
		cwd,
		stdio: ["ignore", "pipe", "pipe"],
	});
	let output = "";
	for (const stream of [child.stdout, child.stderr]) {
		stream.setEncoding("utf8");
		stream.on("data", (chunk: string) => {
			output += chunk;
		});
	}
	const [code] = (await once(child, "exit")) as [number | null];
	if (code !== 0) throw new Error(`${command} failed (${code})\n${output}`);
}

async function getFreePort(): Promise<number> {
	const server = createServer();
	server.listen(0, "127.0.0.1");
	await once(server, "listening");
	const address = server.address();
	if (!address || typeof address === "string") {
		throw new Error("failed to allocate E2E port");
	}
	server.close();
	await once(server, "close");
	return address.port;
}
