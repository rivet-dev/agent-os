import { z } from "zod";
import type {
	MountConfig,
	MountConfigJsonObject,
	NativeMountPluginDescriptor,
} from "./agent-os.js";
import type { Binding, Bindings } from "./bindings.js";

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

function getSerializableClientConfig(
	client: AgentOsSandboxClient,
): Pick<SandboxMountPluginConfig, "baseUrl" | "token" | "headers"> {
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

async function normalizeSandboxInput(input: AgentOsSandboxInput): Promise<{
	options: ResolvedSandboxOptions;
	dispose?: SandboxDisposeHook;
}> {
	assertNoLegacySandboxOptions(input);
	if (isProviderOptions(input)) {
		if (typeof input.provider?.start !== "function") {
			throw new Error("sandbox.provider must expose a start() function.");
		}
		const client = await input.provider.start();
		return {
			options: { ...input, client },
			dispose: () => client.dispose?.(),
		};
	}
	if (!isClientOptions(input)) {
		throw new Error(
			"sandbox must be configured with either { provider } or { client }.",
		);
	}
	const dispose =
		typeof input.dispose === "function"
			? input.dispose
			: input.dispose === true
				? () => input.client.dispose?.()
				: undefined;
	return {
		options: input,
		dispose,
	};
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

	const normalizedSandbox = await normalizeSandboxInput(sandbox);
	try {
		const sandboxOptions = normalizedSandbox.options;
		const expanded = rest as Omit<T, "sandbox"> & {
			mounts?: MountConfig[];
			bindings?: Bindings[];
		};
		const mountPath = sandboxOptions.mountPath ?? "/mnt/sandbox";
		const mounts = [
			...(expanded.mounts ?? []),
			{
				path: mountPath,
				plugin: createSandboxFs(sandboxOptions),
				readOnly: sandboxOptions.readOnly,
			},
		];
		const bindings = [
			...(expanded.bindings ?? []),
			createSandboxBindings(sandboxOptions),
		];

		return attachSandboxDisposeHooks(
			{
				...expanded,
				mounts,
				bindings,
			},
			normalizedSandbox.dispose ? [normalizedSandbox.dispose] : [],
		);
	} catch (error) {
		if (!normalizedSandbox.dispose) throw error;
		try {
			await normalizedSandbox.dispose();
		} catch (disposeError) {
			throw new AggregateError(
				[error, disposeError],
				"Sandbox configuration and cleanup failed",
			);
		}
		throw error;
	}
}
