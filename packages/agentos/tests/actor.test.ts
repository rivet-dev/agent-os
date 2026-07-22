import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { AgentOs } from "@rivet-dev/agentos-core";
import {
	AGENT_OS_CONFORMANCE_ACTIONS,
	AGENT_OS_CONFORMANCE_EVENTS,
} from "@rivet-dev/agentos-test-harness/agent-os-conformance";
import { event } from "rivetkit";
import type { RawAccess } from "rivetkit/db";
import { describe, expect, test, vi } from "vitest";
import {
	migrateAgentOsActorTables,
	validateAgentOsActorMigrationLadder,
} from "../src/actor.js";
import { agentOS, createAgentOsActions } from "../src/index.js";

const { DatabaseSync } = createRequire(import.meta.url)(
	"node:sqlite",
) as typeof import("node:sqlite");

function createActorMigrationDatabase() {
	const sqlite = new DatabaseSync(":memory:");
	let access: RawAccess;
	const execute = vi.fn(
		async <TRow extends Record<string, unknown>>(
			query: string,
			...args: unknown[]
		): Promise<TRow[]> => {
			if (/^\s*SELECT\b/i.test(query)) {
				return sqlite.prepare(query).all(...(args as never[])) as TRow[];
			}
			if (args.length > 0) {
				sqlite.prepare(query).run(...(args as never[]));
			} else {
				sqlite.exec(query);
			}
			return [];
		},
	);
	access = {
		execute,
		transaction: async (callback) => {
			sqlite.exec("SAVEPOINT actor_test_transaction");
			try {
				const result = await callback(access);
				sqlite.exec("RELEASE SAVEPOINT actor_test_transaction");
				return result;
			} catch (error) {
				sqlite.exec("ROLLBACK TO SAVEPOINT actor_test_transaction");
				sqlite.exec("RELEASE SAVEPOINT actor_test_transaction");
				throw error;
			}
		},
		__withSqliteLease: async (_timeoutMs, callback) => callback(access),
		close: async () => sqlite.close(),
	};
	return { access, execute, sqlite };
}

describe("agentOS actor", () => {
	test("keeps the dedicated permission hook and event removed", () => {
		const sources = [
			readFileSync(new URL("../src/actor.ts", import.meta.url), "utf8"),
			readFileSync(new URL("../src/types.ts", import.meta.url), "utf8"),
			readFileSync(new URL("../src/client.ts", import.meta.url), "utf8"),
		];
		for (const removed of ["onPermissionRequest", "permissionRequest"]) {
			expect(sources.every((source) => !source.includes(removed))).toBe(true);
		}
	});

	test("is a normal actor with built-in and user-defined actions", () => {
		const definition = agentOS({
			createState: () => ({ count: 0 }),
			events: { countChanged: event<{ count: number }>() },
			actions: {
				increment: (c, amount: number) => {
					c.state.count += amount;
					return c.state.count;
				},
			},
		});

		expect(definition.config.actions).toHaveProperty("increment");
		expect(definition.config.actions).toHaveProperty("readFile");
		expect(definition.config.actions).toHaveProperty("openSession");
		expect(definition.config.actions).toHaveProperty("allProcesses");
		expect(definition.config.actions).toHaveProperty("filesystem.readFile");
		expect(definition.config.actions).toHaveProperty("process.spawn");
		expect(definition.config.actions).toHaveProperty("terminal.open");
		expect(definition.config.actions).toHaveProperty("typescript.execute");
		expect(definition.config.actions).toHaveProperty("sessions.open");
		for (const action of AGENT_OS_CONFORMANCE_ACTIONS) {
			expect(definition.config.actions).toHaveProperty(action);
		}
		expect(definition.config.actions).toHaveProperty("cancelPrompt");
		expect(definition.config.actions).toHaveProperty("deleteSession");
		expect(definition.config.actions).toHaveProperty("setSessionConfigOption");
		expect(definition.config.actions).toHaveProperty("listSessions");
		expect(definition.config.events).toHaveProperty("countChanged");
		expect(definition.config.events).toHaveProperty("vmBooted");
		expect(definition.config.events).toHaveProperty("sessionEvent");
	});

	test("accepts per-VM sandbox providers", () => {
		expect(() =>
			agentOS({
				sandbox: {
					provider: { start: async () => ({}) as never },
				},
			}),
		).not.toThrow();
	});

	test("rejects sandbox clients that would be shared across actor VMs", () => {
		expect(() =>
			agentOS({
				sandbox: { client: {} as never },
			}),
		).toThrow(/cannot share sandbox: \{ client \}/);
	});

	test("exposes nested execution actions without new flat aliases", () => {
		const actions = createAgentOsActions();
		expect(actions.process.exec).not.toBe(actions.exec);
		expect(actions.process.execFile).not.toBe(actions.execArgv);
		expect(actions.process.spawn).toBe(actions.spawn);
		expect(actions.allProcesses).toBeTypeOf("function");
		expect(actions.terminal.open).toBe(actions.openShell);
		expect(actions.filesystem.readFile).toBe(actions.readFile);
		expect(actions.sessions.open).toBe(actions.openSession);
		expect(actions.javascript.execute).toBeTypeOf("function");
		expect(actions.typescript.checkProject).toBeTypeOf("function");
		expect(actions.javascript.npm.install).toBeTypeOf("function");
		expect(actions.python.execute).toBeTypeOf("function");
		expect(actions.contexts.list).toBeTypeOf("function");
		expect(actions.process.readOutput).toBeTypeOf("function");
		for (const removedFlatAction of [
			"executeJavaScript",
			"evaluateJavaScript",
			"executeJavaScriptFile",
			"executeTypeScript",
			"evaluateTypeScript",
			"executeTypeScriptFile",
			"checkTypeScript",
			"checkTypeScriptProject",
			"installNpmPackages",
			"executeNpmScript",
			"executeNpmPackage",
			"executePython",
			"evaluatePython",
			"executePythonFile",
			"executePythonModule",
			"installPythonPackages",
			"getExecution",
			"listExecutions",
			"waitExecution",
			"cancelExecution",
			"signalExecution",
			"resetExecution",
			"deleteExecution",
			"writeExecutionStdin",
			"closeExecutionStdin",
			"resizeExecutionPty",
			"readExecutionOutput",
		]) {
			expect(actions).not.toHaveProperty(removedFlatAction);
		}
		const definition = agentOS();
		expect(Object.keys(definition.config.events ?? {}).sort()).toEqual(
			[...AGENT_OS_CONFORMANCE_EVENTS, "vmBooted", "vmShutdown"].sort(),
		);
	});

	test("validates the complete actor migration ladder before execution", () => {
		expect(() => validateAgentOsActorMigrationLadder([])).toThrow(
			"migration ladder must not be empty",
		);
		expect(() =>
			validateAgentOsActorMigrationLadder([
				{
					version: 2,
					sql: "CREATE TABLE agentos_actor_example(id INTEGER) STRICT",
				},
			]),
		).toThrow("expected version 1, received 2");
		expect(() =>
			validateAgentOsActorMigrationLadder([{ version: 1, sql: "  " }]),
		).toThrow("SQL is empty");
		expect(() =>
			validateAgentOsActorMigrationLadder([
				{
					version: 1,
					sql: "BEGIN; CREATE TABLE agentos_actor_example(id INTEGER) STRICT; COMMIT",
				},
			]),
		).toThrow("transaction control belongs to the migration provider");
		expect(() =>
			validateAgentOsActorMigrationLadder([
				{
					version: 1,
					sql: "CREATE TABLE agentos_actor_example(id INTEGER)",
				},
			]),
		).toThrow("every actor-owned table must be STRICT");
		expect(() =>
			validateAgentOsActorMigrationLadder([
				{
					version: 1,
					sql: "CREATE TABLE agentos_core_example(id INTEGER) STRICT",
				},
			]),
		).toThrow("only agentos_actor_* tables and indexes");
		expect(() =>
			validateAgentOsActorMigrationLadder([
				{
					version: 1,
					sql: "CREATE TABLE agentos_actor_child(parent_id INTEGER REFERENCES agentos_actor_parent(id)) STRICT",
				},
			]),
		).toThrow("foreign keys are not used");
	});

	test("migrates only strict actor-owned tables in the provider savepoint", async () => {
		const { access, execute, sqlite } = createActorMigrationDatabase();
		const provider = agentOS().config.db;
		if (!provider) throw new Error("expected actor database provider");

		await provider.onMigrate(access);

		const tables = sqlite
			.prepare(
				"SELECT name, sql FROM sqlite_schema WHERE type = 'table' AND name LIKE 'agentos_%' ORDER BY name",
			)
			.all() as Array<{ name: string; sql: string }>;
		expect(tables.map((table) => table.name)).toEqual([
			"agentos_actor_dynamic_mounts",
			"agentos_actor_linked_software",
			"agentos_actor_preview_tokens",
			"agentos_actor_schema_version",
		]);
		expect(
			tables.every((table) => table.sql.trimEnd().endsWith("STRICT")),
		).toBe(true);
		expect(
			sqlite
				.prepare(
					"SELECT schema_version FROM agentos_actor_schema_version WHERE singleton = 1",
				)
				.get(),
		).toEqual({ schema_version: 1 });
		expect(() =>
			sqlite
				.prepare(
					"INSERT INTO agentos_actor_preview_tokens(token, port, created_at_ms, expires_at_ms) VALUES (?, ?, ?, ?)",
				)
				.run("a".repeat(48), "not-a-port", 1, 2),
		).toThrow();
		expect(() =>
			sqlite
				.prepare(
					"INSERT INTO agentos_actor_dynamic_mounts(path, descriptor_json) VALUES (?, ?)",
				)
				.run("/mount", "[]"),
		).toThrow();

		execute.mockClear();
		await provider.onMigrate(access);
		expect(
			execute.mock.calls.some(([query]) =>
				String(query).includes("CREATE TABLE agentos_actor_preview_tokens"),
			),
		).toBe(false);
		await access.close();
	});

	test("rolls back actor schema and version changes without touching other owners", async () => {
		const { access, sqlite } = createActorMigrationDatabase();
		sqlite.exec(`
			CREATE TABLE agentos_fs_schema_version (
				singleton INTEGER PRIMARY KEY,
				schema_version INTEGER NOT NULL
			) STRICT;
			INSERT INTO agentos_fs_schema_version VALUES (1, 7);
			CREATE TABLE agentos_core_schema_version (
				singleton INTEGER PRIMARY KEY,
				schema_version INTEGER NOT NULL
			) STRICT;
			INSERT INTO agentos_core_schema_version VALUES (1, 9);
		`);
		const execute = access.execute;
		access.execute = async (query, ...args) => {
			if (/INSERT INTO\s+"?agentos_actor_schema_version"?/i.test(query)) {
				throw new Error("intentional actor version write failure");
			}
			return execute(query, ...args);
		};
		const provider = agentOS().config.db;
		if (!provider) throw new Error("expected actor database provider");

		await expect(provider.onMigrate(access)).rejects.toThrow(
			"intentional actor version write failure",
		);

		expect(
			sqlite
				.prepare(
					"SELECT name FROM sqlite_schema WHERE name LIKE 'agentos_actor_%' ORDER BY name",
				)
				.all(),
		).toEqual([]);
		expect(
			sqlite
				.prepare(
					"SELECT (SELECT schema_version FROM agentos_fs_schema_version) AS fs, (SELECT schema_version FROM agentos_core_schema_version) AS core",
				)
				.get(),
		).toEqual({ fs: 7, core: 9 });
		await access.close();
	});

	test.each([
		{ label: "negative", rows: [{ schema_version: -1 }] },
		{ label: "fractional", rows: [{ schema_version: 0.5 }] },
		{ label: "text", rows: [{ schema_version: "1" }] },
		{ label: "null", rows: [{ schema_version: null }] },
		{
			label: "duplicate",
			rows: [{ schema_version: 0 }, { schema_version: 0 }],
		},
	])("rejects a $label actor schema version", async ({ rows }) => {
		const execute = vi.fn(async (query: string) =>
			query.includes("SELECT schema_version") ? rows : [],
		);
		await expect(
			migrateAgentOsActorTables({ execute } as never),
		).rejects.toThrow(/invalid (?:schema version|migration table)/);
	});

	test("rejects a future actor schema version without changing it", async () => {
		const { access, sqlite } = createActorMigrationDatabase();
		const provider = agentOS().config.db;
		if (!provider) throw new Error("expected actor database provider");
		await provider.onMigrate(access);
		sqlite
			.prepare(
				"UPDATE agentos_actor_schema_version SET schema_version = 2 WHERE singleton = 1",
			)
			.run();

		await expect(provider.onMigrate(access)).rejects.toThrow(
			'migration version 2 in "agentos_actor_schema_version" is newer than supported version 1',
		);
		expect(
			sqlite
				.prepare("SELECT schema_version FROM agentos_actor_schema_version")
				.get(),
		).toEqual({ schema_version: 2 });
		await access.close();
	});

	test("creates and expires actor-only signed preview URLs", async () => {
		const execute = vi.fn(async () => []);
		const actions = createAgentOsActions();
		const context = { db: { execute } } as never;
		const preview = await actions.createPreviewUrl(context, 8080, 60);
		expect(preview).toMatchObject({
			path: `/fetch/${preview.token}`,
			port: 8080,
		});
		expect(preview.expiresAt).toBeGreaterThan(Date.now());
		expect(execute).toHaveBeenCalledWith(
			expect.stringContaining("INSERT INTO agentos_actor_preview_tokens"),
			preview.token,
			8080,
			expect.any(Number),
			preview.expiresAt,
		);

		await actions.expirePreviewUrl(context, preview.token);
		expect(execute).toHaveBeenLastCalledWith(
			expect.stringContaining("DELETE FROM agentos_actor_preview_tokens"),
			preview.token,
		);
	});

	test("bounds active preview tokens", async () => {
		const execute = vi.fn(async (query: string) =>
			query.includes("COUNT(*)") ? [{ count: 1 }] : [],
		);
		const actions = createAgentOsActions({}, {}, { maxActiveTokens: 1 });
		const context = { db: { execute } } as never;
		await expect(actions.createPreviewUrl(context, 8080, 60)).rejects.toThrow(
			"preview token limit 1 reached; raise preview.maxActiveTokens",
		);
	});

	test("returns public typed preview errors and warns near the token limit", async () => {
		try {
			createAgentOsActions({}, {}, { maxActiveTokens: 0 });
			throw new Error("expected invalid preview config to fail");
		} catch (error) {
			expect(error).toMatchObject({
				code: "agentos_preview_invalid_config",
				public: true,
			});
		}

		const execute = vi.fn(async (query: string) =>
			query.includes("COUNT(*)") ? [{ count: 3 }] : [],
		);
		const warn = vi.fn();
		const actions = createAgentOsActions(
			{},
			{},
			{
				defaultExpiresInSeconds: 10,
				maxExpiresInSeconds: 60,
				maxActiveTokens: 5,
			},
		);
		const context = { db: { execute }, log: { warn } } as never;

		await expect(
			actions.createPreviewUrl(context, 0, 10),
		).rejects.toMatchObject({
			code: "agentos_preview_invalid_port",
			public: true,
		});
		await expect(
			actions.createPreviewUrl(context, 8080, 61),
		).rejects.toMatchObject({
			code: "agentos_preview_invalid_ttl",
			public: true,
		});
		await expect(
			actions.createPreviewUrl(context, 8080, 10),
		).resolves.toMatchObject({ port: 8080 });
		expect(warn).toHaveBeenCalledWith(
			expect.objectContaining({
				activeTokenCount: 4,
				limit: 5,
				msg: expect.stringContaining("raise preview.maxActiveTokens"),
			}),
		);
	});

	test("preserves normal actor connection hooks", async () => {
		const onBeforeConnect = vi.fn();
		const onConnect = vi.fn();
		const onDisconnect = vi.fn();
		const createConnState = vi.fn(() => ({ authenticated: true }));
		const definition = agentOS({
			onBeforeConnect,
			onConnect,
			onDisconnect,
			createConnState,
		});
		await definition.config.onBeforeConnect?.(
			{ request: undefined } as never,
			undefined,
		);
		expect(onBeforeConnect).toHaveBeenCalledOnce();
		expect(definition.config.onConnect).toBe(onConnect);
		expect(definition.config.onDisconnect).toBe(onDisconnect);
		expect(definition.config.createConnState).toBe(createConnState);
	});

	test("only bypasses onBeforeConnect for well-formed preview URLs", async () => {
		const onBeforeConnect = vi.fn();
		const definition = agentOS({ onBeforeConnect });
		const token = "a".repeat(48);
		await definition.config.onBeforeConnect?.(
			{
				request: new Request(`https://actor.test/fetch/${token}/path`),
			} as never,
			undefined,
		);
		expect(onBeforeConnect).not.toHaveBeenCalled();

		await definition.config.onBeforeConnect?.(
			{ request: new Request("https://actor.test/fetch/not-a-token") } as never,
			undefined,
		);
		expect(onBeforeConnect).toHaveBeenCalledOnce();
	});

	test("preserves legacy exec results separately from lifecycle execution", async () => {
		const legacyResult = {
			exitCode: 0,
			stdout: new Uint8Array(Buffer.from("legacy")),
			stderr: new Uint8Array(),
		};
		const executionResult = {
			outcome: "succeeded",
			exitCode: 0,
			stdout: "nested",
			stderr: "",
		};
		const exec = vi.fn(async () => legacyResult);
		const processExec = vi.fn(async () => executionResult);
		vi.spyOn(AgentOs, "create").mockResolvedValue({
			exec,
			process: { exec: processExec },
			onCronEvent: vi.fn(),
			onExecutionOutput: vi.fn(() => vi.fn()),
			onExecutionCompleted: vi.fn(() => vi.fn()),
		} as never);
		const actions = createAgentOsActions({});
		const context = {
			actorId: "exec-compatibility-test",
			actorRuntimeSocket: vi.fn(async () => ({
				path: "/tmp/actor.sock",
			})),
			broadcast: vi.fn(),
			db: { execute: vi.fn(async () => []) },
			keepAwake: <T>(promise: Promise<T>) => promise,
			log: { info: vi.fn(), error: vi.fn() },
		} as never;

		await expect(actions.exec(context, "printf legacy")).resolves.toBe(
			legacyResult,
		);
		await expect(actions.process.exec(context, "printf nested")).resolves.toBe(
			executionResult,
		);
		expect(exec).toHaveBeenCalledWith("printf legacy");
		expect(processExec).toHaveBeenCalledWith("printf nested", undefined);
	});

	test("keeps nested and legacy spawn as managed PID processes", async () => {
		let emitOutput: ((event: unknown) => void) | undefined;
		let emitExit: ((event: unknown) => void) | undefined;
		const waitProcess = Promise.resolve({
			pid: 42,
			outcome: "exited",
			exitCode: 0,
		});
		const spawn = vi.fn(async () => ({
			pid: 42,
			state: "running",
			command: "node",
			startedAtMs: 1,
		}));
		const wait = vi.fn(() => waitProcess);
		const vm = {
			process: { spawn, wait },
			onProcessOutput: vi.fn((_pid, callback) => {
				emitOutput = callback;
				return vi.fn();
			}),
			onProcessExit: vi.fn((_pid, callback) => {
				emitExit = callback;
				return vi.fn();
			}),
			onCronEvent: vi.fn(),
			onExecutionOutput: vi.fn(() => vi.fn()),
			onExecutionCompleted: vi.fn(() => vi.fn()),
		};
		vi.spyOn(AgentOs, "create").mockResolvedValue(vm as never);
		const actions = createAgentOsActions({});
		const keepAwake = vi.fn(<T>(promise: Promise<T>) => promise);
		const broadcast = vi.fn();
		const context = {
			actorId: "nested-spawn-test",
			actorRuntimeSocket: vi.fn(async () => ({
				path: "/tmp/actor.sock",
			})),
			broadcast,
			db: { execute: vi.fn(async () => []) },
			keepAwake,
			log: { info: vi.fn(), error: vi.fn() },
		} as never;

		await expect(
			actions.process.spawn(context, "node", ["server.js"]),
		).resolves.toMatchObject({ pid: 42, state: "running" });
		expect(spawn).toHaveBeenCalledWith("node", ["server.js"]);
		expect(wait).toHaveBeenCalledWith(42);
		expect(keepAwake).toHaveBeenCalledWith(waitProcess);

		emitOutput?.({ pid: 42, stream: "stdout", data: "ready" });
		emitExit?.({ pid: 42, exitCode: 0 });
		expect(broadcast).toHaveBeenCalledWith(
			"processOutput",
			expect.objectContaining({ pid: 42 }),
		);
		expect(broadcast).toHaveBeenCalledWith(
			"processExit",
			expect.objectContaining({ pid: 42 }),
		);

		await expect(
			actions.spawn(context, "node", ["legacy.js"]),
		).resolves.toMatchObject({ pid: 42, state: "running" });
	});

	test("keeps generic native session-event hooks awake with actor context", async () => {
		let emitSessionEvent: ((event: unknown) => void) | undefined;
		let resolveHook!: () => void;
		const hook = new Promise<void>((resolve) => {
			resolveHook = resolve;
		});
		const vm = {
			onCronEvent: vi.fn(),
			onExecutionOutput: vi.fn(() => vi.fn()),
			onExecutionCompleted: vi.fn(() => vi.fn()),
			sessions: { open: vi.fn(async () => undefined) },
			onSessionEvent: vi.fn((_sessionId, callback) => {
				emitSessionEvent = callback;
			}),
		};
		vi.spyOn(AgentOs, "create").mockResolvedValue(vm as never);

		const onSessionEvent = vi.fn(() => hook);
		const actions = createAgentOsActions({}, { onSessionEvent });
		const pending: Promise<unknown>[] = [];
		const keepAwake = vi.fn(<T>(promise: Promise<T>) => {
			pending.push(promise);
			return promise;
		});
		const context = {
			actorId: "hook-test",
			actorRuntimeSocket: vi.fn(async () => ({
				path: "/tmp/actor.sock",
			})),
			broadcast: vi.fn(),
			db: { execute: vi.fn(async () => []) },
			keepAwake,
			waitUntil: vi.fn(),
			log: { info: vi.fn(), error: vi.fn() },
		} as never;

		await expect(
			actions.openSession(context, { agent: "test-agent" }),
		).resolves.toBeUndefined();
		emitSessionEvent?.({
			durability: "durable",
			type: "agent_message_chunk",
			sessionId: "main",
			sequence: 1,
			timestamp: "2026-07-18T00:00:00.000Z",
			content: { type: "text", text: "hello" },
		});
		emitSessionEvent?.({
			durability: "durable",
			type: "permission_request",
			sessionId: "main",
			sequence: 2,
			timestamp: "2026-07-18T00:00:01.000Z",
			requestId: "permission-1",
			options: [],
			toolCall: { toolCallId: "tool-1" },
		});
		await vi.waitFor(() => expect(keepAwake).toHaveBeenCalledTimes(2));
		expect(context.waitUntil).not.toHaveBeenCalled();
		resolveHook();
		await Promise.all(pending);

		expect(onSessionEvent).toHaveBeenNthCalledWith(1, context, "main", {
			durability: "durable",
			type: "agent_message_chunk",
			sessionId: "main",
			sequence: 1,
			timestamp: "2026-07-18T00:00:00.000Z",
			content: { type: "text", text: "hello" },
		});
		expect(onSessionEvent).toHaveBeenNthCalledWith(2, context, "main", {
			durability: "durable",
			type: "permission_request",
			sessionId: "main",
			sequence: 2,
			timestamp: "2026-07-18T00:00:01.000Z",
			requestId: "permission-1",
			options: [],
			toolCall: { toolCallId: "tool-1" },
		});
		expect(context.db.execute).toHaveBeenNthCalledWith(
			1,
			"SELECT descriptor_json FROM agentos_actor_dynamic_mounts ORDER BY path",
		);
		expect(context.db.execute).toHaveBeenNthCalledWith(
			2,
			"SELECT descriptor_json FROM agentos_actor_linked_software ORDER BY path",
		);
	});

	test("keeps the actor awake for the full prompt turn", async () => {
		const promptResult = {
			sessionId: "main",
			message: null,
			stopReason: "end_turn",
		};
		let resolvePrompt!: (value: typeof promptResult) => void;
		const prompt = new Promise<typeof promptResult>((resolve) => {
			resolvePrompt = resolve;
		});
		vi.spyOn(AgentOs, "create").mockResolvedValue({
			sessions: { prompt: vi.fn(() => prompt) },
			onCronEvent: vi.fn(),
			onExecutionOutput: vi.fn(() => vi.fn()),
			onExecutionCompleted: vi.fn(() => vi.fn()),
			onSessionEvent: vi.fn(),
		} as never);
		const actions = createAgentOsActions({});
		const keepAwake = vi.fn(<T>(hold: Promise<T>) => hold);
		const context = {
			actorId: "prompt-keep-awake-test",
			actorRuntimeSocket: vi.fn(async () => ({
				path: "/tmp/actor.sock",
			})),
			broadcast: vi.fn(),
			db: { execute: vi.fn(async () => []) },
			keepAwake,
			waitUntil: vi.fn(),
			log: { info: vi.fn(), error: vi.fn() },
		} as never;

		const result = actions.prompt(context, {
			content: [{ type: "text", text: "wait for approval" }],
		});
		await vi.waitFor(() => expect(keepAwake).toHaveBeenCalledOnce());
		expect(keepAwake).toHaveBeenCalledWith(prompt);
		resolvePrompt(promptResult);
		await expect(result).resolves.toEqual(promptResult);
	});

	test("returns public typed prompt errors", async () => {
		const adapterError = Object.assign(new Error("Invalid API key."), {
			code: "acp_api_error",
		});
		vi.spyOn(AgentOs, "create").mockResolvedValue({
			sessions: {
				prompt: vi.fn(async () => {
					throw adapterError;
				}),
			},
			onCronEvent: vi.fn(),
			onExecutionOutput: vi.fn(() => vi.fn()),
			onExecutionCompleted: vi.fn(() => vi.fn()),
			onSessionEvent: vi.fn(),
		} as never);
		const actions = createAgentOsActions({});
		const log = { info: vi.fn(), error: vi.fn() };
		const context = {
			actorId: "prompt-error-test",
			actorRuntimeSocket: vi.fn(async () => ({
				path: "/tmp/actor.sock",
			})),
			broadcast: vi.fn(),
			db: { execute: vi.fn(async () => []) },
			keepAwake: <T>(promise: Promise<T>) => promise,
			waitUntil: vi.fn(),
			log,
		} as never;

		await expect(
			actions.prompt(context, {
				sessionId: "credential-test",
				content: [{ type: "text", text: "hello" }],
			}),
		).rejects.toMatchObject({
			message: "AgentOS prompt failed: Invalid API key.",
			code: "agentos_prompt_failed",
			public: true,
			metadata: { causeCode: "acp_api_error" },
		});
		expect(log.error).toHaveBeenCalledWith(
			expect.objectContaining({
				msg: "agent-os prompt action failed",
				sessionId: "credential-test",
				causeCode: "acp_api_error",
			}),
		);
	});

	test("logs adapter crashes without holding an idle durable session awake", async () => {
		let onAgentExit:
			| ((event: {
					sessionId: string;
					agentType: string;
					processId: string;
					pid: number | null;
					exitCode: number | null;
					restart: "not_attempted";
					restartCount: number;
					maxRestarts: number;
			  }) => void)
			| undefined;
		const vm = {
			onCronEvent: vi.fn(),
			onExecutionOutput: vi.fn(() => vi.fn()),
			onExecutionCompleted: vi.fn(() => vi.fn()),
			sessions: { open: vi.fn(async () => undefined) },
			onSessionEvent: vi.fn(),
		};
		vi.spyOn(AgentOs, "create").mockImplementation(async (options) => {
			onAgentExit = options.onAgentExit as typeof onAgentExit;
			return vm as never;
		});
		const log = { info: vi.fn(), error: vi.fn() };
		const context = {
			actorId: "terminal-exit-test",
			actorRuntimeSocket: vi.fn(async () => ({
				path: "/tmp/actor.sock",
			})),
			broadcast: vi.fn(),
			db: { execute: vi.fn(async () => []) },
			keepAwake: vi.fn(async (hold: Promise<void>) => hold),
			waitUntil: vi.fn(),
			log,
		} as never;
		const userOnAgentExit = vi.fn(() => {
			throw new Error("hook failure");
		});
		const actions = createAgentOsActions({ onAgentExit: userOnAgentExit });
		const sessionId = "terminal-session-1";
		await actions.openSession(context, {
			sessionId,
			agent: "test-agent",
		});
		onAgentExit?.({
			sessionId,
			agentType: "test-agent",
			processId: "process-exit",
			pid: 1,
			exitCode: 1,
			restart: "not_attempted",
			restartCount: 0,
			maxRestarts: 0,
		});
		expect(context.keepAwake).not.toHaveBeenCalled();
		expect(userOnAgentExit).toHaveBeenCalledOnce();
		expect(log.error).toHaveBeenCalledWith(
			expect.objectContaining({
				msg: "agent-os agent adapter exited unexpectedly",
			}),
		);
		expect(log.error).toHaveBeenCalledWith(
			expect.objectContaining({ msg: "agent-os onAgentExit hook failed" }),
		);
	});

	test("rejects collisions with AgentOS defaults", () => {
		expect(() =>
			agentOS({
				actions: { readFile: () => "shadowed" },
			} as never),
		).toThrow("agentOS() action name is reserved: readFile");
	});

	test("runs host cleanup once after the VM has been disposed", async () => {
		const order: string[] = [];
		const vm = {
			onCronEvent: vi.fn(),
			filesystem: {
				readFile: vi.fn(async () => new Uint8Array([1])),
			},
			dispose: vi.fn(async () => {
				order.push("dispose");
			}),
		};
		vi.spyOn(AgentOs, "create").mockResolvedValue(vm as never);
		const definition = agentOS({
			onVmStop: async () => {
				order.push("stop");
			},
			onVmDisposed: async () => {
				order.push("cleanup");
			},
		});
		const context = {
			actorId: "vm-disposed-hook-test",
			actorRuntimeSocket: vi.fn(async () => ({
				path: "/tmp/actor.sock",
			})),
			broadcast: vi.fn(),
			db: { execute: vi.fn(async () => []) },
			keepAwake: <T>(promise: Promise<T>) => promise,
			log: { info: vi.fn(), error: vi.fn() },
		} as never;

		await definition.config.actions.readFile(context, "/fixture");
		await definition.config.onDestroy?.(context);
		await definition.config.onDestroy?.(context);

		expect(order).toEqual(["stop", "dispose", "cleanup"]);
	});

	test("keeps AgentOS limits bounded by default", () => {
		const definition = agentOS();
		expect(definition.config.options.actionTimeout).toBe(2_147_483_647);
		expect(definition.config.options.sleepGracePeriod).toBe(15 * 60_000);
	});
});
