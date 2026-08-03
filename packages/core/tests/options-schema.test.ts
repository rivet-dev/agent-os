import { describe, expect, test } from "vitest";
import {
	AgentOs,
	agentOsOptionsSchema,
	SandboxStartupError,
} from "../src/index.js";
import {
	getSandboxDisposeHooks,
	resolveSandboxOptions,
} from "../src/sandbox.js";

describe("AgentOsOptions validation", () => {
	test("accepts the path-only actor runtime socket descriptor", () => {
		expect(
			agentOsOptionsSchema.safeParse({
				database: {
					type: "actor_uds",
					path: "/tmp/actor-runtime.sock",
				},
			}).success,
		).toBe(true);
	});

	test("accepts a declarative sidecar-native root", () => {
		expect(
			agentOsOptionsSchema.safeParse({
				rootFilesystem: {
					type: "native",
					plugin: {
						id: "chunked_actor_sqlite",
						config: { path: "/tmp/actor.sock" },
					},
				},
			}).success,
		).toBe(true);
	});

	test("rejects unknown top-level options before booting a VM", async () => {
		await expect(
			AgentOs.create({
				onSessionEvent: () => {},
			} as never),
		).rejects.toThrow(/onSessionEvent/);
	});

	test("rejects unknown nested permission fields", () => {
		expect(() =>
			agentOsOptionsSchema.parse({
				permissions: {
					filesystem: "allow",
				},
			}),
		).toThrow(/filesystem/);
	});

	test("rejects create option factories on the one-shot core constructor", () => {
		expect(() =>
			agentOsOptionsSchema.parse({
				createOptions: () => ({}),
			}),
		).toThrow(/createOptions/);
	});

	test("accepts bindings as the public name for host binding collections", () => {
		expect(
			agentOsOptionsSchema.safeParse({
				bindings: [
					{
						name: "weather",
						description: "Weather bindings",
						bindings: {},
					},
				],
			}).success,
		).toBe(true);
	});

	test("accepts a sandbox provider as a public VM option", () => {
		expect(
			agentOsOptionsSchema.safeParse({
				sandbox: { provider: { start: async () => ({}) } },
			}).success,
		).toBe(true);
	});

	test("uses the sidecar wire name for the per-VM binding limit", () => {
		expect(
			agentOsOptionsSchema.safeParse({
				limits: { bindings: { maxRegisteredBindingsPerVm: 256 } },
			}).success,
		).toBe(true);
		expect(
			agentOsOptionsSchema.safeParse({
				limits: { bindings: { maxRegisteredCollectionsPerVm: 256 } },
			}).success,
		).toBe(false);
	});

	test("validates execution retention limits as positive safe integers", () => {
		expect(
			agentOsOptionsSchema.safeParse({
				limits: {
					execution: {
						completedTtlMs: 300_000,
						maxCompletedExecutions: 1_024,
						liveExecutionWarningThreshold: 64,
					},
				},
			}).success,
		).toBe(true);
		for (const value of [0, -1, 1.5, Number.MAX_SAFE_INTEGER + 1]) {
			expect(
				agentOsOptionsSchema.safeParse({
					limits: { execution: { completedTtlMs: value } },
				}).success,
			).toBe(false);
		}
	});
	test("provider sandbox starts lazily and owns disposal", async () => {
		let started = 0;
		let disposed = false;
		const client = {
			baseUrl: "http://127.0.0.1:1234",
			listProcesses: async () => ({ processes: [] }),
			dispose: () => {
				disposed = true;
			},
		} as never;

		const options = await resolveSandboxOptions({
			sandbox: {
				provider: {
					start: async () => {
						started += 1;
						return client;
					},
				},
			},
		} as never);
		expect(options).not.toHaveProperty("sandbox");
		expect(options.mounts?.[0]?.path).toBe("/mnt/sandbox");
		expect(options.bindings?.[0]?.name).toBe("sandbox");
		expect(started).toBe(0);

		await options.bindings?.[0]?.bindings["list-processes"].execute({});
		expect(started).toBe(1);
		for (const hook of getSandboxDisposeHooks(options)) {
			await hook();
		}
		expect(disposed).toBe(true);
	});

	test("advanced sandbox client leaves client disposal manual by default", async () => {
		let disposed = false;
		const client = {
			baseUrl: "http://127.0.0.1:1234",
			dispose: () => {
				disposed = true;
			},
		} as never;
		const options = await resolveSandboxOptions({
			sandbox: {
				client,
				mountPath: "/work",
			},
		} as never);
		expect(options.mounts?.[0]?.path).toBe("/work");
		const mount = options.mounts?.[0];
		if (!mount || !("plugin" in mount)) {
			throw new Error("sandbox mount config is missing");
		}
		expect(mount.plugin.config.baseUrl).not.toBe("http://127.0.0.1:1234");
		expect(mount.plugin.config.token).toEqual(expect.any(String));
		expect(getSandboxDisposeHooks(options)).toHaveLength(1);
		for (const hook of getSandboxDisposeHooks(options)) await hook();
		expect(disposed).toBe(false);
	});

	test("advanced sandbox client can transfer disposal ownership", async () => {
		let disposed = 0;
		const options = await resolveSandboxOptions({
			sandbox: {
				client: {
					baseUrl: "http://127.0.0.1:1234",
					dispose: async () => {
						disposed += 1;
					},
				} as never,
				dispose: true,
			},
		} as never);
		for (const hook of getSandboxDisposeHooks(options)) await hook();
		expect(disposed).toBe(1);
	});

	test("shares one provider startup across mount and binding calls", async () => {
		let started = 0;
		let releaseStart!: () => void;
		const startGate = new Promise<void>((resolve) => {
			releaseStart = resolve;
		});
		const options = await resolveSandboxOptions({
			sandbox: {
				provider: {
					start: async () => {
						started += 1;
						await startGate;
						return {
							request: async () =>
								new Response(
									JSON.stringify({
										path: "/",
										entryType: "directory",
										size: 0,
									}),
									{ headers: { "content-type": "application/json" } },
								),
							listProcesses: async () => ({ processes: [] }),
							dispose: async () => {},
						} as never;
					},
				},
			},
		} as never);
		const execute = options.bindings?.[0]?.bindings["list-processes"].execute;
		if (!execute) throw new Error("sandbox list-processes binding is missing");
		const mount = options.mounts?.[0];
		if (!mount || !("plugin" in mount)) {
			throw new Error("sandbox mount config is missing");
		}
		const mountConfig = mount.plugin.config;
		const bindingCall = execute({});
		const mountCall = fetch(
			`${String(mountConfig.baseUrl)}/v1/fs/stat?path=%2F`,
			{
				headers: {
					authorization: `Bearer ${String(mountConfig.token)}`,
				},
			},
		);
		await new Promise((resolve) => setTimeout(resolve, 10));
		expect(started).toBe(1);
		releaseStart();
		const [, mountResponse] = await Promise.all([bindingCall, mountCall]);
		expect(mountResponse.status).toBe(200);
		await mountResponse.arrayBuffer();
		expect(started).toBe(1);
		for (const hook of getSandboxDisposeHooks(options)) await hook();
	});

	test("reports startup failures and retries on the next operation", async () => {
		let started = 0;
		const options = await resolveSandboxOptions({
			sandbox: {
				provider: {
					start: async () => {
						started += 1;
						if (started === 1) throw new Error("provider unavailable");
						return {
							listProcesses: async () => ({ processes: [] }),
							dispose: async () => {},
						} as never;
					},
				},
			},
		} as never);
		const execute = options.bindings?.[0]?.bindings["list-processes"].execute;
		if (!execute) throw new Error("sandbox list-processes binding is missing");
		await expect(execute({})).rejects.toEqual(
			expect.objectContaining({
				name: SandboxStartupError.name,
				message: expect.stringContaining("provider unavailable"),
			}),
		);
		await expect(execute({})).resolves.toEqual({ processes: [] });
		expect(started).toBe(2);
		for (const hook of getSandboxDisposeHooks(options)) await hook();
	});

	test("restarts an idle provider without changing the mount endpoint", async () => {
		let started = 0;
		let disposed = 0;
		const options = await resolveSandboxOptions({
			sandbox: {
				idleTimeoutMs: 10,
				provider: {
					start: async () => {
						started += 1;
						return {
							listProcesses: async () => ({ processes: [] }),
							dispose: async () => {
								disposed += 1;
							},
						} as never;
					},
				},
			},
		} as never);
		const mount = options.mounts?.[0];
		if (!mount || !("plugin" in mount)) {
			throw new Error("sandbox mount config is missing");
		}
		const relayUrl = mount.plugin.config.baseUrl;
		const execute = options.bindings?.[0]?.bindings["list-processes"].execute;
		if (!execute) throw new Error("sandbox list-processes binding is missing");
		await execute({});
		for (let attempt = 0; attempt < 50 && disposed === 0; attempt++) {
			await new Promise((resolve) => setTimeout(resolve, 5));
		}
		expect(disposed).toBe(1);
		await execute({});
		expect(started).toBe(2);
		expect(mount.plugin.config.baseUrl).toBe(relayUrl);
		for (const hook of getSandboxDisposeHooks(options)) await hook();
	});

	test("validates sandbox relay and lifecycle limits", async () => {
		for (const [field, value] of [
			["maxRelayRequests", 0],
			["idleTimeoutMs", -1],
			["startupTimeoutMs", 1.5],
		] as const) {
			await expect(
				resolveSandboxOptions({
					sandbox: {
						client: { baseUrl: "http://127.0.0.1:1234" } as never,
						[field]: value,
					},
				} as never),
			).rejects.toThrow(new RegExp(`sandbox\\.${field}`));
		}
	});

	test("does not start a provider when VM option validation fails", async () => {
		let started = 0;
		let disposed = 0;
		await expect(
			AgentOs.create({
				defaultSoftware: false,
				sandbox: {
					provider: {
						start: async () => {
							started += 1;
							return {
								baseUrl: "http://127.0.0.1:1234",
								dispose: () => {
									disposed += 1;
								},
							} as never;
						},
					},
				},
				bindings: [
					{
						name: "INVALID",
						description: "Invalid binding collection",
						bindings: {},
					},
				],
			}),
		).rejects.toThrow(/must be lowercase alphanumeric/);
		expect(started).toBe(0);
		expect(disposed).toBe(0);
	});

	test("rejects removed sandbox mount and binding toggles", async () => {
		const client = { baseUrl: "http://127.0.0.1:1234" } as never;
		await expect(
			resolveSandboxOptions({
				sandbox: {
					client,
					mount: false,
				} as never,
			} as never),
		).rejects.toThrow(/sandbox\.mount has been removed/);

		await expect(
			resolveSandboxOptions({
				sandbox: {
					client,
					bindings: false,
				} as never,
			} as never),
		).rejects.toThrow(/sandbox\.bindings has been removed/);
	});

	test("rejects old sandbox path option names", async () => {
		const client = { baseUrl: "http://127.0.0.1:1234" } as never;
		await expect(
			resolveSandboxOptions({
				sandbox: {
					client,
					basePath: "/app",
				} as never,
			} as never),
		).rejects.toThrow(/sandbox\.basePath has been removed/);
	});
});
