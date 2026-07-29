import { createHash } from "node:crypto";
import { isAbsolute, posix } from "node:path";
import type { AgentOs } from "@rivet-dev/agentos-core";
import type {
	SandboxBackend,
	SandboxBackendCreateInput,
	SandboxBackendHandle,
	SandboxCommandResult,
	SandboxProcess,
	SandboxReadTextFileOptions,
	SandboxSession,
} from "eve/sandbox";

const ACTOR_BACKEND_NAME = "agentos-rivet-actor-v1";
const CORE_BACKEND_NAME = "agentos-core-v1";
const PROCESS_STOP_TIMEOUT_MS = 5_000;
const WORKSPACE_ROOT = "/workspace";
const AGENTOS_HOME = "/home/agentos";
const METADATA_VERSION = 1;

interface AgentOSActorConnection {
	readonly ready: PromiseLike<void>;
	on(event: string, listener: (payload: unknown) => void): unknown;
	off?(event: string, listener: (payload: unknown) => void): void;
	removeListener?(event: string, listener: (payload: unknown) => void): void;
	dispose(): Promise<void>;
	spawn(
		command: string,
		args: string[],
		options: { cwd: string; env?: Record<string, string> },
	): Promise<{ pid: number }>;
	waitProcess(pid: number): Promise<number>;
	killProcess(pid: number): Promise<void>;
	readFile(path: string): Promise<unknown>;
	writeFile(path: string, content: Uint8Array): Promise<void>;
	exists(path: string): Promise<boolean>;
	mkdir(path: string, options: { recursive: boolean }): Promise<void>;
	remove(path: string, options: { recursive?: boolean }): Promise<void>;
}

interface AgentOSActorAccessor {
	getOrCreate(key: string[]): { connect(): AgentOSActorConnection };
}

type AgentOSActorClient = Record<string, AgentOSActorAccessor | undefined>;

export interface AgentOSRegistry {
	startAndWait(): Promise<void>;
	parseConfig(): {
		endpoint?: string;
		namespace: string;
		token?: string;
		headers?: Record<string, string>;
		envoy: { poolName: string };
	};
}

export interface AgentOSBackendOptions {
	/** Registry key of an actor created with `agentOS()`. */
	actor: string;
	/** Application registry containing the selected agentOS actor. */
	registry: AgentOSRegistry;
	/** Advanced: an existing client for the same registry. */
	client?: object;
}

export interface AgentOSCoreCreateInput {
	/** Stable Eve session identity. Use it to select caller-owned persistence. */
	sessionKey: string;
}

export interface AgentOSCoreBackendOptions {
	/** Creates one caller-configured agentOS Core VM for an Eve session. */
	create(input: AgentOSCoreCreateInput): AgentOs | Promise<AgentOs>;
}

export class AgentOSActorConfigurationError extends Error {
	readonly code = "agentos_actor_configuration";

	constructor(
		readonly actor: string,
		message: string,
		options?: ErrorOptions,
	) {
		super(message, options);
		this.name = "AgentOSActorConfigurationError";
	}
}

export class AgentOSTemplateUnsupportedError extends Error {
	readonly code = "agentos_template_unsupported";

	constructor(readonly templateKey: string) {
		super(
			`agentOS cannot prewarm Eve template ${JSON.stringify(templateKey)} yet; initialize /workspace through the configured VM or filesystem mount`,
		);
		this.name = "AgentOSTemplateUnsupportedError";
	}
}

/**
 * Uses an existing `agentOS()` actor as Eve's sandbox. The actor owns its VM,
 * filesystem configuration, mounts, persistence, limits, and permissions.
 */
export function agentOSBackend(options: AgentOSBackendOptions): SandboxBackend {
	if (!options || typeof options.actor !== "string" || !options.actor.trim()) {
		throw new TypeError("agentOSBackend requires the registry actor name");
	}
	if (
		!options.registry ||
		typeof options.registry.startAndWait !== "function" ||
		typeof options.registry.parseConfig !== "function"
	) {
		throw new TypeError("agentOSBackend requires the application registry");
	}
	const actor = options.actor.trim();
	const handles = new Map<string, Promise<SandboxBackendHandle>>();

	return {
		name: ACTOR_BACKEND_NAME,
		async prewarm(input) {
			// TODO: Support prebuilt workspace state using actor forking once available.
			throw new AgentOSTemplateUnsupportedError(input.templateKey);
		},
		async create(input) {
			if (input.templateKey !== null) {
				throw new AgentOSTemplateUnsupportedError(input.templateKey);
			}
			validateActorMetadata(input, actor);
			const key = actorKey(input.sessionKey);
			return cachedHandle({
				backendName: ACTOR_BACKEND_NAME,
				cacheKey: key.join("\0"),
				connect: () => connectActor(options, actor, key),
				handles,
				metadata: { version: METADATA_VERSION, actor, key },
				networkPolicyOwner: `actor ${JSON.stringify(actor)}; configure permissions on agentOS({...})`,
				sessionKey: input.sessionKey,
			});
		},
	};
}

/** Uses a caller-created standalone agentOS Core VM as Eve's sandbox. */
export function agentOSCoreBackend(
	options: AgentOSCoreBackendOptions,
): SandboxBackend {
	if (!options || typeof options.create !== "function") {
		throw new TypeError("agentOSCoreBackend requires a create factory");
	}
	const handles = new Map<string, Promise<SandboxBackendHandle>>();

	return {
		name: CORE_BACKEND_NAME,
		async prewarm(input) {
			throw new AgentOSTemplateUnsupportedError(input.templateKey);
		},
		async create(input) {
			if (input.templateKey !== null) {
				throw new AgentOSTemplateUnsupportedError(input.templateKey);
			}
			validateCoreMetadata(input);
			return cachedHandle({
				backendName: CORE_BACKEND_NAME,
				cacheKey: input.sessionKey,
				connect: async () =>
					coreConnection(
						await options.create({ sessionKey: input.sessionKey }),
					),
				handles,
				metadata: { version: METADATA_VERSION },
				networkPolicyOwner:
					"the caller-created agentOS Core VM; configure permissions in create()",
				sessionKey: input.sessionKey,
			});
		},
	};
}

function actorKey(sessionKey: string): string[] {
	return ["eve", "session", stableId(sessionKey)];
}

const registryClients = new WeakMap<object, Promise<AgentOSActorClient>>();

async function actorClient(
	options: AgentOSBackendOptions,
): Promise<AgentOSActorClient> {
	await options.registry.startAndWait();
	if (options.client) return options.client as AgentOSActorClient;

	const registryKey = options.registry as object;
	let pending = registryClients.get(registryKey);
	if (!pending) {
		pending = (async () => {
			const config = options.registry.parseConfig();
			const { createClient } = await import("@rivet-dev/agentos/client");
			return createClient<never>({
				endpoint: config.endpoint,
				namespace: config.namespace,
				poolName: config.envoy.poolName,
				token: config.token,
				headers: config.headers,
				disableMetadataLookup: true,
			} as never) as unknown as AgentOSActorClient;
		})().catch((error) => {
			registryClients.delete(registryKey);
			throw error;
		});
		registryClients.set(registryKey, pending);
	}
	return pending;
}

async function connectActor(
	options: AgentOSBackendOptions,
	actor: string,
	key: string[],
): Promise<AgentOSActorConnection> {
	try {
		const accessor = (await actorClient(options))[actor];
		if (!accessor || typeof accessor.getOrCreate !== "function") {
			throw new Error(`registry has no actor named ${JSON.stringify(actor)}`);
		}
		const connection = accessor.getOrCreate(key).connect();
		await connection.ready;
		const methods = connection as unknown as Record<string, unknown>;
		for (const method of [
			"on",
			"dispose",
			"spawn",
			"waitProcess",
			"killProcess",
			"readFile",
			"writeFile",
			"exists",
			"mkdir",
			"remove",
		]) {
			if (typeof methods[method] !== "function") {
				await connection.dispose();
				throw new Error("selected actor is not an @rivet-dev/agentos actor");
			}
		}
		return connection;
	} catch (cause) {
		if (cause instanceof AgentOSActorConfigurationError) throw cause;
		throw new AgentOSActorConfigurationError(
			actor,
			`agentOS actor ${JSON.stringify(actor)} could not be used; register an agentOS() actor under that exact setup({ use }) key: ${cause instanceof Error ? cause.message : String(cause)}`,
			{ cause },
		);
	}
}

function validateActorMetadata(
	input: SandboxBackendCreateInput,
	actor: string,
): void {
	if (input.existingMetadata === undefined) return;
	const expectedKey = actorKey(input.sessionKey);
	const metadata = input.existingMetadata;
	if (
		metadata.version !== METADATA_VERSION ||
		metadata.actor !== actor ||
		!Array.isArray(metadata.key) ||
		metadata.key.length !== expectedKey.length ||
		metadata.key.some((value, index) => value !== expectedKey[index])
	) {
		throw new Error(
			`corrupt or incompatible agentOS actor reconnect metadata for ${JSON.stringify(actor)}`,
		);
	}
}

function validateCoreMetadata(input: SandboxBackendCreateInput): void {
	if (input.existingMetadata === undefined) return;
	if (input.existingMetadata.version !== METADATA_VERSION) {
		throw new Error("corrupt or incompatible agentOS Core reconnect metadata");
	}
}

interface CachedHandleInput {
	backendName: string;
	cacheKey: string;
	connect(): Promise<AgentOSActorConnection>;
	handles: Map<string, Promise<SandboxBackendHandle>>;
	metadata: Record<string, unknown>;
	networkPolicyOwner: string;
	sessionKey: string;
}

async function cachedHandle(
	input: CachedHandleInput,
): Promise<SandboxBackendHandle> {
	const existing = input.handles.get(input.cacheKey);
	if (existing) return existing;

	let pending!: Promise<SandboxBackendHandle>;
	pending = (async () => {
		const connection = await input.connect();
		let actorSession: ReturnType<typeof createActorSession>;
		try {
			actorSession = createActorSession(
				connection,
				input.sessionKey,
				input.networkPolicyOwner,
			);
		} catch (error) {
			try {
				await connection.dispose();
			} catch (disposeError) {
				throw new AggregateError(
					[error, disposeError],
					"agentOS Eve session setup and connection cleanup both failed",
				);
			}
			throw error;
		}
		let shutdownPromise: Promise<void> | undefined;
		return {
			session: actorSession.session,
			useSessionFn: async () => actorSession.session,
			async captureState() {
				return {
					backendName: input.backendName,
					metadata: input.metadata,
					sessionKey: input.sessionKey,
				};
			},
			async shutdown() {
				if (input.handles.get(input.cacheKey) === pending) {
					input.handles.delete(input.cacheKey);
				}
				shutdownPromise ??= shutdownSession(actorSession, connection);
				await shutdownPromise;
			},
		};
	})();
	input.handles.set(input.cacheKey, pending);
	try {
		return await pending;
	} catch (error) {
		if (input.handles.get(input.cacheKey) === pending) {
			input.handles.delete(input.cacheKey);
		}
		throw error;
	}
}

async function shutdownSession(
	actorSession: ReturnType<typeof createActorSession>,
	connection: AgentOSActorConnection,
): Promise<void> {
	let shutdownError: unknown;
	try {
		await actorSession.shutdown();
	} catch (error) {
		shutdownError = error;
	}
	try {
		await connection.dispose();
	} catch (disposeError) {
		if (shutdownError) {
			throw new AggregateError(
				[shutdownError, disposeError],
				"agentOS Eve shutdown and connection cleanup both failed",
			);
		}
		throw disposeError;
	}
	if (shutdownError) throw shutdownError;
}

function coreConnection(vm: AgentOs): AgentOSActorConnection {
	const methods = vm as unknown as Record<string, unknown>;
	for (const method of [
		"dispose",
		"readFile",
		"writeFile",
		"exists",
		"mkdir",
		"remove",
	]) {
		if (typeof methods?.[method] !== "function") {
			throw new TypeError(
				"agentOSCoreBackend create() must return an AgentOs instance",
			);
		}
	}
	const processMethods = vm.process as unknown as
		| Record<string, unknown>
		| undefined;
	for (const method of ["spawn", "wait", "kill"]) {
		if (typeof processMethods?.[method] !== "function") {
			throw new TypeError(
				"agentOSCoreBackend create() must return an AgentOs instance",
			);
		}
	}
	const outputListeners = new Set<(payload: unknown) => void>();
	return {
		ready: Promise.resolve(),
		on(event, listener) {
			if (event !== "processOutput") {
				throw new Error(`unsupported agentOS Core event: ${event}`);
			}
			outputListeners.add(listener);
			return () => outputListeners.delete(listener);
		},
		async dispose() {
			outputListeners.clear();
			await vm.dispose();
		},
		async spawn(command, args, options) {
			let processPid: number | undefined;
			const earlyOutput: Array<{
				stream: "stdout" | "stderr";
				data: Uint8Array;
			}> = [];
			const emit = (stream: "stdout" | "stderr", data: Uint8Array) => {
				if (processPid === undefined) {
					earlyOutput.push({ stream, data });
					return;
				}
				for (const listener of outputListeners) {
					listener({ pid: processPid, stream, data });
				}
			};
			const process = await vm.process.spawn(command, args, {
				...options,
				onStdout: (data) => emit("stdout", data),
				onStderr: (data) => emit("stderr", data),
			});
			processPid = process.pid;
			for (const event of earlyOutput) emit(event.stream, event.data);
			return process;
		},
		async waitProcess(pid) {
			const exit = await vm.process.wait(pid);
			return exit.exitCode ?? 1;
		},
		killProcess: (pid) => vm.process.kill(pid),
		readFile: (path) => vm.readFile(path),
		writeFile: (path, content) => vm.writeFile(path, content),
		exists: (path) => vm.exists(path),
		mkdir: (path, options) => vm.mkdir(path, options),
		remove: (path, options) => vm.remove(path, options),
	};
}

function createActorSession(
	connection: AgentOSActorConnection,
	id: string,
	networkPolicyOwner: string,
): { session: SandboxSession; shutdown(): Promise<void> } {
	type OutputEvent = {
		pid: number;
		stream: "stdout" | "stderr";
		data: Uint8Array;
	};
	type OutputRoute = {
		deliver(event: OutputEvent): void;
		fail(error: unknown): void;
	};
	type OrphanOutput = {
		events: OutputEvent[];
		error?: unknown;
	};

	const activeProcesses = new Map<number, { kill: () => Promise<void> }>();
	const pendingSpawnBarriers = new Set<Promise<void>>();
	const outputRoutes = new Map<number, OutputRoute>();
	const orphanOutput = new Map<number, OrphanOutput>();
	const shutdownController = new AbortController();
	let pendingSpawnCount = 0;
	let closed = false;
	let shutdownPromise: Promise<void> | undefined;

	function assertOpen(): void {
		if (closed) throw new Error("agentOS Eve sandbox session is shut down");
	}

	function clearOrphanOutput(): void {
		orphanOutput.clear();
	}

	async function stopUntrackedProcess(pid: number): Promise<void> {
		await settleWithin(connection.killProcess(pid), PROCESS_STOP_TIMEOUT_MS);
		await settleWithin(connection.waitProcess(pid), PROCESS_STOP_TIMEOUT_MS);
	}

	const unsubscribeOutput = subscribeActorEvent(
		connection,
		"processOutput",
		(rawEvent: unknown) => {
			if (!rawEvent || typeof rawEvent !== "object") return;
			const pid = (rawEvent as { pid?: unknown }).pid;
			if (!Number.isSafeInteger(pid)) return;
			const processPid = pid as number;
			const route = outputRoutes.get(processPid);
			let event: OutputEvent;
			try {
				const stream = (rawEvent as { stream?: unknown }).stream;
				if (stream !== "stdout" && stream !== "stderr") {
					throw new TypeError("agentOS process output has an invalid stream");
				}
				event = {
					pid: processPid,
					stream,
					data: actorBytes((rawEvent as { data?: unknown }).data),
				};
			} catch (error) {
				if (route) route.fail(error);
				else if (pendingSpawnCount > 0) {
					const orphan = orphanOutput.get(processPid) ?? {
						events: [],
					};
					orphan.error = error;
					orphanOutput.set(processPid, orphan);
				}
				return;
			}
			if (route) {
				route.deliver(event);
				return;
			}
			if (pendingSpawnCount === 0) return;
			const orphan = orphanOutput.get(processPid) ?? {
				events: [],
			};
			orphan.events.push(event);
			orphanOutput.set(processPid, orphan);
		},
	);

	async function spawn(
		options: Parameters<SandboxSession["spawn"]>[0],
	): Promise<SandboxProcess> {
		assertOpen();
		options.abortSignal?.throwIfAborted();
		let releaseSpawnBarrier!: () => void;
		const spawnBarrier = new Promise<void>((resolve) => {
			releaseSpawnBarrier = resolve;
		});
		pendingSpawnBarriers.add(spawnBarrier);
		let spawnBarrierReleased = false;
		const finishSpawnBarrier = () => {
			if (spawnBarrierReleased) return;
			spawnBarrierReleased = true;
			pendingSpawnBarriers.delete(spawnBarrier);
			releaseSpawnBarrier();
		};
		pendingSpawnCount += 1;
		let processPid: number;
		const spawnRequest = connection.spawn("sh", ["-lc", options.command], {
			cwd: resolveWorkspacePath(options.workingDirectory ?? WORKSPACE_ROOT),
			env: options.env,
		});
		const spawnSignals = options.abortSignal
			? [options.abortSignal, shutdownController.signal]
			: [shutdownController.signal];
		try {
			const process = await waitForPromiseOrAbort(spawnRequest, spawnSignals);
			processPid = process.pid;
		} catch (error) {
			if (spawnSignals.some((signal) => signal.aborted)) {
				void spawnRequest
					.then(async ({ pid }) => {
						if (Number.isSafeInteger(pid) && pid > 0) {
							await stopUntrackedProcess(pid);
						}
					})
					.catch(() => {});
			}
			pendingSpawnCount -= 1;
			if (pendingSpawnCount === 0) clearOrphanOutput();
			finishSpawnBarrier();
			throw error;
		}
		if (!Number.isSafeInteger(processPid) || processPid <= 0) {
			pendingSpawnCount -= 1;
			if (pendingSpawnCount === 0) clearOrphanOutput();
			finishSpawnBarrier();
			throw new TypeError("agentOS spawn returned an invalid process PID");
		}
		if (closed) {
			pendingSpawnCount -= 1;
			if (pendingSpawnCount === 0) clearOrphanOutput();
			await stopUntrackedProcess(processPid);
			finishSpawnBarrier();
			throw new Error("agentOS Eve sandbox session is shut down");
		}

		const stdout = createGuardedByteStream();
		const stderr = createGuardedByteStream();
		let settled = false;
		let terminalError: unknown;
		let killPromise: Promise<void> | undefined;

		const fail = (error: unknown) => {
			if (settled || terminalError) return;
			terminalError = error;
			stdout.error(error);
			stderr.error(error);
			void kill();
		};
		const route: OutputRoute = {
			deliver(event) {
				if (settled || terminalError) return;
				(event.stream === "stderr" ? stderr : stdout).enqueue(event.data);
			},
			fail,
		};
		outputRoutes.set(processPid, route);
		pendingSpawnCount -= 1;
		const pending = orphanOutput.get(processPid);
		if (pending) {
			orphanOutput.delete(processPid);
		}
		if (pendingSpawnCount === 0) clearOrphanOutput();

		const exit: Promise<number> = Promise.resolve()
			.then(() => connection.waitProcess(processPid))
			.then(
				(exitCode: number) => {
					settled = true;
					outputRoutes.delete(processPid);
					if (!terminalError) {
						stdout.close();
						stderr.close();
					}
					return exitCode;
				},
				(error: unknown) => {
					settled = true;
					outputRoutes.delete(processPid);
					if (!terminalError) {
						terminalError = error;
						stdout.error(error);
						stderr.error(error);
					}
					throw error;
				},
			);
		function kill(): Promise<void> {
			killPromise ??= (async () => {
				if (!settled) {
					await settleWithin(
						connection.killProcess(processPid),
						PROCESS_STOP_TIMEOUT_MS,
					);
				}
				await settleWithin(exit, PROCESS_STOP_TIMEOUT_MS);
			})();
			return killPromise;
		}
		activeProcesses.set(processPid, { kill });
		void exit.finally(() => activeProcesses.delete(processPid)).catch(() => {});
		if (pending) {
			for (const event of pending.events) route.deliver(event);
			if (pending.error) route.fail(pending.error);
		}

		const abort = () => {
			if (terminalError) return;
			terminalError =
				options.abortSignal?.reason ??
				new DOMException("Aborted", "AbortError");
			stdout.error(terminalError);
			stderr.error(terminalError);
			void kill();
		};
		options.abortSignal?.addEventListener("abort", abort, { once: true });
		if (options.abortSignal?.aborted) abort();
		const removeAbortListener = () =>
			options.abortSignal?.removeEventListener("abort", abort);
		void exit.then(removeAbortListener, removeAbortListener);
		finishSpawnBarrier();

		return {
			pid: processPid,
			stdout: stdout.stream,
			stderr: stderr.stream,
			async wait() {
				const exitCode = await exit;
				if (terminalError) throw terminalError;
				return { exitCode };
			},
			kill,
		};
	}

	async function run(
		options: Parameters<SandboxSession["run"]>[0],
	): Promise<SandboxCommandResult> {
		const process = await spawn(options);
		const [stdout, stderr, result] = await Promise.all([
			streamToBytes(process.stdout).then((bytes) => decode(bytes, "utf-8")),
			streamToBytes(process.stderr).then((bytes) => decode(bytes, "utf-8")),
			process.wait(),
		]);
		return { ...result, stdout, stderr };
	}

	const session: SandboxSession = {
		id,
		resolvePath: resolveWorkspacePath,
		run,
		spawn,
		async readFile({ path, abortSignal }) {
			assertOpen();
			abortSignal?.throwIfAborted();
			const resolved = resolveWorkspacePath(path);
			if (!(await connection.exists(resolved))) return null;
			return bytesToStream(actorBytes(await connection.readFile(resolved)));
		},
		async readBinaryFile({ path, abortSignal }) {
			assertOpen();
			abortSignal?.throwIfAborted();
			const resolved = resolveWorkspacePath(path);
			return (await connection.exists(resolved))
				? actorBytes(await connection.readFile(resolved))
				: null;
		},
		async readTextFile(options) {
			assertOpen();
			options.abortSignal?.throwIfAborted();
			validateLineRange(options);
			const resolved = resolveWorkspacePath(options.path);
			if (!(await connection.exists(resolved))) return null;
			return selectLines(
				decode(
					actorBytes(await connection.readFile(resolved)),
					options.encoding ?? "utf-8",
				),
				options,
			);
		},
		async writeFile({ path: authoredPath, content, abortSignal }) {
			assertOpen();
			abortSignal?.throwIfAborted();
			const path = resolveWorkspacePath(authoredPath);
			await connection.mkdir(posix.dirname(path), { recursive: true });
			await connection.writeFile(path, await streamToBytes(content));
		},
		async writeBinaryFile({ path: authoredPath, content, abortSignal }) {
			assertOpen();
			abortSignal?.throwIfAborted();
			const path = resolveWorkspacePath(authoredPath);
			await connection.mkdir(posix.dirname(path), { recursive: true });
			await connection.writeFile(path, content);
		},
		async writeTextFile(options) {
			assertOpen();
			options.abortSignal?.throwIfAborted();
			const path = resolveWorkspacePath(options.path);
			await connection.mkdir(posix.dirname(path), { recursive: true });
			await connection.writeFile(
				path,
				Buffer.from(
					options.content,
					(options.encoding ?? "utf-8") as BufferEncoding,
				),
			);
		},
		async removePath(options) {
			assertOpen();
			options.abortSignal?.throwIfAborted();
			const path = resolveWorkspacePath(options.path);
			if (options.force && !(await connection.exists(path))) return;
			await connection.remove(path, { recursive: options.recursive });
		},
		async setNetworkPolicy() {
			assertOpen();
			throw new Error(
				`agentOS network policy is fixed by ${networkPolicyOwner}`,
			);
		},
	};

	return {
		session,
		async shutdown() {
			shutdownPromise ??= (async () => {
				closed = true;
				shutdownController.abort(
					new Error("agentOS Eve sandbox session is shut down"),
				);
				await Promise.allSettled([...pendingSpawnBarriers]);
				await Promise.allSettled(
					[...activeProcesses.values()].map(({ kill }) => kill()),
				);
				unsubscribeOutput();
				clearOrphanOutput();
			})();
			await shutdownPromise;
		},
	};
}

function resolveWorkspacePath(path: string): string {
	const expanded = path
		.replace(/^\$HOME(?=\/|$)/u, AGENTOS_HOME)
		.replace(/^\$\{HOME\}(?=\/|$)/u, AGENTOS_HOME);
	if (isAbsolute(expanded)) return expanded;
	const resolved = posix.resolve(WORKSPACE_ROOT, expanded);
	if (
		resolved !== WORKSPACE_ROOT &&
		!resolved.startsWith(`${WORKSPACE_ROOT}/`)
	) {
		throw new RangeError(
			`relative sandbox path escapes ${WORKSPACE_ROOT}: ${path}`,
		);
	}
	return resolved;
}

function waitForPromiseOrAbort<T>(
	promise: Promise<T>,
	signals: AbortSignal[],
): Promise<T> {
	return new Promise<T>((resolve, reject) => {
		let settled = false;
		const cleanup = () => {
			for (const signal of signals) {
				signal.removeEventListener("abort", onAbort);
			}
		};
		const finish = (callback: () => void) => {
			if (settled) return;
			settled = true;
			cleanup();
			callback();
		};
		const onAbort = () => {
			const signal = signals.find((candidate) => candidate.aborted);
			if (!signal) return;
			finish(() =>
				reject(signal.reason ?? new DOMException("Aborted", "AbortError")),
			);
		};

		for (const signal of signals) {
			signal.addEventListener("abort", onAbort, { once: true });
		}
		onAbort();
		promise.then(
			(value) => finish(() => resolve(value)),
			(error) => finish(() => reject(error)),
		);
	});
}

async function settleWithin(
	promise: Promise<unknown>,
	timeoutMs: number,
): Promise<void> {
	let timeout: ReturnType<typeof setTimeout> | undefined;
	try {
		await Promise.race([
			promise.then(
				() => undefined,
				() => undefined,
			),
			new Promise<void>((resolve) => {
				timeout = setTimeout(resolve, timeoutMs);
				timeout.unref?.();
			}),
		]);
	} finally {
		if (timeout) clearTimeout(timeout);
	}
}

function stableId(value: string): string {
	return createHash("sha256").update(value).digest("hex");
}

function subscribeActorEvent(
	connection: AgentOSActorConnection,
	event: string,
	listener: (payload: unknown) => void,
): () => void {
	const result = connection.on(event, listener);
	if (typeof result === "function") return result as () => void;
	return () => {
		if (typeof connection.off === "function") connection.off(event, listener);
		else if (typeof connection.removeListener === "function") {
			connection.removeListener(event, listener);
		}
	};
}

function actorBytes(value: unknown): Uint8Array {
	if (value instanceof Uint8Array) return value;
	if (
		Array.isArray(value) &&
		value.length === 2 &&
		value[0] === "$Uint8Array" &&
		typeof value[1] === "string"
	) {
		return Buffer.from(value[1], "base64");
	}
	throw new TypeError("agentOS returned an invalid binary payload");
}

interface GuardedByteStream {
	stream: ReadableStream<Uint8Array>;
	enqueue(bytes: Uint8Array): void;
	close(): void;
	error(error: unknown): void;
}

function createGuardedByteStream(): GuardedByteStream {
	let controller!: ReadableStreamDefaultController<Uint8Array>;
	let open = true;
	const stream = new ReadableStream<Uint8Array>({
		start(value) {
			controller = value;
		},
		cancel() {
			open = false;
		},
	});
	return {
		stream,
		enqueue(bytes) {
			if (!open) return;
			try {
				controller.enqueue(bytes);
			} catch {
				open = false;
			}
		},
		close() {
			if (!open) return;
			open = false;
			try {
				controller.close();
			} catch {}
		},
		error(error) {
			if (!open) return;
			open = false;
			try {
				controller.error(error);
			} catch {}
		},
	};
}

function bytesToStream(bytes: Uint8Array): ReadableStream<Uint8Array> {
	return new ReadableStream({
		start(controller) {
			controller.enqueue(bytes);
			controller.close();
		},
	});
}

async function streamToBytes(
	stream: ReadableStream<Uint8Array>,
): Promise<Uint8Array> {
	const chunks: Uint8Array[] = [];
	let length = 0;
	const reader = stream.getReader();
	try {
		for (;;) {
			const result = await reader.read();
			if (result.done) break;
			chunks.push(result.value);
			length += result.value.byteLength;
		}
	} finally {
		reader.releaseLock();
	}
	const bytes = new Uint8Array(length);
	let offset = 0;
	for (const chunk of chunks) {
		bytes.set(chunk, offset);
		offset += chunk.byteLength;
	}
	return bytes;
}

function decode(bytes: Uint8Array, encoding: string): string {
	if (encoding === "utf-8" || encoding === "utf8") {
		return new TextDecoder("utf-8").decode(bytes);
	}
	return Buffer.from(bytes).toString(encoding as BufferEncoding);
}

function validateLineRange(options: SandboxReadTextFileOptions): void {
	const { startLine, endLine } = options;
	if (
		startLine !== undefined &&
		(!Number.isInteger(startLine) || startLine < 1)
	) {
		throw new Error("startLine must be a positive integer (1-based).");
	}
	if (endLine !== undefined && (!Number.isInteger(endLine) || endLine < 1)) {
		throw new Error("endLine must be a positive integer (1-based).");
	}
	if (startLine !== undefined && endLine !== undefined && startLine > endLine) {
		throw new Error("startLine must not be greater than endLine.");
	}
}

function selectLines(
	text: string,
	options: SandboxReadTextFileOptions,
): string {
	if (options.startLine === undefined && options.endLine === undefined) {
		return text;
	}
	const lines = text.match(/.*(?:\r\n|\r|\n)|.+$/gu) ?? [];
	const start = (options.startLine ?? 1) - 1;
	const end = options.endLine ?? lines.length;
	return lines.slice(start, end).join("");
}
