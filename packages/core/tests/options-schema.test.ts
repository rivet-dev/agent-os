import { describe, expect, test } from "vitest";
import { AgentOs, agentOsOptionsSchema } from "../src/index.js";
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

	test("accepts the distinct WASM CPU fields and rejects removed aliases", () => {
		expect(
			agentOsOptionsSchema.safeParse({
				limits: {
					wasm: {
						activeCpuTimeLimitMs: 30_000,
						wallClockLimitMs: 45_000,
						deterministicFuel: 1_000_000,
					},
				},
			}).success,
		).toBe(true);
		expect(() =>
			agentOsOptionsSchema.parse({
				limits: { resources: { maxWasmFuel: 1 } },
			}),
		).toThrow(/maxWasmFuel/);
		expect(() =>
			agentOsOptionsSchema.parse({
				limits: { wasm: { runnerCpuTimeLimitMs: 1 } },
			}),
		).toThrow(/runnerCpuTimeLimitMs/);
	});

	test("accepts distinct per-process and per-VM WASM thread limits", () => {
		expect(
			agentOsOptionsSchema.safeParse({
				limits: {
					wasm: { maxThreads: 8, maxConcurrentThreads: 32 },
				},
			}).success,
		).toBe(true);
		expect(
			agentOsOptionsSchema.safeParse({
				limits: { wasm: { maxConcurrentThreads: 0 } },
			}).success,
		).toBe(false);
	});

	test("accepts only supported VM-wide standalone WASM backends", () => {
		for (const wasmBackend of ["v8", "wasmtime", "wasmtime-threads"] as const) {
			expect(agentOsOptionsSchema.safeParse({ wasmBackend }).success).toBe(
				true,
			);
		}
		expect(
			agentOsOptionsSchema.safeParse({ wasmBackend: "automatic" }).success,
		).toBe(false);
	});

	test("bounds and materializes Linux account records", () => {
		const exactPasswdRecord = {
			uid: 0,
			gid: 0,
			username: "u",
			homedir: "/",
			shell: "/",
			gecos: "x".repeat(4083),
		};
		expect(
			agentOsOptionsSchema.safeParse({ user: exactPasswdRecord }).success,
		).toBe(true);
		expect(
			agentOsOptionsSchema.safeParse({
				user: { ...exactPasswdRecord, gecos: "😀".repeat(1021) },
			}).success,
		).toBe(false);
		expect(
			agentOsOptionsSchema.safeParse({
				user: {
					uid: 0,
					gid: 0,
					username: "root",
					supplementaryGids: [44],
					groups: [{ gid: 99, name: "group44", members: [] }],
				},
			}).success,
		).toBe(false);
		expect(
			agentOsOptionsSchema.safeParse({
				user: {
					groups: [
						{
							gid: 7,
							name: "g",
							members: Array.from({ length: 257 }, (_, index) => `m${index}`),
						},
					],
				},
			}).success,
		).toBe(false);
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
	test("provider sandbox starts a client and owns disposal", async () => {
		let disposed = false;
		const client = {
			baseUrl: "http://127.0.0.1:1234",
			dispose: () => {
				disposed = true;
			},
		} as never;

		const options = await resolveSandboxOptions({
			sandbox: {
				provider: {
					start: async () => client,
				},
			},
		} as never);
		expect(options).not.toHaveProperty("sandbox");
		expect(options.mounts?.[0]?.path).toBe("/mnt/sandbox");
		expect(options.bindings?.[0]?.name).toBe("sandbox");

		for (const hook of getSandboxDisposeHooks(options)) {
			await hook();
		}
		expect(disposed).toBe(true);
	});

	test("advanced sandbox client leaves disposal manual by default", async () => {
		const client = { baseUrl: "http://127.0.0.1:1234" } as never;
		const options = await resolveSandboxOptions({
			sandbox: {
				client,
				mountPath: "/work",
			},
		} as never);
		expect(options.mounts?.[0]?.path).toBe("/work");
		expect(getSandboxDisposeHooks(options)).toHaveLength(0);
	});

	test("disposes a provider client when sandbox expansion fails", async () => {
		let disposed = 0;
		await expect(
			resolveSandboxOptions({
				sandbox: {
					provider: {
						start: async () => ({
							dispose: () => {
								disposed += 1;
							},
						}),
					},
				},
			} as never),
		).rejects.toThrow(/serializable baseUrl/);
		expect(disposed).toBe(1);
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
