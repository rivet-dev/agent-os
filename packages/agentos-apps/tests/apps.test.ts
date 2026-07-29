import { createHash } from "node:crypto";
import {
	mkdir,
	mkdtemp,
	readFile,
	rm,
	stat,
	symlink,
	writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { AgentOs } from "@rivet-dev/agentos-core";
import { Hono } from "hono";
import { setup } from "rivetkit";
import { afterEach, describe, expect, test, vi } from "vitest";
import {
	createAppsActors,
	migrateAppsTables,
	normalizeScaling,
	normalizeServerlessCallbackPath,
	replicaGuestEnvironment,
	replicaLoopbackExemptPorts,
	resolveAppCallbackSecret,
	type ScalerState,
} from "../src/actors.js";
import {
	configureAppNamespaceRunner,
	provisionAppNamespace,
	type ResolvedRivetConnection,
} from "../src/control-plane.js";
import { deployApp } from "../src/deploy.js";
import { setupApps } from "../src/index.js";
import {
	type AgentOSAppsRoutingClient,
	createAppsRouter,
} from "../src/router.js";
import {
	appRunnerPool,
	canonicalDeploymentHash,
	ensureServerlessRunnerConfig,
	releaseEnvoyVersion,
	runnerSource,
	runtimeLoopbackPort,
	staticRunnerSource,
} from "../src/runtime.js";
import { prepareSource } from "../src/source.js";
import type { PreparedDeployAppInput } from "../src/types.js";

vi.mock("@rivet-dev/agentos-toolchain", () => ({
	packAospkgFromTarBytes(source: Buffer) {
		return { bytes: source, summary: { name: "app", version: "test" } };
	},
}));

const temporaryDirectories: string[] = [];

afterEach(async () => {
	vi.restoreAllMocks();
	vi.unstubAllGlobals();
	vi.unstubAllEnvs();
	while (temporaryDirectories.length > 0) {
		const path = temporaryDirectories.pop();
		if (path) await rm(path, { recursive: true, force: true });
	}
});

function logger() {
	return { info: vi.fn(), warn: vi.fn(), error: vi.fn() };
}

function typecheckPublicApi(): void {
	const { appsActors } = setupApps();
	const registry = setup({ use: { ...appsActors } });
	void registry;
	void deployApp({
		appId: "directory-app",
		source: new URL("../fixtures/app/", import.meta.url),
	});
	void deployApp({
		appId: "memory-app",
		createNamespace: true,
		files: {
			"index.html": "<h1>Hello</h1>",
			"logo.png": new Uint8Array([137, 80, 78, 71]),
		},
		scaling: { maxReplicas: 16 },
	});
}
void typecheckPublicApi;

describe("public API", () => {
	test("keeps one callback credential across app rollouts", () => {
		expect(
			resolveAppCallbackSecret(
				[{ callbackSecret: "older" }, { callbackSecret: "newer" }],
				{ callbackSecret: "active" },
			),
		).toBe("active");
		expect(
			resolveAppCallbackSecret([
				{ callbackSecret: "" },
				{ callbackSecret: "existing" },
			]),
		).toBe("existing");
	});

	test("returns only the three stable actor definitions without doing I/O", () => {
		const fetchMock = vi.fn();
		vi.stubGlobal("fetch", fetchMock);

		const result = setupApps();

		expect(Object.keys(result)).toEqual(["appsActors"]);
		expect(Object.keys(result.appsActors)).toEqual([
			"agentOSAppsApp",
			"agentOSAppsScaler",
			"agentOSAppsReplica",
		]);
		expect(fetchMock).not.toHaveBeenCalled();
		expect(
			(result.appsActors.agentOSAppsApp.config.options as { noSleep?: boolean })
				.noSleep,
		).not.toBe(true);
		expect(
			(
				result.appsActors.agentOSAppsReplica.config.options as {
					noSleep?: boolean;
				}
			).noSleep,
		).toBe(true);
	});

	test("uses scale-to-zero, 128 replicas, and concurrency 8 by default", () => {
		expect(normalizeScaling(undefined)).toEqual({
			minReplicas: 0,
			maxReplicas: 128,
			targetConcurrency: 8,
		});
		expect(normalizeScaling({ maxReplicas: 12 })).toEqual({
			minReplicas: 0,
			maxReplicas: 12,
			targetConcurrency: 8,
		});
		expect(() => normalizeScaling({ minReplicas: 2, maxReplicas: 1 })).toThrow(
			"cannot exceed",
		);
	});
});

describe("conditional Rivet Engine access", () => {
	const configuration = {
		appId: "hello",
		release: "release-1",
		artifactHash: "hash",
		artifactBytes: 4,
		namespace: "app-hello",
		envoyVersion: 7,
		runtime: {
			endpoint: "http://127.0.0.1:6420",
			namespace: "app-hello",
			pool: "agentos-apps-hello",
		},
	};

	test("gives plain and static releases no Rivet environment or Engine exemption", () => {
		expect(replicaGuestEnvironment(configuration)).toEqual({
			NODE_ENV: "production",
		});
		expect(replicaLoopbackExemptPorts(configuration)).toEqual([]);
	});

	test("gives RivetKit releases non-secret routing metadata and only the scoped proxy port", () => {
		const input = { ...configuration, usesRivetKit: true };

		expect(
			replicaGuestEnvironment(input, "http://127.0.0.1:3081/capability"),
		).toMatchObject({
			RIVET_ENDPOINT: "http://127.0.0.1:3081/capability",
			RIVET_NAMESPACE: "app-hello",
			RIVET_POOL: "agentos-apps-hello",
			RIVET_RUNNER: "agentos-apps-hello",
			RIVET_RUNNER_POOL: "agentos-apps-hello",
		});
		expect(replicaGuestEnvironment(input)).not.toHaveProperty("RIVET_TOKEN");
		expect(replicaLoopbackExemptPorts(input, 3_081)).toEqual([3_081]);
	});

	test("adds release security columns to an existing SQLite table once", async () => {
		const columns = new Set(["release_id"]);
		const statements: string[] = [];
		const database = {
			execute: vi.fn(async (sql: string) => {
				statements.push(sql);
				if (sql.startsWith("PRAGMA table_info")) {
					return [...columns].map((name) => ({ name }));
				}
				if (sql.includes("ADD COLUMN callback_secret"))
					columns.add("callback_secret");
				if (sql.includes("ADD COLUMN uses_rivetkit"))
					columns.add("uses_rivetkit");
				return [];
			}),
		};

		await migrateAppsTables(database as never);
		await migrateAppsTables(database as never);

		expect(
			statements.filter((sql) =>
				sql.includes("ALTER TABLE agentos_apps_releases"),
			),
		).toHaveLength(2);
		expect(
			statements.find((sql) =>
				sql.includes("CREATE TABLE IF NOT EXISTS agentos_apps_releases"),
			),
		).toContain("uses_rivetkit INTEGER NOT NULL DEFAULT 0");
		expect(
			statements.find((sql) =>
				sql.includes("CREATE TABLE IF NOT EXISTS agentos_apps_releases"),
			),
		).toContain("callback_secret TEXT NOT NULL DEFAULT ''");
	});
});

describe("source loading and deployment facade", () => {
	test("loads sorted binary files, ignores fixed local directories, and preserves empties", async () => {
		const root = await mkdtemp(join(tmpdir(), "agentos-apps-source-"));
		temporaryDirectories.push(root);
		await mkdir(join(root, "assets"), { recursive: true });
		await mkdir(join(root, "node_modules", "ignored"), { recursive: true });
		await writeFile(join(root, "assets", "empty.txt"), "");
		await writeFile(
			join(root, "assets", "logo.bin"),
			new Uint8Array([0, 1, 255]),
		);
		await writeFile(join(root, "index.html"), "hello");
		await writeFile(join(root, "node_modules", "ignored", "index.js"), "bad");

		const files = await prepareSource({
			appId: "source-app",
			source: new URL(`file://${root}/`),
		});

		expect(Object.keys(files)).toEqual([
			"assets/empty.txt",
			"assets/logo.bin",
			"index.html",
		]);
		expect(files["assets/empty.txt"]).toHaveLength(0);
		expect([...files["assets/logo.bin"]!]).toEqual([0, 1, 255]);
	});

	test("rejects source symlinks and invalid app IDs", async () => {
		const root = await mkdtemp(join(tmpdir(), "agentos-apps-symlink-"));
		temporaryDirectories.push(root);
		await writeFile(join(root, "index.html"), "hello");
		await symlink(join(root, "index.html"), join(root, "linked.html"));

		await expect(
			prepareSource({
				appId: "source-app",
				source: new URL(`file://${root}/`),
			}),
		).rejects.toMatchObject({ code: "agentos_apps_source_symlink" });
		await expect(
			prepareSource({ appId: "Not Valid", files: { "index.html": "x" } }),
		).rejects.toMatchObject({ code: "agentos_apps_invalid_app_id" });
	});

	test("deploys in-memory bytes through an ordinary supplied client", async () => {
		const calls: unknown[] = [];
		const fetchMock = vi.fn();
		vi.stubGlobal("fetch", fetchMock);
		vi.stubEnv("RIVET_ENGINE", "http://existing.test");
		vi.stubEnv("RIVET_NAMESPACE", "existing");
		const client = {
			agentOSAppsApp: {
				getOrCreate: (key: string | string[]) => ({
					resolve: async () => "app-actor-id",
					deploy: async (input: unknown) => {
						calls.push({ key, input });
						return {
							appId: "memory-app",
							release: "release-1",
							namespace: "namespace-1",
							pool: appRunnerPool("memory-app"),
							regions: ["local"],
							appActorId: "app-actor-id",
							usesRivetKit: false,
						};
					},
				}),
			},
		};

		const result = await deployApp(
			{
				appId: "memory-app",
				files: {
					"index.html": "<h1>Hello</h1>",
					"asset.bin": new Uint8Array([0, 255]),
				},
			},
			{ client },
		);

		expect(result).toEqual({
			appId: "memory-app",
			release: "release-1",
			namespace: "namespace-1",
			pool: appRunnerPool("memory-app"),
			regions: ["local"],
		});
		expect(calls).toHaveLength(1);
		expect(calls[0]).toMatchObject({
			key: ["memory-app"],
			input: {
				appId: "memory-app",
				namespace: "existing",
				runtime: {
					pool: appRunnerPool("memory-app"),
				},
				files: {
					"index.html": expect.any(Uint8Array),
					"asset.bin": expect.any(Uint8Array),
				},
			},
		});
		expect(fetchMock).not.toHaveBeenCalled();
	});

	test("creates a namespace only when requested by deployApp", async () => {
		let createdName: string | undefined;
		const requests: Array<{ url: URL; init?: RequestInit }> = [];
		vi.stubGlobal(
			"fetch",
			vi.fn(async (input: string | URL | Request, init?: RequestInit) => {
				const url = new URL(String(input));
				requests.push({ url, init });
				if (url.pathname === "/namespaces" && init?.method === "POST") {
					createdName = (JSON.parse(String(init.body)) as { name: string })
						.name;
					return Response.json({});
				}
				if (url.pathname === "/namespaces") {
					return Response.json({
						namespaces: createdName ? [{ name: createdName }] : [],
					});
				}
				throw new Error(`unexpected control request ${url}`);
			}),
		);
		vi.stubEnv("RIVET_ENGINE", "http://engine.test");
		const deploy = vi.fn(async (input: PreparedDeployAppInput) => ({
			appId: input.appId,
			release: "release-1",
			namespace: input.namespace,
			pool: input.runtime.pool,
			regions: ["local"],
			appActorId: "app-actor-id",
			usesRivetKit: false,
		}));

		const result = await deployApp(
			{
				appId: "isolated-app",
				createNamespace: true,
				files: { "index.html": "hello" },
			},
			{
				client: {
					agentOSAppsApp: {
						getOrCreate: () => ({
							resolve: async () => "app-actor-id",
							deploy,
						}),
					},
				},
			},
		);

		expect(result.namespace).toMatch(/^agentos-app-isolated-app-[a-f0-9]{10}$/);
		expect(deploy).toHaveBeenCalledWith(
			expect.objectContaining({ namespace: result.namespace }),
		);
		expect(
			requests.filter(
				(request) =>
					request.url.pathname === "/namespaces" &&
					request.init?.method === "POST",
			),
		).toHaveLength(1);
	});
});

describe("namespace and runner plumbing", () => {
	test("creates one deterministic namespace and configures its app pool", async () => {
		let createdName: string | undefined;
		let lookupAttempts = 0;
		const requests: Array<{ url: URL; init?: RequestInit }> = [];
		vi.stubGlobal(
			"fetch",
			vi.fn(async (input: string | URL | Request, init?: RequestInit) => {
				const url = new URL(String(input));
				requests.push({ url, init });
				if (url.pathname === "/namespaces" && init?.method === "POST") {
					const body = JSON.parse(String(init.body)) as { name: string };
					createdName = body.name;
					return Response.json({});
				}
				if (url.pathname === "/namespaces") {
					lookupAttempts += 1;
					if (lookupAttempts === 1) {
						return new Response("retry", { status: 503 });
					}
					return Response.json({
						namespaces: createdName ? [{ name: createdName }] : [],
					});
				}
				if (url.pathname === "/datacenters") {
					return Response.json({ datacenters: [{ name: "us-west" }] });
				}
				if (url.pathname.startsWith("/runner-configs/")) {
					return Response.json({});
				}
				throw new Error(`unexpected request ${url}`);
			}),
		);
		const connection: ResolvedRivetConnection = {
			endpoint: "http://engine.test",
			namespace: "default",
			token: "secret",
		};

		const first = await provisionAppNamespace("hello", connection);
		const second = await provisionAppNamespace("hello", connection);
		await configureAppNamespaceRunner(
			"app-actor-id",
			{
				endpoint: first.endpoint,
				namespace: first.namespace,
				pool: first.pool,
			},
			"callback-secret",
			connection,
		);
		const runnerConfigRequest = requests.find((request) =>
			request.url.pathname.startsWith("/runner-configs/"),
		);
		expect(
			JSON.parse(String(runnerConfigRequest?.init?.body)).datacenters["us-west"]
				.serverless.url,
		).toBe(
			"http://engine.test/gateway/app-actor-id/request/.agentos/apps/rivet",
		);

		expect(first).toEqual(second);
		expect(first.namespace).toMatch(/^agentos-app-hello-[a-f0-9]{10}$/);
		expect(first.pool).toBe(appRunnerPool("hello"));
		expect(runnerConfigRequest?.url.pathname).toBe(
			`/runner-configs/${appRunnerPool("hello")}`,
		);
		expect(
			requests.filter(
				(request) =>
					request.url.pathname === "/namespaces" &&
					request.init?.method === "POST",
			),
		).toHaveLength(1);
		expect(
			requests.find((request) =>
				request.url.pathname.startsWith("/runner-configs/"),
			)?.init,
		).toMatchObject({
			method: "PUT",
			headers: expect.objectContaining({ authorization: "Bearer secret" }),
		});
	});

	test("scopes generated namespace identity to the host namespace", async () => {
		vi.stubGlobal(
			"fetch",
			vi.fn(async (input: string | URL | Request, init?: RequestInit) => {
				const url = new URL(String(input));
				if (url.pathname === "/namespaces" && init?.method === "POST") {
					return Response.json({});
				}
				if (url.pathname === "/namespaces") {
					return Response.json({ namespaces: [] });
				}
				throw new Error(`unexpected request ${url}`);
			}),
		);
		const base = {
			endpoint: "http://engine.test",
		};

		const first = await provisionAppNamespace("hello", {
			...base,
			namespace: "tenant-a",
		});
		const second = await provisionAppNamespace("hello", {
			...base,
			namespace: "tenant-b",
		});

		expect(first.namespace).not.toBe(second.namespace);
		expect(first.pool).toBe(second.pool);
	});

	test("bounds and formats serverless runner configuration", async () => {
		const fetchMock = vi
			.fn()
			.mockResolvedValueOnce(
				new Response("retry", {
					status: 429,
					headers: { "retry-after": "0" },
				}),
			)
			.mockResolvedValueOnce(
				Response.json({ datacenters: [{ name: "us-west" }] }),
			)
			.mockResolvedValueOnce(Response.json({}));
		vi.stubGlobal("fetch", fetchMock);

		await ensureServerlessRunnerConfig({
			endpoint: "http://engine.test/base",
			namespace: "app-hello",
			pool: "app-pool",
			token: "secret",
			callbackSecret: "callback-secret",
			url: "http://engine.test/gateway/agentOSAppsApp/.agentos/apps/rivet",
		});

		expect(String(fetchMock.mock.calls[1]?.[0])).toBe(
			"http://engine.test/datacenters?namespace=app-hello",
		);
		expect(String(fetchMock.mock.calls[2]?.[0])).toBe(
			"http://engine.test/runner-configs/app-pool?namespace=app-hello",
		);
		expect(JSON.parse(String(fetchMock.mock.calls[2]?.[1]?.body))).toEqual({
			datacenters: {
				"us-west": {
					serverless: {
						url: "http://engine.test/gateway/agentOSAppsApp/.agentos/apps/rivet",
						headers: {
							"x-agentos-app-callback-token": "callback-secret",
						},
						request_lifespan: 3_600,
						metadata_poll_interval: 1_000,
						max_runners: 1_024,
						min_runners: 0,
						runners_margin: 0,
						slots_per_runner: 1,
					},
					metadata: {},
					drain_on_version_upgrade: true,
				},
			},
		});
	});
});

describe("serverless callback credentials", () => {
	test("rejects invalid callback secrets and strips trusted credentials from the guest", async () => {
		const definitions = createAppsActors();
		const onRequest = definitions.agentOSAppsApp.config.onRequest as (
			context: any,
			request: Request,
		) => Promise<Response>;
		const actions = definitions.agentOSAppsApp.config.actions as Record<
			string,
			(...args: any[]) => any
		>;
		const callbackSecret = "release-callback-secret";
		const releaseRow = {
			release_id: "release-1",
			created_at: Date.now(),
			status: "ready",
			entrypoint: "index.js",
			artifact_hash: "hash",
			artifact_bytes: 4,
			build_error: null,
			regions_json: JSON.stringify(["us-west"]),
			scaling_json: JSON.stringify({
				minReplicas: 0,
				maxReplicas: 128,
				targetConcurrency: 8,
			}),
			namespace: "app-hello",
			envoy_version: 1,
			runtime_endpoint: "http://engine.test",
			runtime_pool: "agentos-apps-guest",
			callback_secret: callbackSecret,
			uses_rivetkit: 1,
		};
		const acquire = vi.fn(async () => ({
			admissionId: "admission-1",
			leaseMs: 60_000,
			key: ["hello", "release-1", "us-west", "0"],
			release: "release-1",
			region: "us-west",
			replicaCount: 1,
			queueDelayMs: 0,
			coldStart: false,
		}));
		const release = vi.fn(async () => ({ released: true }));
		const replicaFetch = vi.fn(
			async (_input: string | URL | Request, _init?: RequestInit) =>
				new Response("metadata", {
					headers: { "content-type": "text/plain" },
				}),
		);
		const context = {
			actorId: "app-actor",
			key: ["hello"],
			region: "us-west",
			state: {
				activeRelease: "release-1",
				namespace: "app-hello",
				revision: 1,
			},
			db: {
				execute: vi.fn(async (sql: string) => {
					if (sql.startsWith("SELECT * FROM agentos_apps_releases")) {
						return [releaseRow];
					}
					throw new Error(`unexpected SQL: ${sql}`);
				}),
			},
			client: () => ({
				agentOSAppsScaler: {
					getOrCreate: () => ({ acquire, release }),
				},
				agentOSAppsReplica: {
					getOrCreate: () => ({ fetch: replicaFetch }),
				},
			}),
			log: logger(),
		};
		const callbackUrl =
			"http://host.test/gateway/app-actor/request/.agentos/apps/rivet/metadata";

		const rejected = await onRequest(
			context,
			new Request(callbackUrl, {
				headers: {
					"user-agent": "RivetEngine/test",
					"x-agentos-app-callback-token": "wrong",
				},
			}),
		);

		expect(rejected.status).toBe(401);
		expect(acquire).not.toHaveBeenCalled();

		const accepted = await onRequest(
			context,
			new Request(callbackUrl, {
				headers: {
					authorization: "Bearer host-management-token",
					"user-agent": "RivetEngine/test",
					"x-agentos-app-callback-token": callbackSecret,
					"x-rivet-token": "host-management-token",
					"x-safe": "yes",
				},
			}),
		);

		expect(accepted.status).toBe(200);
		expect(await accepted.text()).toBe("metadata");
		const forwarded = replicaFetch.mock.calls[0]?.[1] as
			| { headers?: Record<string, string> }
			| undefined;
		expect(forwarded?.headers).toMatchObject({ "x-safe": "yes" });
		expect(forwarded?.headers).not.toHaveProperty("authorization");
		expect(forwarded?.headers).not.toHaveProperty("x-rivet-token");
		expect(forwarded?.headers).not.toHaveProperty(
			"x-agentos-app-callback-token",
		);
		expect(release).toHaveBeenCalledWith("admission-1");
		await expect(
			actions.getRelease!(context, "release-1"),
		).resolves.not.toHaveProperty("callbackSecret");
		const inspection = await actions.inspect!(context);
		expect(inspection.releases[0]).not.toHaveProperty("callbackSecret");
	});
});

describe("HTTP router", () => {
	test("redirects a bare app path so relative static assets stay under the app", async () => {
		const getOrCreate = vi.fn();
		const server = new Hono();
		server.route(
			"/apps",
			createAppsRouter({
				client: {
					agentOSAppsApp: { getOrCreate },
				} as unknown as AgentOSAppsRoutingClient,
			}),
		);

		const response = await server.request(
			"http://host.test/apps/static-site?preview=1",
		);

		expect(response.status).toBe(308);
		expect(response.headers.get("location")).toBe(
			"http://host.test/apps/static-site/?preview=1",
		);
		expect(
			new URL("styles.css", response.headers.get("location")!).pathname,
		).toBe("/apps/static-site/styles.css");
		expect(getOrCreate).not.toHaveBeenCalled();
	});

	test("forwards the canonical app root to the guest root", async () => {
		const fetch = vi.fn(async () => new Response(null, { status: 204 }));
		const client = {
			agentOSAppsApp: {
				getOrCreate: () => ({
					fetch,
				}),
			},
		} as unknown as AgentOSAppsRoutingClient;
		const server = new Hono();
		server.route("/apps", createAppsRouter({ client }));

		const response = await server.request(
			"http://host.test/apps/static-site/?preview=1",
		);

		expect(response.status).toBe(204);
		expect(fetch).toHaveBeenCalledWith(
			expect.objectContaining({
				url: "http://host.test/?preview=1",
				method: "GET",
			}),
		);
	});

	test("normalizes Rivet Engine callbacks before forwarding them to the guest", () => {
		expect(
			normalizeServerlessCallbackPath(
				new Request(
					"http://host.test/gateway/actor-id/.agentos/apps/rivet/metadata",
					{
						headers: { "user-agent": "RivetEngine/test" },
					},
				),
			),
		).toBe("/api/rivet/metadata");
		expect(
			normalizeServerlessCallbackPath(
				new Request(
					"http://host.test/gateway/actor-id/.agentos/apps/rivet/start",
					{
						method: "POST",
						headers: { "user-agent": "RivetEngine/test" },
					},
				),
			),
		).toBe("/api/rivet/start");
		expect(
			normalizeServerlessCallbackPath(
				new Request("http://host.test/api/rivet/metadata"),
			),
		).toBeUndefined();
	});

	test("mounts under a Hono prefix and streams the replica response", async () => {
		const fetch = vi.fn(async (request: Request) => {
			expect(request.headers.get("connection")).toBe("keep-alive");
			return new Response("hello world");
		});
		const client = {
			agentOSAppsApp: {
				getOrCreate: () => ({
					fetch,
				}),
			},
		} as unknown as AgentOSAppsRoutingClient;
		const server = new Hono();
		server.route("/apps", createAppsRouter({ client }));

		const response = await server.request(
			"http://host.test/apps/hello/chat/messages?cursor=2",
			{ headers: { connection: "keep-alive", "x-custom": "yes" } },
		);

		expect(response.status).toBe(200);
		expect(await response.text()).toBe("hello world");
		expect(fetch).toHaveBeenCalledWith(
			expect.objectContaining({
				url: "http://host.test/chat/messages?cursor=2",
			}),
		);
	});

	test("maps invalid application IDs before touching Rivet", async () => {
		const client = {
			agentOSAppsApp: { getOrCreate: vi.fn() },
		} as unknown as AgentOSAppsRoutingClient;
		const response = await createAppsRouter({ client }).request("/Not-Valid");

		expect(response.status).toBe(400);
		expect(client.agentOSAppsApp.getOrCreate).not.toHaveBeenCalled();
	});
});

describe("regional scaler", () => {
	test("warns on upward 50% crossings, retains warm replicas, and scales to zero", async () => {
		const definitions = createAppsActors();
		const actions = definitions.agentOSAppsScaler.config.actions as Record<
			string,
			(...args: any[]) => any
		>;
		const state = (
			(definitions.agentOSAppsScaler.config as any)
				.createState as () => ScalerState
		)();
		const release = {
			release: "release-1",
			artifactHash: "hash",
			artifactBytes: 4,
			createdAt: Date.now(),
			regions: ["us-west"],
			scaling: {
				minReplicas: 0,
				maxReplicas: 2,
				targetConcurrency: 1,
			},
			status: "ready" as const,
			entrypoint: "index.js",
			namespace: "app-hello",
			envoyVersion: 1,
			runtimeEndpoint: "http://localhost:6420",
			runtimePool: "agentos-apps-guest",
			usesRivetKit: true,
		};
		const replica = {
			configure: vi.fn(async () => undefined),
			inspect: vi.fn(async () => ({ release: null, startedAt: null })),
			vmFetch: vi.fn(async () => ({
				status: 200,
				statusText: "OK",
				headers: {},
				body: new TextEncoder().encode(
					JSON.stringify({ release: release.release }),
				),
			})),
			markStarted: vi.fn(async () => undefined),
			destroy: vi.fn(async () => undefined),
		};
		const log = logger();
		const context = {
			actorId: "scaler-test",
			key: ["hello", release.release, "us-west"],
			region: "us-west",
			state,
			client: () => ({
				agentOSAppsApp: {
					getOrCreate: () => ({ getRelease: async () => release }),
				},
				agentOSAppsReplica: { getOrCreate: () => replica },
			}),
			keepAwake: <T>(promise: Promise<T>) => promise,
			schedule: { after: vi.fn(async () => undefined) },
			log,
			destroy: vi.fn(),
		};

		await actions.prepare!(context, {
			appId: "hello",
			release,
			region: "us-west",
			verifyReplica: true,
		});
		expect(replica.configure).toHaveBeenCalledWith(
			expect.objectContaining({ usesRivetKit: true }),
		);
		expect(state.replicas).toHaveLength(1);
		expect(log.warn).not.toHaveBeenCalledWith(
			expect.objectContaining({ maxReplicas: 2 }),
		);

		const firstAdmission = await actions.acquire!(context);
		await vi.waitFor(() => expect(state.replicas).toHaveLength(2));
		expect(log.warn).toHaveBeenCalledTimes(1);
		expect(log.warn).toHaveBeenCalledWith(
			expect.objectContaining({
				appId: "hello",
				release: "release-1",
				region: "us-west",
				maxReplicas: 2,
				utilizationPercent: 100,
			}),
		);
		await actions.release!(context, firstAdmission.admissionId);

		for (const candidate of state.replicas) {
			candidate.lastUsedAt = Date.now() - 5 * 60_000 - 1;
		}
		await actions.reconcile!(context);
		expect(state.replicas).toHaveLength(1);
		expect(state.capacityWarningLatched).toBe(false);
		state.replicas[0]!.lastUsedAt = Date.now() - 5 * 60_000 - 1;
		await actions.reconcile!(context);
		expect(state.replicas).toHaveLength(0);

		const coldAdmission = await actions.acquire!(context);
		expect(coldAdmission.coldStart).toBe(true);
		await vi.waitFor(() => expect(state.replicas).toHaveLength(2));
		expect(log.warn).toHaveBeenCalledTimes(2);
		await actions.release!(context, coldAdmission.admissionId);

		const abandonedAdmission = await actions.acquire!(context);
		state.admissions![abandonedAdmission.admissionId]!.expiresAt =
			Date.now() - 1;
		await actions.reconcile!(context);
		expect(state.admissions![abandonedAdmission.admissionId]).toBeUndefined();
		expect(
			state.replicas.reduce(
				(total, candidate) => total + candidate.activeRequests,
				0,
			),
		).toBe(0);
	});

	test("recovers durable warming reservations when the scaler wakes", async () => {
		const definitions = createAppsActors();
		const config = definitions.agentOSAppsScaler.config as any;
		const state = (config.createState as () => ScalerState)();
		const warmingKey = ["hello", "release-1", "us-west", "3"];
		Object.assign(state, {
			appId: "hello",
			release: "release-1",
			region: "us-west",
			scaling: {
				minReplicas: 1,
				maxReplicas: 128,
				targetConcurrency: 8,
			},
			warmingReplicas: 1,
			warmingReplicaKeys: [warmingKey],
			nextReplicaIndex: 4,
		});
		const destroyReplica = vi.fn(async () => undefined);
		const context = {
			actorId: "scaler-recovery",
			key: ["hello", "release-1", "us-west"],
			region: "us-west",
			state,
			client: () => ({
				agentOSAppsReplica: {
					getOrCreate: vi.fn(() => ({ destroy: destroyReplica })),
				},
			}),
			schedule: { after: vi.fn(async () => undefined) },
			log: logger(),
			destroy: vi.fn(),
		};

		await config.onWake(context);

		expect(destroyReplica).toHaveBeenCalledTimes(1);
		expect(state.warmingReplicas).toBe(0);
		expect(state.warmingReplicaKeys).toEqual([]);
		expect(context.schedule.after).toHaveBeenCalledWith(1, "reconcile");
		expect(context.log.warn).toHaveBeenCalledWith(
			expect.objectContaining({ strandedReservations: 1 }),
		);
	});

	test("destroys a replica whose warm completes after scaler retirement", async () => {
		const definitions = createAppsActors();
		const actions = definitions.agentOSAppsScaler.config.actions as Record<
			string,
			(...args: any[]) => any
		>;
		const state = (
			(definitions.agentOSAppsScaler.config as any)
				.createState as () => ScalerState
		)();
		const release = {
			release: "release-1",
			artifactHash: "hash",
			artifactBytes: 4,
			createdAt: Date.now(),
			regions: ["us-west"],
			scaling: {
				minReplicas: 0,
				maxReplicas: 2,
				targetConcurrency: 1,
			},
			status: "ready" as const,
			entrypoint: "index.js",
			namespace: "app-hello",
			envoyVersion: 1,
			runtimeEndpoint: "http://localhost:6420",
			runtimePool: "agentos-apps-guest",
		};
		let finishSecondConfigure!: () => void;
		const secondConfigure = new Promise<void>((resolve) => {
			finishSecondConfigure = resolve;
		});
		const makeReplica = (configure: () => Promise<void>) => ({
			configure: vi.fn(configure),
			inspect: vi.fn(async () => ({ release: null, startedAt: null })),
			vmFetch: vi.fn(async () => ({
				status: 200,
				statusText: "OK",
				headers: {},
				body: new TextEncoder().encode(
					JSON.stringify({ release: release.release }),
				),
			})),
			markStarted: vi.fn(async () => undefined),
			destroy: vi.fn(async () => undefined),
		});
		const firstReplica = makeReplica(async () => undefined);
		const secondReplica = makeReplica(() => secondConfigure);
		const keepAwake = vi.fn(<T>(promise: Promise<T>) => promise);
		const context = {
			actorId: "scaler-retire-during-warm",
			key: ["hello", release.release, "us-west"],
			region: "us-west",
			state,
			client: () => ({
				agentOSAppsApp: {
					getOrCreate: () => ({ getRelease: async () => release }),
				},
				agentOSAppsReplica: {
					getOrCreate: (key: string[]) =>
						key.at(-1) === "0" ? firstReplica : secondReplica,
				},
			}),
			keepAwake,
			schedule: { after: vi.fn(async () => undefined) },
			log: logger(),
			destroy: vi.fn(),
		};

		await actions.prepare!(context, {
			appId: "hello",
			release,
			region: "us-west",
			verifyReplica: true,
		});
		const admission = await actions.acquire!(context);
		await vi.waitFor(() => expect(secondReplica.configure).toHaveBeenCalled());
		expect(state.warmingReplicas).toBe(1);

		await actions.retire!(context);
		finishSecondConfigure();

		await vi.waitFor(() => expect(secondReplica.destroy).toHaveBeenCalled());
		expect(state.warmingReplicas).toBe(0);
		expect(state.warmingReplicaKeys).toEqual([]);
		expect(state.replicas).not.toContainEqual(
			expect.objectContaining({ key: ["hello", "release-1", "us-west", "1"] }),
		);
		expect(keepAwake).toHaveBeenCalled();

		await actions.release!(context, admission.admissionId);
		expect(firstReplica.destroy).toHaveBeenCalled();
		expect(context.destroy).toHaveBeenCalled();
	});

	test("logs a failed background warm and clears its reservation", async () => {
		const definitions = createAppsActors();
		const actions = definitions.agentOSAppsScaler.config.actions as Record<
			string,
			(...args: any[]) => any
		>;
		const state = (
			(definitions.agentOSAppsScaler.config as any)
				.createState as () => ScalerState
		)();
		const release = {
			release: "release-1",
			artifactHash: "hash",
			artifactBytes: 4,
			createdAt: Date.now(),
			regions: ["us-west"],
			scaling: {
				minReplicas: 0,
				maxReplicas: 2,
				targetConcurrency: 1,
			},
			status: "ready" as const,
			entrypoint: "index.js",
			namespace: "app-hello",
			envoyVersion: 1,
			runtimeEndpoint: "http://localhost:6420",
			runtimePool: "agentos-apps-guest",
		};
		Object.assign(state, {
			appId: "hello",
			release: release.release,
			region: "us-west",
			scaling: release.scaling,
			replicas: [
				{
					key: ["hello", release.release, "us-west", "0"],
					readyAt: Date.now(),
					activeRequests: 0,
					lastUsedAt: Date.now(),
					draining: false,
				},
			],
			nextReplicaIndex: 1,
		});
		const warmError = new Error("warm failed");
		const warmingReplica = {
			configure: vi.fn(async () => {
				throw warmError;
			}),
			inspect: vi.fn(),
			vmFetch: vi.fn(),
			markStarted: vi.fn(),
			destroy: vi.fn(async () => undefined),
		};
		const log = logger();
		const keepAwake = vi.fn(<T>(promise: Promise<T>) => promise);
		const context = {
			actorId: "scaler-failed-background-warm",
			key: ["hello", release.release, "us-west"],
			region: "us-west",
			state,
			client: () => ({
				agentOSAppsApp: {
					getOrCreate: () => ({ getRelease: async () => release }),
				},
				agentOSAppsReplica: {
					getOrCreate: () => warmingReplica,
				},
			}),
			keepAwake,
			schedule: { after: vi.fn(async () => undefined) },
			log,
			destroy: vi.fn(),
		};

		const admission = await actions.acquire!(context);

		await vi.waitFor(() =>
			expect(log.error).toHaveBeenCalledWith({
				msg: "AgentOS Apps background replica warm failed",
				error: warmError,
			}),
		);
		expect(keepAwake).toHaveBeenCalled();
		expect(warmingReplica.destroy).toHaveBeenCalled();
		expect(state.warmingReplicas).toBe(0);
		expect(state.warmingReplicaKeys).toEqual([]);

		await actions.release!(context, admission.admissionId);
	});
});

describe("replica artifact lifecycle", () => {
	test("rehydrates from chunks and deletes the temporary package after VM disposal", async () => {
		const artifact = new Uint8Array([1, 2, 3, 4, 5]);
		const hash = createHash("sha256").update(artifact).digest("hex");
		let mountedPath: string | undefined;
		let existedDuringDispose = false;
		let loopbackExemptPorts: number[] | undefined;
		const vm = {
			onCronEvent: vi.fn(),
			spawn: vi.fn(() => ({ pid: 7 })),
			waitProcess: vi.fn(async () => 0),
			stopProcess: vi.fn(),
			dispose: vi.fn(async () => {
				if (mountedPath) {
					existedDuringDispose = (await stat(mountedPath)).isFile();
				}
			}),
			readFile: vi.fn(async () => new Uint8Array([9])),
		};
		vi.spyOn(AgentOs, "create").mockImplementation(async (options) => {
			loopbackExemptPorts = options?.loopbackExemptPorts;
			mountedPath = (
				options?.mounts?.[0] as {
					plugin?: { config?: { tarPath?: string } };
				}
			)?.plugin?.config?.tarPath;
			return vm as never;
		});
		vi.stubGlobal(
			"fetch",
			vi.fn(async (input: string | URL | Request) => {
				const url = new URL(String(input));
				if (url.pathname === "/datacenters") {
					return Response.json({ datacenters: [{ name: "local" }] });
				}
				return Response.json({});
			}),
		);
		vi.stubEnv("RIVET_TOKEN", "host-management-token");
		const definitions = createAppsActors();
		const replicaDefinition = definitions.agentOSAppsReplica;
		const actions = replicaDefinition.config.actions as Record<
			string,
			(...args: any[]) => any
		>;
		const context = {
			actorId: "replica-lifecycle",
			key: ["hello", "release-1", "local", "0"],
			region: "local",
			state: {
				configuration: null,
				startedAt: null,
				guestPid: null,
			},
			client: () => ({
				agentOSAppsApp: {
					getOrCreate: () => ({
						getArtifactManifest: async () => ({
							hash,
							bytes: artifact.byteLength,
							chunks: 1,
							chunkBytes: 512 * 1024,
						}),
						readArtifactChunk: async () => artifact,
					}),
				},
			}),
			actorRuntimeSocket: async () => ({ path: "/tmp/actor.sock" }),
			db: { execute: vi.fn(async () => []) },
			keepAwake: <T>(promise: Promise<T>) => promise,
			broadcast: vi.fn(),
			log: logger(),
		};
		await actions.configure!(context, {
			appId: "hello",
			release: "release-1",
			artifactHash: hash,
			artifactBytes: artifact.byteLength,
			namespace: "app-hello",
			envoyVersion: 1,
			usesRivetKit: false,
			runtime: {
				endpoint: "http://localhost:6420",
				namespace: "app-hello",
				pool: "agentos-apps-guest",
			},
		});

		await expect(actions.readFile!(context, "/app/index.js")).resolves.toEqual(
			new Uint8Array([9]),
		);
		expect(vm.spawn).toHaveBeenCalledWith(
			"node",
			["/app/main.mjs"],
			expect.objectContaining({
				env: { NODE_ENV: "production" },
			}),
		);
		expect(loopbackExemptPorts).toEqual([]);
		expect(mountedPath).toBeTruthy();
		expect(await readFile(mountedPath!)).toEqual(Buffer.from(artifact));

		await replicaDefinition.config.onDestroy?.(context as never);

		expect(existedDuringDispose).toBe(true);
		await expect(stat(mountedPath!)).rejects.toMatchObject({ code: "ENOENT" });
	});
});

describe("runtime generation", () => {
	test("hashes binary source deterministically with length-delimited fields", () => {
		const first = canonicalDeploymentHash({
			files: { a: new TextEncoder().encode("1\0b\0\0\0\0\0\0\0\x012") },
			entrypoint: "a",
			build: false,
			packagingIdentity: "builder@1",
		});
		const second = canonicalDeploymentHash({
			files: {
				a: new TextEncoder().encode("1"),
				b: new TextEncoder().encode("2"),
			},
			entrypoint: "a",
			build: false,
			packagingIdentity: "builder@1",
		});
		expect(first).not.toBe(second);
		expect(
			canonicalDeploymentHash({
				files: { asset: new Uint8Array([0, 255]) },
				entrypoint: "asset",
				build: false,
				packagingIdentity: "builder@1",
			}),
		).toBe(
			canonicalDeploymentHash({
				files: { asset: new Uint8Array([0, 255]) },
				entrypoint: "asset",
				build: false,
				packagingIdentity: "builder@1",
			}),
		);
		expect(
			canonicalDeploymentHash({
				files: { asset: new Uint8Array([0, 255]) },
				entrypoint: "asset",
				build: false,
				packagingIdentity: "builder@1",
				deploymentIdentity: '{"regions":["us-west"]}',
			}),
		).not.toBe(
			canonicalDeploymentHash({
				files: { asset: new Uint8Array([0, 255]) },
				entrypoint: "asset",
				build: false,
				packagingIdentity: "builder@1",
				deploymentIdentity: '{"regions":["eu-west"]}',
			}),
		);
	});

	test("generates server and static runners with readiness endpoints", () => {
		const server = runnerSource({
			entrypoint: "src/index.mjs",
			release: "release",
			port: 3_080,
			maxRequestBytes: 1_024,
			maxResponseBytes: 1_024,
			usesRivetKit: true,
		});
		expect(server).toContain('await import("./src/index.mjs")');
		expect(server).toContain(
			'typeof __AGENTOS_RIVETKIT_WASM_PATH__ === "string"',
		);
		expect(server).toContain('"@rivetkit/rivetkit-wasm/rivetkit_wasm_bg.wasm"');
		expect(server).toContain("Registry.prototype.start = function");
		expect(server).toContain("guestRegistry.handler(request)");
		expect(server).toContain('pathname.startsWith("/api/rivet")');
		expect(server).toContain("outgoing.flushHeaders?.();");
		expect(server).toContain('incoming.url === "/.agentos/ready"');
		expect(server.indexOf("outgoing.flushHeaders?.();")).toBeLessThan(
			server.lastIndexOf("for await (const chunk of response.body)"),
		);

		const plainServer = runnerSource({
			entrypoint: "src/index.mjs",
			release: "release",
			port: 3_080,
			maxRequestBytes: 1_024,
			maxResponseBytes: 1_024,
			usesRivetKit: false,
		});
		expect(plainServer).toContain('await import("./src/index.mjs")');
		expect(plainServer).not.toContain('import("rivetkit")');
		expect(plainServer).not.toContain("@rivetkit/rivetkit-wasm");
		expect(plainServer).not.toContain("createRequire");

		const staticSource = staticRunnerSource({
			root: "dist",
			release: "release",
			port: 3_080,
		});
		expect(staticSource).toContain('join(root, "index.html")');
		expect(staticSource).toContain('"dist"');
	});

	test("derives stable envoy versions and only exposes loopback engine ports", () => {
		expect(releaseEnvoyVersion(`ffffffff${"0".repeat(56)}`)).toBe(
			2_147_483_647,
		);
		expect(releaseEnvoyVersion(`00000000${"f".repeat(56)}`)).toBe(1);
		expect(runtimeLoopbackPort("http://127.0.0.1:6420")).toBe(6_420);
		expect(runtimeLoopbackPort("https://localhost")).toBe(443);
		expect(runtimeLoopbackPort("https://engine.example.com")).toBeUndefined();
	});
});
