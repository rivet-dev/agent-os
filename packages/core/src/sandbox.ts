import { z } from "zod";
import type {
	MountConfig,
	MountConfigJsonObject,
	NativeMountPluginDescriptor,
} from "./agent-os.js";
import type { Binding, Bindings } from "./bindings.js";
import {
	createSandboxRelay,
	type SandboxRelayClientController,
} from "./sandbox-relay.js";

const DEFAULT_SANDBOX_IDLE_TIMEOUT_MS = 5 * 60_000;
const DEFAULT_SANDBOX_STARTUP_TIMEOUT_MS = 20_000;

export interface AgentOsSandboxProcessResult {
	stdout?: string;
	stderr?: string;
	exitCode?: number | null;
	timedOut?: boolean;
	durationMs?: number;
}

export interface AgentOsSandboxProcessInfo {
	id: string;
	command?: string;
	args?: string[];
	status?: string;
	exitCode?: number | null;
	pid?: number | null;
}

export interface AgentOsSandboxProcessLogs {
	entries: Array<{
		data: string;
		encoding?: "base64" | string;
		stream?: "stdout" | "stderr" | "combined" | string;
		timestampMs?: number;
	}>;
}

export interface AgentOsSandboxClient {
	dispose?(): Promise<void> | void;
	/**
	 * Optional authenticated raw transport used by the native filesystem relay.
	 * The path always starts with `/v1/` and includes its query string. Return the
	 * upstream Response unchanged, including non-success HTTP statuses.
	 */
	request?(path: string, init?: RequestInit): Promise<Response>;
	runProcess(options: {
		command: string;
		args?: string[];
		cwd?: string;
		env?: Record<string, string>;
		timeoutMs?: number;
	}): Promise<AgentOsSandboxProcessResult>;
	createProcess(options: {
		command: string;
		args?: string[];
		cwd?: string;
		env?: Record<string, string>;
	}): Promise<AgentOsSandboxProcessInfo>;
	listProcesses(): Promise<{ processes: AgentOsSandboxProcessInfo[] }>;
	stopProcess(id: string): Promise<AgentOsSandboxProcessInfo>;
	killProcess(id: string): Promise<AgentOsSandboxProcessInfo>;
	getProcessLogs(
		id: string,
		options?: { stream?: "stdout" | "stderr" | "combined"; tail?: number },
	): Promise<AgentOsSandboxProcessLogs>;
	sendProcessInput(
		id: string,
		input: { data: string; encoding: "base64" },
	): Promise<unknown>;
}

export interface AgentOsSandboxProvider {
	start(): Promise<AgentOsSandboxClient>;
}

export interface AgentOsSandboxCommonOptions {
	/** Mount path inside the agentOS VM. Defaults to "/mnt/sandbox". */
	mountPath?: string;
	/** Root path inside the external sandbox provider. Defaults to "/". */
	sandboxRoot?: string;
	/** Per-request timeout for sandbox-agent filesystem calls. */
	timeoutMs?: number;
	/** Maximum file size allowed for buffered pread/truncate fallbacks. */
	maxFullReadBytes?: number;
	/**
	 * Shut down an inactive provider sandbox after this duration. The next
	 * operation starts a new sandbox. Set to 0 to disable. Defaults to 5 minutes.
	 */
	idleTimeoutMs?: number;
	/** Maximum time to wait for a provider start. Set to 0 to disable. Defaults to 20 seconds. */
	startupTimeoutMs?: number;
	/** Maximum concurrent native filesystem relay requests. Defaults to 64. */
	maxRelayRequests?: number;
	/** Marks the VM mount read-only. Defaults to false. */
	readOnly?: boolean;
}

export interface AgentOsSandboxProviderOptions
	extends AgentOsSandboxCommonOptions {
	/** Provider used to start a Sandbox Agent client for this VM. */
	provider: AgentOsSandboxProvider;
}

export interface AgentOsSandboxClientOptions
	extends AgentOsSandboxCommonOptions {
	/** Externally provisioned Sandbox Agent-compatible client instance. */
	client: AgentOsSandboxClient;
	/**
	 * Advanced lifecycle control. Set true to dispose `client` with the VM, or
	 * provide a custom dispose hook. Defaults to false for the object form.
	 */
	dispose?: boolean | (() => void | Promise<void>);
}

export type AgentOsSandboxOptions =
	| AgentOsSandboxProviderOptions
	| AgentOsSandboxClientOptions;

export type AgentOsSandboxInput = AgentOsSandboxOptions;

const sandboxDisposeHooks = Symbol("agentos.sandboxDisposeHooks");

type SandboxDisposeHook = () => void | Promise<void>;

export type AgentOsSandboxExpandedOptions = {
	mounts?: MountConfig[];
	bindings?: Bindings[];
	[sandboxDisposeHooks]?: SandboxDisposeHook[];
};

type ResolvedSandboxOptions = AgentOsSandboxCommonOptions & {
	client: AgentOsSandboxClient;
};

export class SandboxStartupError extends Error {
	constructor(message: string, options?: ErrorOptions) {
		super(message, options);
		this.name = "SandboxStartupError";
	}
}

class SandboxClientController implements SandboxRelayClientController {
	readonly #provider?: AgentOsSandboxProvider;
	readonly #startupTimeoutMs: number;
	readonly #idleTimeoutMs: number;
	readonly #disposeClient?: SandboxDisposeHook;
	#current?: AgentOsSandboxClient;
	#startPromise?: Promise<AgentOsSandboxClient>;
	#stopPromise?: Promise<void>;
	#idleTimer?: NodeJS.Timeout;
	#activeOperations = 0;
	#disposed = false;

	constructor(options: {
		provider?: AgentOsSandboxProvider;
		client?: AgentOsSandboxClient;
		disposeClient?: SandboxDisposeHook;
		startupTimeoutMs?: number;
		idleTimeoutMs?: number;
	}) {
		this.#provider = options.provider;
		this.#current = options.client;
		this.#disposeClient = options.disposeClient;
		this.#startupTimeoutMs =
			options.startupTimeoutMs ?? DEFAULT_SANDBOX_STARTUP_TIMEOUT_MS;
		this.#idleTimeoutMs =
			options.idleTimeoutMs ?? DEFAULT_SANDBOX_IDLE_TIMEOUT_MS;
		for (const [name, value] of [
			["sandbox.startupTimeoutMs", this.#startupTimeoutMs],
			["sandbox.idleTimeoutMs", this.#idleTimeoutMs],
		] as const) {
			if (!Number.isSafeInteger(value) || value < 0) {
				throw new Error(`${name} must be a non-negative safe integer`);
			}
		}
	}

	async withClient<T>(
		operation: (client: AgentOsSandboxClient) => Promise<T>,
	): Promise<T> {
		if (this.#disposed) {
			throw new Error("agentOS VM sandbox has been disposed");
		}
		this.#clearIdleTimer();
		this.#activeOperations += 1;
		try {
			return await operation(await this.#getClient());
		} finally {
			this.#activeOperations -= 1;
			this.#scheduleIdleStop();
		}
	}

	async #getClient(): Promise<AgentOsSandboxClient> {
		if (this.#disposed) {
			throw new Error("agentOS VM sandbox has been disposed");
		}
		if (this.#current) return this.#current;
		if (!this.#provider) {
			throw new Error("Sandbox client is not available");
		}
		if (this.#stopPromise !== undefined) await this.#stopPromise;
		if (this.#current) return this.#current;
		if (this.#startPromise !== undefined) return await this.#startPromise;

		const startPromise = this.#startProvider();
		this.#startPromise = startPromise;
		try {
			return await startPromise;
		} finally {
			if (this.#startPromise === startPromise) this.#startPromise = undefined;
		}
	}

	async #startProvider(): Promise<AgentOsSandboxClient> {
		const provider = this.#provider;
		if (!provider) throw new Error("Sandbox provider is not configured");

		let abandoned = false;
		let timeout: NodeJS.Timeout | undefined;
		let providerResult: Promise<AgentOsSandboxClient>;
		try {
			providerResult = Promise.resolve(provider.start());
		} catch (error) {
			providerResult = Promise.reject(error);
		}
		const providerStart = providerResult.then(async (client) => {
			if (!client || typeof client !== "object") {
				throw new Error("sandbox.provider.start() did not return a client");
			}
			if (!abandoned && !this.#disposed) return client;
			try {
				await client.dispose?.();
			} catch (error) {
				console.error("agentOS late sandbox startup cleanup failed", error);
			}
			throw new SandboxStartupError(
				"Sandbox provider completed after its startup was cancelled",
			);
		});
		const timeoutPromise = new Promise<never>((_, reject) => {
			if (this.#startupTimeoutMs === 0) return;
			timeout = setTimeout(() => {
				abandoned = true;
				reject(
					new SandboxStartupError(
						`Sandbox provider startup exceeded sandbox.startupTimeoutMs=${this.#startupTimeoutMs}; raise sandbox.startupTimeoutMs to allow a longer startup`,
					),
				);
			}, this.#startupTimeoutMs);
		});
		try {
			const client = await Promise.race([providerStart, timeoutPromise]);
			if (this.#disposed) {
				abandoned = true;
				await client.dispose?.();
				throw new SandboxStartupError(
					"Sandbox provider completed after the agentOS VM was disposed",
				);
			}
			this.#current = client;
			return client;
		} catch (error) {
			abandoned = true;
			if (error instanceof SandboxStartupError) throw error;
			throw new SandboxStartupError(
				`Sandbox provider startup failed: ${error instanceof Error ? error.message : String(error)}`,
				{ cause: error },
			);
		} finally {
			if (timeout) clearTimeout(timeout);
		}
	}

	#clearIdleTimer(): void {
		if (!this.#idleTimer) return;
		clearTimeout(this.#idleTimer);
		this.#idleTimer = undefined;
	}

	#scheduleIdleStop(): void {
		this.#clearIdleTimer();
		if (
			!this.#provider ||
			this.#disposed ||
			this.#idleTimeoutMs === 0 ||
			this.#activeOperations !== 0 ||
			!this.#current
		) {
			return;
		}
		this.#idleTimer = setTimeout(() => {
			this.#idleTimer = undefined;
			void this.#stopIdleClient().catch((error) => {
				console.error("agentOS idle sandbox shutdown failed", error);
			});
		}, this.#idleTimeoutMs);
		this.#idleTimer.unref();
	}

	async #stopIdleClient(): Promise<void> {
		if (
			this.#disposed ||
			this.#activeOperations !== 0 ||
			!this.#current ||
			this.#stopPromise !== undefined
		) {
			return;
		}
		const client = this.#current;
		this.#current = undefined;
		const stopPromise = Promise.resolve(client.dispose?.()).then(
			() => undefined,
		);
		this.#stopPromise = stopPromise;
		try {
			await stopPromise;
		} finally {
			if (this.#stopPromise === stopPromise) this.#stopPromise = undefined;
		}
	}

	async dispose(): Promise<void> {
		if (this.#disposed) return;
		this.#disposed = true;
		this.#clearIdleTimer();
		const errors: unknown[] = [];
		try {
			await this.#startPromise;
		} catch (error) {
			errors.push(error);
		}
		try {
			await this.#stopPromise;
		} catch (error) {
			errors.push(error);
		}
		const client = this.#current;
		this.#current = undefined;
		if (client) {
			try {
				if (this.#provider) await client.dispose?.();
				else await this.#disposeClient?.();
			} catch (error) {
				errors.push(error);
			}
		}
		if (errors.length === 1) throw errors[0];
		if (errors.length > 1) {
			throw new AggregateError(errors, "agentOS sandbox disposal failed");
		}
	}
}

function createControllerClient(
	controller: SandboxClientController,
): AgentOsSandboxClient {
	return {
		runProcess: (options) =>
			controller.withClient((client) => client.runProcess(options)),
		createProcess: (options) =>
			controller.withClient((client) => client.createProcess(options)),
		listProcesses: () =>
			controller.withClient((client) => client.listProcesses()),
		stopProcess: (id) =>
			controller.withClient((client) => client.stopProcess(id)),
		killProcess: (id) =>
			controller.withClient((client) => client.killProcess(id)),
		getProcessLogs: (id, options) =>
			controller.withClient((client) => client.getProcessLogs(id, options)),
		sendProcessInput: (id, input) =>
			controller.withClient((client) => client.sendProcessInput(id, input)),
	};
}

export type SandboxMountPluginConfig = MountConfigJsonObject & {
	baseUrl: string;
	token?: string;
	headers?: Record<string, string>;
	basePath?: string;
	timeoutMs?: number;
	maxFullReadBytes?: number;
};

interface SerializableSandboxClient {
	baseUrl?: string;
	token?: string;
	defaultHeaders?: RequestInit["headers"];
}

function binding<INPUT, OUTPUT>(
	def: Binding<INPUT, OUTPUT>,
): Binding<INPUT, OUTPUT> {
	return def;
}

function normalizeHeaders(
	headers: RequestInit["headers"] | undefined,
): Record<string, string> | undefined {
	if (!headers) {
		return undefined;
	}

	if (headers instanceof Headers) {
		return Object.fromEntries(headers.entries());
	}

	if (Array.isArray(headers)) {
		return Object.fromEntries(headers as Iterable<readonly [string, string]>);
	}

	return Object.fromEntries(
		Object.entries(headers).map(([name, value]) => [name, String(value)]),
	);
}

function getSerializableClientConfig(client: AgentOsSandboxClient): {
	baseUrl: string;
	token?: string;
	headers?: Record<string, string>;
} {
	const serializable = client as unknown as SerializableSandboxClient;
	const baseUrl = serializable.baseUrl?.trim().replace(/\/+$/, "");
	if (!baseUrl) {
		throw new Error(
			"Sandbox client does not expose a serializable baseUrl; connect with a standard SandboxAgent client instance",
		);
	}

	return {
		baseUrl,
		...(serializable.token ? { token: serializable.token } : {}),
		...(serializable.defaultHeaders
			? { headers: normalizeHeaders(serializable.defaultHeaders) }
			: {}),
	};
}

export function createSandboxFs(
	input: ResolvedSandboxOptions | AgentOsSandboxClientOptions,
): NativeMountPluginDescriptor<SandboxMountPluginConfig> {
	const options = input;
	return {
		id: "sandbox_agent",
		config: {
			...getSerializableClientConfig(options.client),
			...(options.sandboxRoot ? { basePath: options.sandboxRoot } : {}),
			...(options.timeoutMs != null ? { timeoutMs: options.timeoutMs } : {}),
			...(options.maxFullReadBytes != null
				? { maxFullReadBytes: options.maxFullReadBytes }
				: {}),
		},
	};
}

export function createSandboxBindings(
	input: ResolvedSandboxOptions | AgentOsSandboxClientOptions,
): Bindings {
	const options = input;
	const { client } = options;

	return {
		name: "sandbox",
		description:
			"Execute commands and manage processes in a remote sandbox environment.",
		bindings: {
			"run-command": binding({
				description:
					"Run a command synchronously in the sandbox and return its stdout, stderr, and exit code.",
				inputSchema: z.object({
					command: z
						.string()
						.describe("The command to execute (e.g. 'ls', 'python3')."),
					args: z.array(z.string()).optional(),
					cwd: z.string().optional(),
					env: z.record(z.string(), z.string()).optional(),
					timeoutMs: z.number().optional(),
				}),
				timeout: 120_000,
				execute: async (input) => {
					const result = await client.runProcess(input);
					return {
						stdout: result.stdout,
						stderr: result.stderr,
						exitCode: result.exitCode,
						timedOut: result.timedOut,
						durationMs: result.durationMs,
					};
				},
			}),

			"create-process": binding({
				description:
					"Start a long-running background process in the sandbox. Returns a process ID for later management.",
				inputSchema: z.object({
					command: z.string(),
					args: z.array(z.string()).optional(),
					cwd: z.string().optional(),
					env: z.record(z.string(), z.string()).optional(),
				}),
				execute: async (input) => {
					const proc = await client.createProcess(input);
					return {
						id: proc.id,
						command: proc.command,
						args: proc.args,
						status: proc.status,
						pid: proc.pid,
					};
				},
			}),

			"list-processes": binding({
				description: "List all processes running in the sandbox.",
				inputSchema: z.object({}),
				execute: async () => {
					const result = await client.listProcesses();
					return {
						processes: result.processes.map((p) => ({
							id: p.id,
							command: p.command,
							args: p.args,
							status: p.status,
							exitCode: p.exitCode,
							pid: p.pid,
						})),
					};
				},
			}),

			"stop-process": binding({
				description: "Gracefully stop a running process in the sandbox.",
				inputSchema: z.object({ id: z.string() }),
				execute: async (input) => {
					const proc = await client.stopProcess(input.id);
					return {
						id: proc.id,
						status: proc.status,
						exitCode: proc.exitCode,
					};
				},
			}),

			"kill-process": binding({
				description: "Forcefully kill a running process in the sandbox.",
				inputSchema: z.object({ id: z.string() }),
				execute: async (input) => {
					const proc = await client.killProcess(input.id);
					return {
						id: proc.id,
						status: proc.status,
						exitCode: proc.exitCode,
					};
				},
			}),

			"get-process-logs": binding({
				description: "Get stdout/stderr logs from a sandbox process.",
				inputSchema: z.object({
					id: z.string(),
					stream: z.enum(["stdout", "stderr", "combined"]).optional(),
					tail: z.number().optional(),
				}),
				execute: async (input) => {
					const result = await client.getProcessLogs(input.id, {
						stream: input.stream,
						tail: input.tail,
					});
					return {
						logs: result.entries.map((e) => {
							const data =
								e.encoding === "base64"
									? Buffer.from(e.data, "base64").toString("utf-8")
									: e.data;
							return {
								data,
								stream: e.stream,
								timestampMs: e.timestampMs,
							};
						}),
					};
				},
			}),

			"send-input": binding({
				description:
					"Send text input to an interactive sandbox process via stdin.",
				inputSchema: z.object({
					id: z.string(),
					data: z.string(),
				}),
				execute: async (input) => {
					await client.sendProcessInput(input.id, {
						data: Buffer.from(input.data, "utf-8").toString("base64"),
						encoding: "base64",
					});
					return { sent: true };
				},
			}),
		},
	};
}

function isProviderOptions(
	input: AgentOsSandboxInput,
): input is AgentOsSandboxProviderOptions {
	return "provider" in input;
}

function isClientOptions(
	input: AgentOsSandboxInput,
): input is AgentOsSandboxClientOptions {
	return "client" in input;
}

function assertNoLegacySandboxOptions(input: AgentOsSandboxInput): void {
	const legacyKeys = ["mount", "bindings", "path", "basePath"] as const;
	for (const key of legacyKeys) {
		if (key in input) {
			const replacement =
				key === "path" || key === "basePath" ? "sandboxRoot" : undefined;
			throw new Error(
				replacement
					? `sandbox.${key} has been removed; use sandbox.${replacement} instead.`
					: `sandbox.${key} has been removed; sandbox mounts and bindings are always enabled.`,
			);
		}
	}
}

function createSandboxController(
	input: AgentOsSandboxInput,
): SandboxClientController {
	assertNoLegacySandboxOptions(input);
	if (isProviderOptions(input)) {
		if (typeof input.provider?.start !== "function") {
			throw new Error("sandbox.provider must expose a start() function.");
		}
		return new SandboxClientController({
			provider: input.provider,
			idleTimeoutMs: input.idleTimeoutMs,
			startupTimeoutMs: input.startupTimeoutMs,
		});
	}
	if (!isClientOptions(input)) {
		throw new Error(
			"sandbox must be configured with either { provider } or { client }.",
		);
	}
	if (!input.client || typeof input.client !== "object") {
		throw new Error("sandbox.client must be an object.");
	}
	const disposeClient =
		typeof input.dispose === "function"
			? input.dispose
			: input.dispose === true
				? () => input.client.dispose?.()
				: undefined;
	return new SandboxClientController({
		client: input.client,
		disposeClient,
		idleTimeoutMs: input.idleTimeoutMs ?? 0,
		startupTimeoutMs: input.startupTimeoutMs,
	});
}

function attachSandboxDisposeHooks<T extends object>(
	options: T,
	hooks: SandboxDisposeHook[],
): T {
	if (hooks.length === 0) {
		return options;
	}
	Object.defineProperty(options, sandboxDisposeHooks, {
		value: hooks,
		enumerable: false,
		configurable: false,
		writable: false,
	});
	return options;
}

export function getSandboxDisposeHooks(
	options: object | undefined,
): SandboxDisposeHook[] {
	return options
		? ((options as AgentOsSandboxExpandedOptions)[sandboxDisposeHooks] ?? [])
		: [];
}

export async function resolveSandboxOptions<
	T extends { sandbox?: AgentOsSandboxInput },
>(
	options: T,
): Promise<
	Omit<T, "sandbox"> & {
		mounts?: MountConfig[];
		bindings?: Bindings[];
	}
> {
	const { sandbox, ...rest } = options;
	if (!sandbox) {
		return rest;
	}

	const controller = createSandboxController(sandbox);
	let relay: Awaited<ReturnType<typeof createSandboxRelay>> | undefined;
	try {
		relay = await createSandboxRelay({
			controller,
			maxConcurrentRequests: sandbox.maxRelayRequests,
		});
		const expanded = rest as Omit<T, "sandbox"> & {
			mounts?: MountConfig[];
			bindings?: Bindings[];
		};
		const mountPath = sandbox.mountPath ?? "/mnt/sandbox";
		const plugin: NativeMountPluginDescriptor<SandboxMountPluginConfig> = {
			id: "sandbox_agent",
			config: {
				baseUrl: relay.baseUrl,
				token: relay.token,
				...(sandbox.sandboxRoot ? { basePath: sandbox.sandboxRoot } : {}),
				...(sandbox.timeoutMs != null ? { timeoutMs: sandbox.timeoutMs } : {}),
				...(sandbox.maxFullReadBytes != null
					? { maxFullReadBytes: sandbox.maxFullReadBytes }
					: {}),
			},
		};
		const mounts = [
			...(expanded.mounts ?? []),
			{
				path: mountPath,
				plugin,
				readOnly: sandbox.readOnly,
			},
		];
		const bindings = [
			...(expanded.bindings ?? []),
			createSandboxBindings({
				...sandbox,
				client: createControllerClient(controller),
			}),
		];

		return attachSandboxDisposeHooks(
			{
				...expanded,
				mounts,
				bindings,
			},
			[
				async () => {
					const results = await Promise.allSettled([
						relay?.dispose(),
						controller.dispose(),
					]);
					const errors = results.flatMap((result) =>
						result.status === "rejected" ? [result.reason] : [],
					);
					if (errors.length === 1) throw errors[0];
					if (errors.length > 1) {
						throw new AggregateError(
							errors,
							"agentOS sandbox relay cleanup failed",
						);
					}
				},
			],
		);
	} catch (error) {
		const cleanupResults = await Promise.allSettled([
			relay?.dispose(),
			controller.dispose(),
		]);
		const cleanupErrors = cleanupResults.flatMap((result) =>
			result.status === "rejected" ? [result.reason] : [],
		);
		if (cleanupErrors.length > 0) {
			throw new AggregateError(
				[error, ...cleanupErrors],
				"Sandbox configuration and cleanup failed",
			);
		}
		throw error;
	}
}
