import { createHash } from "node:crypto";
import { posix } from "node:path";
import {
	createSandboxSessionEnv,
	type FileStat,
	type SandboxApi,
	type SandboxFactory,
	type ShellResult,
} from "@flue/runtime";
import type { Registry } from "@rivet-dev/agentos";
import type { Client } from "@rivet-dev/agentos/client";
import type { AgentOs, VirtualStat } from "@rivet-dev/agentos-core";

const DEFAULT_CWD = "/workspace";

interface AgentOSActorConnection {
	readonly ready: PromiseLike<void>;
	exec(
		command: string,
		options?: {
			cwd?: string;
			env?: Record<string, string>;
			timeout?: number;
			captureStdio?: boolean;
		},
	): Promise<ShellResult>;
	readFile(path: string): Promise<Uint8Array>;
	writeFile(path: string, content: string | Uint8Array): Promise<void>;
	stat(path: string): Promise<VirtualStat>;
	readdir(path: string): Promise<string[]>;
	exists(path: string): Promise<boolean>;
	mkdir(path: string, options?: { recursive?: boolean }): Promise<void>;
	remove(path: string, options?: { recursive?: boolean }): Promise<void>;
}

interface AgentOSActorAccessor {
	getOrCreate(
		key: string[],
		options?: { params?: unknown },
	): { connect(): AgentOSActorConnection };
}

type ActorName<TRegistry extends Registry<any>> = Extract<
	keyof TRegistry["config"]["use"],
	string
>;

export interface AgentOSSandboxOptions<
	TRegistry extends Registry<any> = Registry<any>,
> {
	/** Registry key of an actor created with `agentOS()`. */
	actor: ActorName<TRegistry>;
	/** Application registry containing the selected agentOS actor. */
	registry: TRegistry;
	/** Base directory exposed to Flue. Defaults to `/workspace`. */
	cwd?: string;
	/** Connection parameters forwarded to the actor's `onBeforeConnect` hook. */
	params?: unknown;
	/** Advanced: an existing client for the same registry. */
	client?: Client<TRegistry>;
}

export interface AgentOSCoreSandboxOptions {
	/**
	 * Returns the caller-owned agentOS Core VM for a Flue context. The caller
	 * must retain and dispose VMs because Flue has no sandbox disposal hook.
	 */
	create(input: { id: string }): AgentOs | Promise<AgentOs>;
	/** Base directory exposed to Flue. Defaults to `/workspace`. */
	cwd?: string;
}

export class AgentOSFlueConfigurationError extends Error {
	readonly code = "agentos_flue_configuration";

	constructor(
		readonly actor: string,
		message: string,
		options?: ErrorOptions,
	) {
		super(message, options);
		this.name = "AgentOSFlueConfigurationError";
	}
}

/** Uses an existing `agentOS()` actor as the sandbox for each Flue context. */
export function agentOSSandbox<TRegistry extends Registry<any>>(
	options: AgentOSSandboxOptions<TRegistry>,
): SandboxFactory {
	if (!options || typeof options.actor !== "string" || !options.actor.trim()) {
		throw new TypeError("agentOSSandbox requires the registry actor name");
	}
	if (
		!options.registry ||
		typeof options.registry.startAndWait !== "function"
	) {
		throw new TypeError("agentOSSandbox requires the application registry");
	}
	const actor = options.actor.trim();
	const cwd = sandboxCwd(options.cwd);
	const getClient = createActorClient(options);

	return {
		async createSessionEnv({ id }) {
			await options.registry.startAndWait();
			const connection = await connectActor(
				await getClient(),
				actor,
				actorKey(id),
				options.params,
			);
			return createSandboxSessionEnv(actorSandboxApi(connection), cwd);
		},
	};
}

/** Uses caller-created standalone agentOS Core VMs as Flue sandboxes. */
export function agentOSCoreSandbox(
	options: AgentOSCoreSandboxOptions,
): SandboxFactory {
	if (!options || typeof options.create !== "function") {
		throw new TypeError("agentOSCoreSandbox requires a create factory");
	}
	const cwd = sandboxCwd(options.cwd);

	return {
		async createSessionEnv({ id }) {
			const vm = await options.create({ id });
			assertCoreVm(vm);
			return createSandboxSessionEnv(coreSandboxApi(vm), cwd);
		},
	};
}

function createActorClient<TRegistry extends Registry<any>>(
	options: AgentOSSandboxOptions<TRegistry>,
): () => Promise<Client<TRegistry>> {
	if (options.client) {
		return () => Promise.resolve(options.client as Client<TRegistry>);
	}
	let pending: Promise<Client<TRegistry>> | undefined;
	return () => {
		pending ??= import("@rivet-dev/agentos/client").then(({ createClient }) =>
			createClient<TRegistry>(),
		);
		return pending;
	};
}

async function connectActor<TRegistry extends Registry<any>>(
	client: Client<TRegistry>,
	actor: string,
	key: string[],
	params: unknown,
): Promise<AgentOSActorConnection> {
	try {
		const accessor = client[
			actor as keyof Client<TRegistry>
		] as unknown as AgentOSActorAccessor | undefined;
		if (!accessor || typeof accessor.getOrCreate !== "function") {
			throw new Error(`registry has no actor named ${JSON.stringify(actor)}`);
		}
		const connection = accessor
			.getOrCreate(key, { params })
			.connect();
		await connection.ready;
		assertActorConnection(connection);
		return connection;
	} catch (cause) {
		if (cause instanceof AgentOSFlueConfigurationError) throw cause;
		throw new AgentOSFlueConfigurationError(
			actor,
			`agentOS actor ${JSON.stringify(actor)} could not be used; register an agentOS() actor under that exact setup({ use }) key: ${cause instanceof Error ? cause.message : String(cause)}`,
			{ cause },
		);
	}
}

function assertActorConnection(
	connection: AgentOSActorConnection,
): asserts connection is AgentOSActorConnection {
	const methods = connection as unknown as Record<string, unknown>;
	for (const method of [
		"exec",
		"readFile",
		"writeFile",
		"stat",
		"readdir",
		"exists",
		"mkdir",
		"remove",
	]) {
		if (typeof methods[method] !== "function") {
			throw new Error("selected actor is not an @rivet-dev/agentos actor");
		}
	}
}

function assertCoreVm(vm: AgentOs): void {
	const methods = vm as unknown as Record<string, unknown>;
	for (const method of [
		"exec",
		"readFile",
		"writeFile",
		"stat",
		"readdir",
		"exists",
		"mkdir",
		"remove",
	]) {
		if (typeof methods?.[method] !== "function") {
			throw new TypeError(
				"agentOSCoreSandbox create() must return an AgentOs instance",
			);
		}
	}
}

function actorSandboxApi(connection: AgentOSActorConnection): SandboxApi {
	return sandboxApi({
		exec: (command, options) => connection.exec(command, options),
		readFile: (path) => connection.readFile(path),
		writeFile: (path, content) => connection.writeFile(path, content),
		stat: (path) => connection.stat(path),
		readdir: (path) => connection.readdir(path),
		exists: (path) => connection.exists(path),
		mkdir: (path, options) => connection.mkdir(path, options),
		remove: (path, options) => connection.remove(path, options),
	});
}

function coreSandboxApi(vm: AgentOs): SandboxApi {
	return sandboxApi({
		exec: (command, options) => vm.exec(command, options),
		readFile: (path) => vm.readFile(path),
		writeFile: (path, content) => vm.writeFile(path, content),
		stat: (path) => vm.stat(path),
		readdir: (path) => vm.readdir(path),
		exists: (path) => vm.exists(path),
		mkdir: (path, options) => vm.mkdir(path, options),
		remove: (path, options) => vm.remove(path, options),
	});
}

interface AgentOSSandboxApiSource {
	exec(
		command: string,
		options?: {
			cwd?: string;
			env?: Record<string, string>;
			timeout?: number;
			captureStdio?: boolean;
		},
	): Promise<ShellResult>;
	readFile(path: string): Promise<Uint8Array>;
	writeFile(path: string, content: string | Uint8Array): Promise<void>;
	stat(path: string): Promise<VirtualStat>;
	readdir(path: string): Promise<string[]>;
	exists(path: string): Promise<boolean>;
	mkdir(path: string, options?: { recursive?: boolean }): Promise<void>;
	remove(path: string, options?: { recursive?: boolean }): Promise<void>;
}

function sandboxApi(source: AgentOSSandboxApiSource): SandboxApi {
	return {
		async readFile(path) {
			return new TextDecoder().decode(await source.readFile(path));
		},
		readFileBuffer: source.readFile,
		writeFile: source.writeFile,
		async stat(path): Promise<FileStat> {
			const stat = await source.stat(path);
			return {
				isFile: (stat.mode & 0o170000) === 0o100000,
				isDirectory: stat.isDirectory,
				isSymbolicLink: stat.isSymbolicLink,
				size: stat.size,
				mtime: new Date(stat.mtimeMs),
			};
		},
		readdir: source.readdir,
		exists: source.exists,
		mkdir: source.mkdir,
		async rm(path, options) {
			if (options?.force && !(await source.exists(path))) return;
			try {
				await source.remove(path, { recursive: options?.recursive });
			} catch (error) {
				if (options?.force && !(await source.exists(path))) return;
				throw error;
			}
		},
		exec(command, options) {
			return source.exec(command, {
				cwd: options?.cwd,
				env: options?.env,
				timeout: options?.timeoutMs,
				captureStdio: true,
			});
		},
	};
}

function actorKey(id: string): string[] {
	return ["flue", "sandbox", stableId(id)];
}

function stableId(value: string): string {
	return createHash("sha256").update(value).digest("hex");
}

function sandboxCwd(value: string | undefined): string {
	const cwd = value ?? DEFAULT_CWD;
	if (!posix.isAbsolute(cwd)) {
		throw new TypeError("agentOS Flue sandbox cwd must be an absolute path");
	}
	return posix.normalize(cwd);
}
