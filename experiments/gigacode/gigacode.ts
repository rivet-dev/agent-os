#!/usr/bin/env -S node --import tsx

import { spawn } from "node:child_process";
import { createHash, randomBytes } from "node:crypto";
import {
	type Dirent,
	existsSync,
	readFileSync,
	realpathSync,
	statSync,
} from "node:fs";
import {
	mkdir,
	open,
	readdir,
	readFile,
	rename,
	rm,
	writeFile,
} from "node:fs/promises";
import {
	createServer,
	type IncomingMessage,
	type ServerResponse,
} from "node:http";
import { homedir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import pino, { type Logger } from "pino";
import { ForegroundPriorityGate } from "./foreground-priority-gate";

// Keep daemon-only dependencies out of tsx's eager dynamic-import rewrite.
// The rewrite currently fails to parse this single-file client/server entrypoint;
// the native runtime import is still standard ESM and remains fully lazy.
const importKeyword = ["im", "port"].join("");
const importModule = new Function(
	"specifier",
	`return ${importKeyword}(specifier)`,
) as (specifier: string) => Promise<any>;

const HOST = "127.0.0.1";
const API_PORT = boundedPort("GIGACODE_PORT", 2468);
const RIVET_PORT = boundedPort("GIGACODE_RIVET_PORT", 2469);
if (RIVET_PORT === 6420) {
	throw new Error("GIGACODE_RIVET_PORT must not be 6420");
}
const API_ENDPOINT = `http://${HOST}:${API_PORT}`;
const RIVET_ENDPOINT = `http://${HOST}:${RIVET_PORT}`;
const INSPECTOR_URL =
	process.env.GIGACODE_INSPECTOR_URL ?? "http://localhost:43708/";
const STATE_DIR =
	process.env.GIGACODE_STATE_DIR ??
	join(homedir(), ".local", "state", "gigacode");
// Rivet's default ~/.rivetkit database is process-global across ports. Give the
// central Gigacode daemon an isolated, versioned engine database so unrelated
// local Rivet apps cannot hold its RocksDB lock and actors from an incompatible
// experiment generation cannot prevent new actors from scheduling.
const RIVET_STORAGE_PATH = resolve(
	process.env.RIVETKIT_STORAGE_PATH ?? join(STATE_DIR, "engine-v1"),
);
process.env.RIVETKIT_STORAGE_PATH = RIVET_STORAGE_PATH;
const RIVET_NAMESPACE = process.env.RIVET_NAMESPACE ?? "default";
process.env.RIVET_NAMESPACE = RIVET_NAMESPACE;
const PID_FILE = join(STATE_DIR, "daemon.pid");
const LOG_FILE = join(STATE_DIR, "daemon.log");
const LEGACY_SESSION_METADATA_FILE = join(STATE_DIR, "sessions.json");
const MESSAGE_STORE_FILE = join(STATE_DIR, "messages.json");
const MODEL_CACHE_FILE = join(STATE_DIR, "models.json");
const SESSION_LOG_DIR = join(STATE_DIR, "session-logs");
const COORDINATOR_NAME = "coordinator";
const COORDINATOR_KEY = "global";
const RUNNER_NAME = "default";
const MAX_BODY_BYTES = 4 * 1024 * 1024;
const MAX_SESSIONS = 1_000;
const MAX_EVENTS = 4_096;
const MAX_PROMPT_QUEUE_PER_SESSION = boundedInteger(
	"GIGACODE_MAX_PROMPT_QUEUE_PER_SESSION",
	64,
);
const MAX_MESSAGES_PER_SESSION = boundedInteger(
	"GIGACODE_MAX_MESSAGES_PER_SESSION",
	10_000,
);
const MAX_MESSAGE_STORE_BYTES = boundedInteger(
	"GIGACODE_MAX_MESSAGE_STORE_BYTES",
	64 * 1024 * 1024,
);
const MAX_FS_FIND_SCAN = boundedInteger("GIGACODE_MAX_FS_FIND_SCAN", 50_000);
const MAX_FS_FIND_RESULTS = boundedInteger("GIGACODE_MAX_FS_FIND_RESULTS", 200);
const MODEL_PROBE_CONCURRENCY = boundedInteger(
	"GIGACODE_MODEL_PROBE_CONCURRENCY",
	1,
);
const RIVET_HEALTH_TIMEOUT_MS = boundedInteger(
	"GIGACODE_RIVET_HEALTH_TIMEOUT_MS",
	60_000,
);
const RIVET_ACTOR_READY_TIMEOUT_MS = boundedInteger(
	"GIGACODE_RIVET_ACTOR_READY_TIMEOUT_MS",
	RIVET_HEALTH_TIMEOUT_MS,
);
const RIVET_ACTOR_STOP_THRESHOLD_MS = boundedInteger(
	"GIGACODE_RIVET_ACTOR_STOP_THRESHOLD_MS",
	10_000,
);
const RIVET_RESCHEDULE_BACKOFF_MAX_EXPONENT = boundedInteger(
	"GIGACODE_RIVET_RESCHEDULE_BACKOFF_MAX_EXPONENT",
	2,
);
const STARTUP_TIMEOUT_MS = boundedInteger(
	"GIGACODE_STARTUP_TIMEOUT_MS",
	240_000,
);
const SHUTDOWN_GRACE_MS = boundedInteger("GIGACODE_SHUTDOWN_GRACE_MS", 5_000);
const RIVET_SHUTDOWN_TIMEOUT_MS = boundedInteger(
	"GIGACODE_RIVET_SHUTDOWN_TIMEOUT_MS",
	30_000,
);
const CANCEL_QUIESCE_TIMEOUT_MS = boundedInteger(
	"GIGACODE_CANCEL_QUIESCE_TIMEOUT_MS",
	3_000,
);
const MAX_SESSION_ENV_ENTRIES = 128;
const MAX_SESSION_ENV_BYTES = 64 * 1024;
const MAX_PI_CONFIG_BYTES = 64 * 1024;
const MAX_MODEL_CACHE_BYTES = 1024 * 1024;
const MAX_OPENCODE_AUTH_BYTES = 1024 * 1024;
// GigaCode deliberately mounts trusted host projects read-write. AgentOS applies
// its filesystem limits to each writable mount, so the normal 64 MiB/16k-inode
// sandbox defaults reject ordinary source trees based on their existing size.
const MAX_FILESYSTEM_BYTES = boundedInteger(
	"GIGACODE_MAX_FILESYSTEM_BYTES",
	1024 ** 4,
);
const MAX_INODE_COUNT = boundedInteger("GIGACODE_MAX_INODE_COUNT", 10_000_000);
const VERSION = "0.0.1";
const WORKSPACE_MOUNT_PATH = "/workspace";
const DEFAULT_DIRECTORY = canonicalDirectory(
	process.env.GIGACODE_WORKSPACE ?? process.cwd(),
);
function localSandboxInstructions(directory: string) {
	return `
Gigacode is a local AgentOS VM experiment, not a security boundary.
The host project ${directory} is mounted read-write at ${WORKSPACE_MOUNT_PATH},
which is your working directory.

Use your normal shell and execution tools for commands; they execute inside the
local AgentOS VM, not Docker. Writes under ${WORKSPACE_MOUNT_PATH} affect the
real project immediately, subject to the daemon user's operating-system
permissions. Other host directories are not mounted into this VM.
`.trim();
}

function startupLog(message: string, startedAt?: number): void {
	const elapsed =
		startedAt === undefined
			? ""
			: ` (${Math.round(performance.now() - startedAt)} ms)`;
	console.log(`[gigacode] ${message}${elapsed}`);
}

const harnesses = {
	claude: { label: "Claude Code" },
	codex: { label: "Codex" },
	pi: { label: "Pi" },
	opencode: { label: "OpenCode" },
} as const;
type Harness = keyof typeof harnesses;

const HOST_CREDENTIAL_ENV_NAMES = [
	"ANTHROPIC_API_KEY",
	"ANTHROPIC_AUTH_TOKEN",
	"ANTHROPIC_BASE_URL",
	"ANTHROPIC_MODEL",
	"CLAUDE_CODE_USE_BEDROCK",
	"CLAUDE_CODE_USE_VERTEX",
	"OPENAI_API_KEY",
	"OPENAI_BASE_URL",
	"CODEX_API_KEY",
	"AWS_REGION",
	"AWS_PROFILE",
	"AWS_ACCESS_KEY_ID",
	"AWS_SECRET_ACCESS_KEY",
	"AWS_SESSION_TOKEN",
] as const;

function hostCredentialEnvironment(): Record<string, string> {
	return Object.fromEntries(
		HOST_CREDENTIAL_ENV_NAMES.flatMap((name) => {
			const value = process.env[name];
			return value === undefined ? [] : [[name, value]];
		}),
	);
}

function hostCredentialMounts() {
	if (process.env.GIGACODE_SHARE_HOST_CREDENTIALS === "0") return [];
	const readOnly = process.env.GIGACODE_CREDENTIALS_READ_ONLY === "1";
	const candidates = [
		{
			hostPath:
				process.env.GIGACODE_CLAUDE_CONFIG_DIR ?? join(homedir(), ".claude"),
			guestPath: "/home/agentos/.claude",
		},
		{
			hostPath: process.env.GIGACODE_CODEX_HOME ?? join(homedir(), ".codex"),
			guestPath: "/home/agentos/.codex",
		},
		{
			hostPath: process.env.GIGACODE_PI_HOME ?? join(homedir(), ".pi"),
			guestPath: "/home/agentos/.pi",
		},
	];
	return candidates.flatMap(({ hostPath, guestPath }) => {
		if (!existsSync(hostPath)) return [];
		if (!statSync(hostPath).isDirectory()) {
			throw new Error(`host credential path is not a directory: ${hostPath}`);
		}
		return [
			{
				path: guestPath,
				plugin: {
					id: "host_dir",
					config: { hostPath, readOnly },
				},
				readOnly,
			},
		];
	});
}

function openCodeAgentEnvironment(): Record<string, string> {
	const environment: Record<string, string> = {
		OPENCODE_DB: "/home/agentos/.local/share/opencode/opencode.db",
		OPENCODE_DISABLE_AUTOUPDATE: "1",
		OPENCODE_DISABLE_FILEWATCHER: "1",
		OPENCODE_DISABLE_LSP_DOWNLOAD: "1",
		OPENCODE_DISABLE_MODELS_FETCH: "1",
	};
	if (process.env.GIGACODE_SHARE_HOST_CREDENTIALS !== "0") {
		const dataDir =
			process.env.GIGACODE_OPENCODE_DATA_DIR ??
			join(homedir(), ".local", "share", "opencode");
		const authPath = join(dataDir, "auth.json");
		try {
			const auth = readFileSync(authPath, "utf8");
			if (Buffer.byteLength(auth) > MAX_OPENCODE_AUTH_BYTES) {
				throw new Error(
					`OpenCode auth file exceeds ${MAX_OPENCODE_AUTH_BYTES} bytes: ${authPath}`,
				);
			}
			JSON.parse(auth);
			environment.OPENCODE_AUTH_CONTENT = auth;
		} catch (error) {
			if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
		}
	}
	const configDir =
		process.env.GIGACODE_OPENCODE_CONFIG_DIR ??
		join(homedir(), ".config", "opencode");
	for (const filename of ["opencode.json", "config.json"]) {
		try {
			const config = JSON.parse(
				readFileSync(join(configDir, filename), "utf8"),
			) as Record<string, unknown> & {
				mcp?: Record<string, { type?: string; enabled?: boolean }>;
			};
			config.mcp = Object.fromEntries(
				Object.entries(config.mcp ?? {}).map(([name, entry]) => [
					name,
					// ACP sessions receive MCP servers explicitly through session/new.
					// Do not also start ambient host-configured servers: a slow or
					// unavailable remote MCP otherwise blocks unrelated ACP bootstrap.
					{ ...entry, enabled: false },
				]),
			);
			environment.OPENCODE_CONFIG_CONTENT = JSON.stringify(config);
			break;
		} catch {
			// Missing or JSONC-only host config: OpenCode will use its defaults.
		}
	}
	return environment;
}

function loopbackExemptPorts(): number[] {
	const raw = process.env.GIGACODE_LOOPBACK_EXEMPT_PORTS;
	if (!raw) return [];
	const ports = raw.split(",").map((entry) => {
		const port = Number(entry.trim());
		if (!Number.isInteger(port) || port < 1 || port > 65_535) {
			throw new Error(
				"GIGACODE_LOOPBACK_EXEMPT_PORTS must be a comma-separated list of ports",
			);
		}
		return port;
	});
	return [...new Set(ports)];
}

function networkPermission(): "allow" | "deny" | undefined {
	const value = process.env.GIGACODE_NETWORK_PERMISSION;
	if (value === undefined || value === "") return undefined;
	if (value === "allow" || value === "deny") return value;
	throw new Error('GIGACODE_NETWORK_PERMISSION must be "allow" or "deny"');
}

function sessionEnvironment(): Record<string, string> {
	const raw = process.env.GIGACODE_SESSION_ENV_JSON;
	if (!raw) return {};
	if (Buffer.byteLength(raw) > MAX_SESSION_ENV_BYTES) {
		throw new Error(
			`GIGACODE_SESSION_ENV_JSON exceeds ${MAX_SESSION_ENV_BYTES} bytes`,
		);
	}
	const value: unknown = JSON.parse(raw);
	if (!value || typeof value !== "object" || Array.isArray(value)) {
		throw new Error("GIGACODE_SESSION_ENV_JSON must be a JSON object");
	}
	const entries = Object.entries(value);
	if (entries.length > MAX_SESSION_ENV_ENTRIES) {
		throw new Error(
			`GIGACODE_SESSION_ENV_JSON exceeds ${MAX_SESSION_ENV_ENTRIES} entries`,
		);
	}
	for (const [key, entry] of entries) {
		if (!key || typeof entry !== "string") {
			throw new Error(
				"GIGACODE_SESSION_ENV_JSON keys must be non-empty and values must be strings",
			);
		}
	}
	return Object.fromEntries(entries) as Record<string, string>;
}

const USER_SESSION_ENV = sessionEnvironment();
const SESSION_ENV: Record<string, string> = {
	HOME: "/home/agentos",
	PWD: WORKSPACE_MOUNT_PATH,
	XDG_CACHE_HOME: "/home/agentos/.cache",
	XDG_CONFIG_HOME: "/home/agentos/.config",
	XDG_DATA_HOME: "/home/agentos/.local/share",
	XDG_STATE_HOME: "/home/agentos/.local/state",
	CLAUDE_CONFIG_DIR: "/home/agentos/.claude",
	CODEX_HOME: "/home/agentos/.codex",
	PI_ACP_PERMISSION_GATE: "1",
	...hostCredentialEnvironment(),
	...USER_SESSION_ENV,
};
const OPENCODE_SESSION_ENV: Record<string, string> = {
	...SESSION_ENV,
	...openCodeAgentEnvironment(),
	// Explicit session configuration remains authoritative over harness defaults.
	...USER_SESSION_ENV,
};
const NETWORK_PERMISSION = networkPermission();

function boundedPiConfig(name: string): string | undefined {
	const value = process.env[name];
	if (value === undefined || value === "") return undefined;
	if (Buffer.byteLength(value) > MAX_PI_CONFIG_BYTES) {
		throw new Error(`${name} exceeds ${MAX_PI_CONFIG_BYTES} bytes`);
	}
	return value;
}

function hostPiApiKey(): string | undefined {
	if (process.env.GIGACODE_SHARE_HOST_CREDENTIALS === "0") return undefined;
	const piHome = process.env.GIGACODE_PI_HOME ?? join(homedir(), ".pi");
	try {
		const value = JSON.parse(
			readFileSync(join(piHome, "agent", "models.json"), "utf8"),
		) as {
			providers?: { anthropic?: { apiKey?: unknown } };
		};
		const apiKey = value.providers?.anthropic?.apiKey;
		if (typeof apiKey !== "string" || apiKey.length === 0) return undefined;
		if (Buffer.byteLength(apiKey) > MAX_PI_CONFIG_BYTES) {
			throw new Error(
				`Pi API key in ${join(piHome, "agent", "models.json")} exceeds ${MAX_PI_CONFIG_BYTES} bytes`,
			);
		}
		return apiKey;
	} catch (error) {
		if ((error as NodeJS.ErrnoException).code === "ENOENT") return undefined;
		throw error;
	}
}

function hostPiBaseUrl(): string | undefined {
	if (process.env.GIGACODE_SHARE_HOST_CREDENTIALS === "0") return undefined;
	const piHome = process.env.GIGACODE_PI_HOME ?? join(homedir(), ".pi");
	try {
		const value = JSON.parse(
			readFileSync(join(piHome, "agent", "models.json"), "utf8"),
		) as {
			providers?: { anthropic?: { baseUrl?: unknown } };
		};
		const baseUrl = value.providers?.anthropic?.baseUrl;
		if (typeof baseUrl !== "string" || baseUrl.length === 0) return undefined;
		if (Buffer.byteLength(baseUrl) > MAX_PI_CONFIG_BYTES) {
			throw new Error(
				`Pi base URL in ${join(piHome, "agent", "models.json")} exceeds ${MAX_PI_CONFIG_BYTES} bytes`,
			);
		}
		return baseUrl;
	} catch (error) {
		if ((error as NodeJS.ErrnoException).code === "ENOENT") return undefined;
		throw error;
	}
}

const PI_API_KEY = boundedPiConfig("GIGACODE_PI_API_KEY") ?? hostPiApiKey();
const PI_BASE_URL =
	boundedPiConfig("GIGACODE_PI_BASE_URL") ??
	hostPiBaseUrl() ??
	SESSION_ENV.ANTHROPIC_BASE_URL;
const PI_SESSION_ENV =
	PI_API_KEY && !("ANTHROPIC_API_KEY" in SESSION_ENV)
		? { ...SESSION_ENV, ANTHROPIC_API_KEY: PI_API_KEY }
		: SESSION_ENV;

function harnessEnvironment(harness: Harness): Record<string, string> {
	if (harness === "pi") return PI_SESSION_ENV;
	if (harness === "opencode") return OPENCODE_SESSION_ENV;
	return SESSION_ENV;
}
const SESSION_LOG_LEVEL = process.env.GIGACODE_LOG_LEVEL ?? "info";

type SessionLog = {
	logger: Logger;
	destination: ReturnType<typeof pino.destination>;
};

const sessionLogs = new Map<string, SessionLog>();

function sessionLog(sessionId: string): SessionLog["logger"] {
	const existing = sessionLogs.get(sessionId);
	if (existing) return existing.logger;
	if (!/^[a-zA-Z0-9_-]+$/.test(sessionId)) {
		throw new Error(
			`session id cannot be used as a log filename: ${sessionId}`,
		);
	}
	if (sessionLogs.size >= MAX_SESSIONS) {
		throw new Error(
			`session logger limit reached (${MAX_SESSIONS}); delete a session or restart the daemon`,
		);
	}
	const destination = pino.destination({
		dest: join(SESSION_LOG_DIR, `${sessionId}.jsonl`),
		mkdir: true,
		sync: false,
	});
	const logger = pino(
		{
			level: SESSION_LOG_LEVEL,
			base: { service: "gigacode", sessionId },
		},
		destination,
	) as Logger;
	sessionLogs.set(sessionId, { logger, destination });
	return logger;
}

function closeSessionLog(sessionId: string): void {
	const entry = sessionLogs.get(sessionId);
	if (!entry) return;
	entry.logger.flush();
	entry.destination.end();
	sessionLogs.delete(sessionId);
}

function closeAllSessionLogs(): void {
	for (const sessionId of [...sessionLogs.keys()]) closeSessionLog(sessionId);
}

type GigacodeClient = any;
type GigacodeHandle = any;

type SessionMeta = {
	id: string;
	actorId?: string;
	directory: string;
	title: string;
	createdAt: number;
	updatedAt: number;
	harness?: Harness;
	model?: string;
	variant?: string;
	actorSessionId?: string;
	/** Volatile: highest durable ACP event applied to the OpenCode transcript. */
	actorSequence?: number;
	/** Volatile: the persisted ACP id has not been attached to this actor VM. */
	needsResume?: boolean;
};
type OpenCodeMessage = {
	info: Record<string, unknown>;
	parts: Record<string, unknown>[];
};
export type PendingStreamChunk = {
	type: "text" | "reasoning";
	messageId?: string;
	afterSequence: number;
	text: string;
};
type PromptJob = {
	body: Record<string, unknown>;
	text: string;
	harness: Harness;
	requestedModel: string;
	requestedModelIsDefault: boolean;
	requestedModelConfigId?: string;
	requestedVariant?: AgentModelVariant & { id: string };
	autoTitle: boolean;
	actorSequenceBaseline?: number;
	appliedActorSequences?: Set<number>;
	lastActorSequence?: number;
	pendingStreamChunks?: PendingStreamChunk[];
	cancelled: boolean;
	started: boolean;
	user: OpenCodeMessage;
	assistant: OpenCodeMessage;
	finished: Promise<void>;
	resolveFinished: () => void;
	abortAfterSessionClose?: () => void;
	resolve: (message: OpenCodeMessage) => void;
};
type PromptQueue = {
	jobs: PromptJob[];
	running: boolean;
};
type PermissionRecord = {
	id: string;
	acpPermissionId: string;
	sessionID: string;
	actorSessionId: string;
	messageID: string;
	createdAt: number;
	description?: string;
	options: Array<Record<string, unknown>>;
	params: Record<string, unknown>;
};
type AgentModel = {
	id: string;
	name: string;
	family?: string;
	variants?: Record<string, AgentModelVariant>;
};
type AgentModelVariant = {
	name: string;
	configId: string;
	value: string;
};
type AgentModelCatalog = {
	defaultModel: string;
	models: AgentModel[];
	modelConfigId?: string;
	variants?: Record<string, AgentModelVariant>;
};
type ModelCache = {
	version: 2;
	updatedAt: number;
	providers: Partial<Record<Harness, AgentModelCatalog>>;
};

class CompatState {
	readonly sessions = new Map<string, SessionMeta>();
	readonly messages = new Map<string, OpenCodeMessage[]>();
	readonly statuses = new Map<string, "idle" | "busy">();
	readonly permissions = new Map<string, PermissionRecord>();
	readonly promptQueues = new Map<string, PromptQueue>();
	readonly cancellationBarriers = new Map<string, Promise<void>>();
	readonly titleJobs = new Set<string>();
	readonly activeShells = new Set<string>();
	readonly activeShellPids = new Map<string, number>();
	readonly eventClients = new Map<
		ServerResponse,
		{ global: boolean; directory: string }
	>();
	readonly events: Array<{ id: number; payload: unknown }> = [];
	messageSaveTail: Promise<void> = Promise.resolve();
	sessionSyncTail?: Promise<void>;
	messagesLoaded = false;
	nextId = 1;

	emit(payload: unknown) {
		const eventId = this.nextId++;
		const identified =
			payload && typeof payload === "object" && !Array.isArray(payload)
				? { ...(payload as Record<string, unknown>), id: String(eventId) }
				: payload;
		const item = { id: eventId, payload: identified };
		this.events.push(item);
		if (this.events.length > MAX_EVENTS) this.events.shift();
		const directory = this.eventDirectory(identified);
		for (const [client, subscription] of this.eventClients) {
			if (!subscription.global && subscription.directory !== directory)
				continue;
			const data = subscription.global
				? { directory, payload: identified }
				: identified;
			client.write(`id: ${item.id}\ndata: ${JSON.stringify(data)}\n\n`);
		}
	}

	replay(
		client: ServerResponse,
		subscription: { global: boolean; directory: string },
		afterId: number,
	): void {
		for (const item of this.events) {
			if (item.id <= afterId) continue;
			const directory = this.eventDirectory(item.payload);
			if (!subscription.global && subscription.directory !== directory)
				continue;
			const data = subscription.global
				? { directory, payload: item.payload }
				: item.payload;
			client.write(`id: ${item.id}\ndata: ${JSON.stringify(data)}\n\n`);
		}
	}

	private eventDirectory(payload: unknown): string {
		if (!payload || typeof payload !== "object") return DEFAULT_DIRECTORY;
		const properties = (payload as { properties?: unknown }).properties;
		if (!properties || typeof properties !== "object") return DEFAULT_DIRECTORY;
		const record = properties as Record<string, unknown>;
		const info =
			record.info && typeof record.info === "object"
				? (record.info as Record<string, unknown>)
				: undefined;
		const sessionId =
			(typeof record.sessionID === "string" && record.sessionID) ||
			(typeof info?.sessionID === "string" && info.sessionID) ||
			(typeof info?.id === "string" && info.id);
		return sessionId
			? (this.sessions.get(sessionId)?.directory ?? DEFAULT_DIRECTORY)
			: DEFAULT_DIRECTORY;
	}
}

async function readLegacySessionMetadata(): Promise<SessionMeta[]> {
	let raw: string;
	try {
		raw = await readFile(LEGACY_SESSION_METADATA_FILE, "utf8");
	} catch (error) {
		if ((error as NodeJS.ErrnoException).code === "ENOENT") return [];
		throw error;
	}
	const value: unknown = JSON.parse(raw);
	if (!Array.isArray(value))
		throw new Error("Gigacode session metadata is not an array");
	const sessions: SessionMeta[] = [];
	for (const entry of value.slice(0, MAX_SESSIONS)) {
		if (!entry || typeof entry !== "object") continue;
		const meta = entry as Partial<SessionMeta>;
		if (
			typeof meta.id !== "string" ||
			typeof meta.directory !== "string" ||
			typeof meta.title !== "string" ||
			typeof meta.createdAt !== "number" ||
			typeof meta.updatedAt !== "number"
		)
			continue;
		sessions.push(meta as SessionMeta);
	}
	return sessions;
}

async function loadMessages(state: CompatState): Promise<void> {
	let raw: string;
	try {
		raw = await readFile(MESSAGE_STORE_FILE, "utf8");
	} catch (error) {
		if ((error as NodeJS.ErrnoException).code === "ENOENT") return;
		throw error;
	}
	if (Buffer.byteLength(raw) > MAX_MESSAGE_STORE_BYTES) {
		throw new Error(
			`Gigacode message store exceeds ${MAX_MESSAGE_STORE_BYTES} bytes; delete old sessions or raise GIGACODE_MAX_MESSAGE_STORE_BYTES`,
		);
	}
	const value: unknown = JSON.parse(raw);
	if (!value || typeof value !== "object" || Array.isArray(value)) {
		throw new Error("Gigacode message store is not an object");
	}
	for (const [sessionId, messages] of Object.entries(value)) {
		if (!state.sessions.has(sessionId) || !Array.isArray(messages)) continue;
		state.messages.set(
			sessionId,
			messages
				.filter((message): message is OpenCodeMessage =>
					Boolean(
						message &&
							typeof message === "object" &&
							(message as OpenCodeMessage).info &&
							Array.isArray((message as OpenCodeMessage).parts),
					),
				)
				.slice(0, MAX_MESSAGES_PER_SESSION),
		);
	}
}

function saveMessages(state: CompatState): Promise<void> {
	const save = state.messageSaveTail.then(async () => {
		const raw = `${JSON.stringify(Object.fromEntries(state.messages), null, 2)}\n`;
		if (Buffer.byteLength(raw) > MAX_MESSAGE_STORE_BYTES) {
			throw new Error(
				`Gigacode message store would exceed ${MAX_MESSAGE_STORE_BYTES} bytes; delete old sessions or raise GIGACODE_MAX_MESSAGE_STORE_BYTES`,
			);
		}
		await mkdir(STATE_DIR, { recursive: true });
		const temporary = `${MESSAGE_STORE_FILE}.${process.pid}.tmp`;
		await writeFile(temporary, raw, { mode: 0o600 });
		await rename(temporary, MESSAGE_STORE_FILE);
	});
	state.messageSaveTail = save.catch(() => undefined);
	return save;
}

function workspaceMount(directory: string) {
	return {
		path: WORKSPACE_MOUNT_PATH,
		plugin: {
			id: "host_dir",
			config: { hostPath: directory, readOnly: false },
		},
		readOnly: false,
	};
}

async function ensureWorkspaceMount(
	handle: GigacodeHandle,
	directory: string,
): Promise<void> {
	const mounts = (await handle.listMounts()) as Array<{ path?: unknown }>;
	if (mounts.some((mount) => mount.path === WORKSPACE_MOUNT_PATH)) return;
	await handle.mountFs(workspaceMount(directory));
}

function canonicalDirectory(directory: string): string {
	const absolute = resolve(directory);
	const canonical = realpathSync(absolute);
	if (!statSync(canonical).isDirectory()) {
		throw new Error(`workspace is not a directory: ${canonical}`);
	}
	return canonical;
}

function workspaceKey(directory: string): string {
	// v2 persists the per-workspace AgentOS options in actor state. The key
	// version ensures actors created before that state existed are replaced.
	return `cwd-v2-${createHash("sha256").update(directory).digest("hex")}`;
}

function boundedPort(name: string, fallback: number): number {
	const raw = process.env[name];
	const value = raw === undefined ? fallback : Number(raw);
	if (!Number.isInteger(value) || value < 1 || value > 65_535) {
		throw new Error(`${name} must be an integer from 1 through 65535`);
	}
	return value;
}

function boundedInteger(name: string, fallback: number): number {
	const raw = process.env[name];
	const value = raw === undefined ? fallback : Number(raw);
	if (!Number.isSafeInteger(value) || value < 1) {
		throw new Error(`${name} must be a positive safe integer`);
	}
	return value;
}

function now() {
	return Date.now();
}

function id(prefix: string) {
	return `${prefix}_${crypto.randomUUID().replaceAll("-", "")}`;
}

async function forEachConcurrent<T>(
	values: readonly T[],
	concurrency: number,
	run: (value: T, index: number) => Promise<void>,
): Promise<void> {
	let nextIndex = 0;
	await Promise.all(
		Array.from({ length: Math.min(concurrency, values.length) }, async () => {
			while (nextIndex < values.length) {
				const index = nextIndex++;
				await run(values[index] as T, index);
			}
		}),
	);
}

function runtimeActorId(meta: SessionMeta): string {
	return meta.actorId ?? meta.id;
}

function sessionFromCoordinatorRow(row: Record<string, unknown>): SessionMeta {
	if (
		typeof row.id !== "string" ||
		typeof row.actorId !== "string" ||
		typeof row.directory !== "string" ||
		typeof row.title !== "string" ||
		typeof row.createdAt !== "number" ||
		typeof row.updatedAt !== "number"
	) {
		throw new Error("Gigacode coordinator returned invalid session metadata");
	}
	return {
		id: row.id,
		actorId: row.actorId,
		directory: row.directory,
		title: row.title,
		createdAt: row.createdAt,
		updatedAt: row.updatedAt,
		...(isHarness(row.harness) ? { harness: row.harness } : {}),
		...(typeof row.model === "string" ? { model: row.model } : {}),
		...(typeof row.actorSessionId === "string"
			? { actorSessionId: row.actorSessionId }
			: {}),
	};
}

// OpenCode stores message and part updates in ID order. Its native IDs encode
// creation time in the first 12 hex characters, so random UUID-based IDs cause
// later SSE updates to be inserted at arbitrary positions in the TUI.
const OPENCODE_ID_LENGTH = 26;
let lastOpenCodeIdTimestamp = 0;
let openCodeIdCounter = 0;

function ascendingOpenCodeId(prefix: "msg" | "prt"): string {
	const timestamp = Date.now();
	if (timestamp !== lastOpenCodeIdTimestamp) {
		lastOpenCodeIdTimestamp = timestamp;
		openCodeIdCounter = 0;
	}
	openCodeIdCounter += 1;
	const encoded = BigInt(timestamp) * 0x1000n + BigInt(openCodeIdCounter);
	const timeBytes = Buffer.alloc(6);
	for (let index = 0; index < timeBytes.length; index += 1) {
		timeBytes[index] = Number((encoded >> BigInt(40 - 8 * index)) & 0xffn);
	}
	const alphabet =
		"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
	const random = randomBytes(OPENCODE_ID_LENGTH - 12);
	let suffix = "";
	for (const byte of random) suffix += alphabet[byte % alphabet.length];
	return `${prefix}_${timeBytes.toString("hex")}${suffix}`;
}

function isHarness(value: unknown): value is Harness {
	return typeof value === "string" && value in harnesses;
}

function json(res: ServerResponse, value: unknown, status = 200) {
	res.writeHead(status, {
		"content-type": "application/json; charset=utf-8",
		"access-control-allow-origin": "*",
		"access-control-allow-headers":
			"content-type,last-event-id,x-opencode-directory",
		"access-control-allow-methods": "GET,POST,PATCH,DELETE,OPTIONS",
	});
	res.end(JSON.stringify(value));
}

function noContent(res: ServerResponse) {
	res.writeHead(204, {
		"access-control-allow-origin": "*",
		"access-control-allow-headers":
			"content-type,last-event-id,x-opencode-directory",
		"access-control-allow-methods": "GET,POST,PATCH,DELETE,OPTIONS",
	});
	res.end();
}

function errorJson(res: ServerResponse, status: number, message: string) {
	json(res, { name: "GigacodeError", data: { message } }, status);
}

async function readJson(
	req: IncomingMessage,
): Promise<Record<string, unknown>> {
	let size = 0;
	const chunks: Buffer[] = [];
	for await (const chunk of req) {
		const buffer = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
		size += buffer.length;
		if (size > MAX_BODY_BYTES)
			throw new Error(`request body exceeds ${MAX_BODY_BYTES} bytes`);
		chunks.push(buffer);
	}
	if (chunks.length === 0) return {};
	const value: unknown = JSON.parse(Buffer.concat(chunks).toString("utf8"));
	if (!value || typeof value !== "object" || Array.isArray(value)) {
		throw new Error("request body must be a JSON object");
	}
	return value as Record<string, unknown>;
}

function directoryFor(req: IncomingMessage, url: URL) {
	const requested =
		url.searchParams.get("directory") ??
		url.searchParams.get("location[directory]") ??
		url.searchParams.get("location.directory") ??
		(typeof req.headers["x-opencode-directory"] === "string"
			? req.headers["x-opencode-directory"]
			: DEFAULT_DIRECTORY);
	return canonicalDirectory(requested);
}

function providerPayload(cache?: ModelCache) {
	const all = Object.entries(harnesses).map(([name, entry]) => ({
		id: name,
		name: entry.label,
		source: "env",
		env: [],
		options: {},
		models: Object.fromEntries(
			(
				cache?.providers[name as Harness]?.models ?? [
					{ id: "default", name: entry.label },
				]
			).map((model) => [
				model.id,
				{
					id: model.id,
					providerID: name,
					name: model.name,
					family: model.family ?? name,
					status: "active",
					api: { id: model.id, url: "", npm: "@ai-sdk/openai-compatible" },
					capabilities: {
						temperature: false,
						reasoning: true,
						attachment: true,
						toolcall: true,
						input: {
							text: true,
							audio: false,
							image: true,
							video: false,
							pdf: true,
						},
						output: {
							text: true,
							audio: false,
							image: false,
							video: false,
							pdf: false,
						},
						interleaved: false,
					},
					cost: { input: 0, output: 0, cache: { read: 0, write: 0 } },
					attachment: true,
					reasoning: true,
					temperature: false,
					tool_call: true,
					limit: { context: 1_000_000, output: 1_000_000 },
					options: {},
					headers: {},
					release_date: "1970-01-01",
					variants: Object.fromEntries(
						Object.keys(
							model.variants ??
								cache?.providers[name as Harness]?.variants ??
								{},
						).map(
							(variant) => [variant, {}],
						),
					),
				},
			]),
		),
	}));
	return {
		all,
		providers: all,
		default: Object.fromEntries(
			Object.keys(harnesses).map((name) => [
				name,
				cache?.providers[name as Harness]?.defaultModel ?? "default",
			]),
		),
		connected: Object.keys(harnesses),
	};
}

function modelOptionValues(option: Record<string, unknown>): AgentModel[] {
	const legacy = Array.isArray(option.allowedValues)
		? option.allowedValues
		: [];
	const legacyModels = legacy.flatMap((value) => {
		if (!value || typeof value !== "object") return [];
		const record = value as Record<string, unknown>;
		if (typeof record.id !== "string") return [];
		return [
			{
				id: record.id,
				name: typeof record.label === "string" ? record.label : record.id,
			},
		];
	});
	if (legacyModels.length > 0) return legacyModels;

	const stable = Array.isArray(option.options) ? option.options : [];
	return stable.flatMap((value) => {
		if (!value || typeof value !== "object") return [];
		const record = value as Record<string, unknown>;
		if (typeof record.value === "string") {
			return [
				{
					id: record.value,
					name: typeof record.name === "string" ? record.name : record.value,
				},
			];
		}
		if (!Array.isArray(record.options)) return [];
		const family =
			typeof record.name === "string"
				? record.name
				: typeof record.group === "string"
					? record.group
					: undefined;
		return record.options.flatMap((nested) => {
			if (!nested || typeof nested !== "object") return [];
			const item = nested as Record<string, unknown>;
			if (typeof item.value !== "string") return [];
			return [
				{
					id: item.value,
					name: typeof item.name === "string" ? item.name : item.value,
					...(family ? { family } : {}),
				},
			];
		});
	});
}

export function catalogFromConfigOptions(
	value: unknown,
): AgentModelCatalog | undefined {
	if (!Array.isArray(value)) return undefined;
	const option = value.find((entry) => {
		if (!entry || typeof entry !== "object") return false;
		const record = entry as Record<string, unknown>;
		return (
			record.category === "model" ||
			record.id === "model" ||
			record.id === "models"
		);
	}) as Record<string, unknown> | undefined;
	if (!option) return undefined;
	const models = modelOptionValues(option);
	const currentValue =
		typeof option.currentValue === "string" ? option.currentValue : undefined;
	if (models.length === 0 && currentValue) {
		models.push({ id: currentValue, name: currentValue });
	}
	if (models.length === 0) return undefined;
	const deduplicated = [
		...new Map(models.map((model) => [model.id, model])).values(),
	];
	// OpenCode can expose each native model variant as an additional model-select
	// value (`provider/model/variant`). Collapse those expanded choices back onto
	// their base model so the OpenCode-compatible provider response has the same
	// per-model `variants` shape as native OpenCode.
	const expandedModelIds = new Set<string>();
	for (const candidate of deduplicated) {
		const base = deduplicated
			.filter(
				(model) =>
					model !== candidate &&
					candidate.id.startsWith(`${model.id}/`) &&
					candidate.name.startsWith(`${model.name} (`) &&
					candidate.name.endsWith(")"),
			)
			.sort((left, right) => right.id.length - left.id.length)[0];
		if (!base) continue;
		const value = candidate.id.slice(base.id.length + 1);
		if (!value) continue;
		base.variants ??= {};
		base.variants[value] = {
			name: candidate.name.slice(base.name.length + 2, -1),
			configId: typeof option.id === "string" ? option.id : "model",
			value: candidate.id,
		};
		expandedModelIds.add(candidate.id);
	}
	const baseModels = deduplicated.filter(
		(model) => !expandedModelIds.has(model.id),
	);
	const variantCandidates = value.flatMap((entry) => {
		if (!entry || typeof entry !== "object") return [];
		const config = entry as Record<string, unknown>;
		const configId = typeof config.id === "string" ? config.id : undefined;
		if (!configId || config.type !== "select") return [];
		if (
			config.category === "model" ||
			config.category === "mode" ||
			configId === "model" ||
			configId === "models" ||
			configId === "mode"
		) {
			return [];
		}
		return modelOptionValues(config).map((selection) => ({
			name: selection.name,
			configId,
			value: selection.id,
		}));
	});
	const variantValueCounts = new Map<string, number>();
	for (const variant of variantCandidates) {
		variantValueCounts.set(
			variant.value,
			(variantValueCounts.get(variant.value) ?? 0) + 1,
		);
	}
	const variants = Object.fromEntries(
		variantCandidates.map((variant) => [
			variantValueCounts.get(variant.value) === 1
				? variant.value
				: `${variant.configId}:${variant.value}`,
			variant,
		]),
	);
	// Selectors such as Claude/Codex effort are uniform across the catalog. Keep
	// the legacy provider-level field for old caches, and materialize it per model
	// for accurate OpenCode provider responses and model-aware lookup.
	for (const model of baseModels) {
		if (!model.variants && Object.keys(variants).length > 0) {
			model.variants = variants;
		}
	}
	return {
		models: baseModels,
		...(typeof option.id === "string" ? { modelConfigId: option.id } : {}),
		...(Object.keys(variants).length > 0 ? { variants } : {}),
		defaultModel:
			currentValue && deduplicated.some((model) => model.id === currentValue)
				? currentValue
				: deduplicated[0].id,
	};
}

const DEFAULT_SESSION_TITLE =
	/^(?:New session - |Child session - )\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/;

const TITLE_GENERATOR_INSTRUCTIONS = `You generate only a brief title for a conversation.
Return one meaningful line in the same language as the user, ideally no more than 50 characters.
Focus on the main topic or requested work. Preserve exact technical terms, numbers, filenames, and HTTP codes.
Do not answer the user, explain the title, mention tools, or use markdown.`;

export function isDefaultSessionTitle(title: string): boolean {
	return DEFAULT_SESSION_TITLE.test(title);
}

export function cleanGeneratedSessionTitle(text: string): string | undefined {
	const cleaned = text
		.replace(/<think>[\s\S]*?<\/think>\s*/g, "")
		.split("\n")
		.map((line) => line.trim())
		.find((line) => line.length > 0);
	if (!cleaned) return undefined;
	return cleaned.length > 100 ? `${cleaned.slice(0, 97)}...` : cleaned;
}

function agentMessageText(payload: unknown): string {
	if (!payload || typeof payload !== "object") return "";
	const wrapper = payload as Record<string, unknown>;
	const event =
		wrapper.event && typeof wrapper.event === "object"
			? (wrapper.event as Record<string, unknown>)
			: undefined;
	const params =
		event?.params && typeof event.params === "object"
			? (event.params as Record<string, unknown>)
			: undefined;
	const legacyUpdate =
		params?.update && typeof params.update === "object"
			? (params.update as Record<string, unknown>)
			: undefined;
	const update =
		event?.method === "session/update"
			? legacyUpdate
			: typeof wrapper.type === "string"
				? wrapper
				: undefined;
	if ((update?.sessionUpdate ?? update?.type) !== "agent_message_chunk") {
		return "";
	}
	return acpContentText(update?.content);
}

function sessionValue(meta: SessionMeta) {
	return {
		id: meta.id,
		slug: `gigacode-${meta.id.slice(0, 8)}`,
		version: VERSION,
		projectID: "gigacode-local",
		directory: meta.directory,
		path: meta.directory.replace(/^\/+/, ""),
		cost: 0,
		tokens: {
			input: 0,
			output: 0,
			reasoning: 0,
			cache: { read: 0, write: 0 },
		},
		title: meta.title,
		time: { created: meta.createdAt, updated: meta.updatedAt },
		...(meta.harness
			? {
					providerID: meta.harness,
					model: {
						id: meta.model ?? "default",
						providerID: meta.harness,
						...(meta.variant ? { variant: meta.variant } : {}),
					},
					agent: "build",
				}
			: {}),
	};
}

function assistantInfo(
	meta: SessionMeta,
	messageId: string,
	parentId: string,
	completed?: number,
) {
	return {
		id: messageId,
		sessionID: meta.id,
		role: "assistant",
		time: { created: now(), ...(completed ? { completed } : {}) },
		parentID: parentId,
		modelID: meta.model ?? "default",
		providerID: meta.harness ?? "claude",
		...(meta.variant ? { variant: meta.variant } : {}),
		mode: "build",
		agent: "build",
		path: { cwd: meta.directory, root: meta.directory },
		cost: 0,
		tokens: { input: 0, output: 0, reasoning: 0, cache: { read: 0, write: 0 } },
		finish: "stop",
	};
}

async function ensureRunnerReady(registryStartedAt: number): Promise<void> {
	const deadline = Date.now() + 30_000;
	const datacentersResponse = await fetch(
		`${RIVET_ENDPOINT}/datacenters?namespace=${encodeURIComponent(RIVET_NAMESPACE)}`,
	);
	if (!datacentersResponse.ok) {
		throw new Error(
			`Rivet datacenter list failed (${datacentersResponse.status}): ${await datacentersResponse.text()}`,
		);
	}
	const datacenters = (await datacentersResponse.json()) as {
		datacenters?: Array<{ name?: string }>;
	};
	const datacenter = datacenters.datacenters?.[0]?.name;
	if (!datacenter) throw new Error("Rivet engine returned no local datacenter");
	let runnerConfigured = false;
	let lastConfigureFailure = "Rivet namespace was not ready";
	while (Date.now() < deadline) {
		const configureResponse = await fetch(
			`${RIVET_ENDPOINT}/runner-configs/${RUNNER_NAME}?namespace=${encodeURIComponent(RIVET_NAMESPACE)}`,
			{
				method: "PUT",
				headers: { "content-type": "application/json" },
				body: JSON.stringify({
					datacenters: { [datacenter]: { normal: {} } },
				}),
			},
		);
		if (configureResponse.ok) {
			runnerConfigured = true;
			break;
		}
		lastConfigureFailure = `Rivet runner configuration failed (${configureResponse.status}): ${await configureResponse.text()}`;
		// Engine health precedes the registry's asynchronous namespace creation.
		// Rivet's own native runtime helper uses this idempotent PUT as the
		// namespace-readiness handshake.
		if (!lastConfigureFailure.includes('"code":"not_found"')) {
			throw new Error(lastConfigureFailure);
		}
		await new Promise((resolve) => setTimeout(resolve, 100));
	}
	if (!runnerConfigured) throw new Error(lastConfigureFailure);

	while (Date.now() < deadline) {
		const response = await fetch(
			`${RIVET_ENDPOINT}/envoys?namespace=${encodeURIComponent(RIVET_NAMESPACE)}&name=${encodeURIComponent(RUNNER_NAME)}`,
		).catch((error) => {
			if (process.env.GIGACODE_DEBUG)
				console.error("waiting for Rivet runner", error);
			return undefined;
		});
		if (response?.ok) {
			const body = (await response.json()) as {
				envoys?: Array<{
					create_ts?: number;
					last_ping_ts?: number;
					stop_ts?: number | null;
				}>;
			};
			// Envoy rows are durable engine state. A row from the previous daemon can
			// briefly survive an engine restart and must not be mistaken for the new
			// in-process runner, or the first actor is scheduled to a dead envoy.
			if (
				body.envoys?.some(
					(envoy) =>
						envoy.stop_ts == null &&
						(envoy.create_ts ?? 0) >= registryStartedAt &&
						(envoy.last_ping_ts ?? 0) >= registryStartedAt,
				)
			)
				return;
		}
		await new Promise((resolve) => setTimeout(resolve, 100));
	}
	throw new Error(`Rivet runner ${RUNNER_NAME} did not become ready`);
}

class RivetRuntime {
	#client?: GigacodeClient;
	#coordinator?: GigacodeHandle;
	#connections = new Map<string, Promise<any>>();
	#eventHandlers = new Map<
		string,
		Map<string, Set<(payload: unknown) => void>>
	>();
	#actorTurnTails = new Map<string, Promise<void>>();
	#probeGate = new ForegroundPriorityGate();
	#registry?: { shutdown(): Promise<void> };
	#ready = false;
	#started?: Promise<void>;
	#startupStage = "waiting to start RivetKit";

	start(): Promise<void> {
		this.#started ??= this.#initialize();
		return this.#started;
	}

	isReady(): boolean {
		return this.#ready;
	}

	startupStage(): string {
		return this.#startupStage;
	}

	#setStartupStage(stage: string, startedAt?: number): void {
		this.#startupStage = stage;
		startupLog(stage, startedAt);
	}

	async client(): Promise<GigacodeClient> {
		await this.start();
		if (!this.#client) throw new Error("Gigacode Rivet client is unavailable");
		return this.#client;
	}

	async workspace(directory: string): Promise<{
		handle: GigacodeHandle;
		actorId: string;
	}> {
		const debugStarted = performance.now();
		const resolved = await this.resolvedWorkspace(directory);
		if (process.env.GIGACODE_DEBUG) {
			console.error(
				`[gigacode] actor handle resolved (${Math.round(performance.now() - debugStarted)} ms)`,
			);
		}
		await ensureWorkspaceMount(resolved.handle, canonicalDirectory(directory));
		if (process.env.GIGACODE_DEBUG) {
			console.error(
				`[gigacode] actor workspace mount ready (${Math.round(performance.now() - debugStarted)} ms cumulative)`,
			);
		}
		return resolved;
	}

	async resolvedWorkspace(directory: string): Promise<{
		handle: GigacodeHandle;
		actorId: string;
	}> {
		const canonical = canonicalDirectory(directory);
		const client = await this.client();
		const handle = client.vm.getOrCreate(workspaceKey(canonical));
		const actorId = await handle.resolve();
		return { handle, actorId };
	}

	async listSessions(): Promise<SessionMeta[]> {
		const rows = (await (
			await this.#coordinatorHandle()
		).listSessions()) as Array<Record<string, unknown>>;
		return rows.map(sessionFromCoordinatorRow);
	}

	connection(directory: string): Promise<any> {
		const canonical = canonicalDirectory(directory);
		let pending = this.#connections.get(canonical);
		if (!pending) {
			pending = (async () => {
				const debugStarted = performance.now();
				const { handle } = await this.workspace(canonical);
				if (process.env.GIGACODE_DEBUG) {
					console.error(
						`[gigacode] actor workspace resolved and mounted (${Math.round(performance.now() - debugStarted)} ms)`,
					);
				}
				const connection = handle.connect();
				try {
					// Rivet connections expose one remote subscription per event name. Calling
					// an individual `connection.on()` disposer can tear down that shared remote
					// subscription even when another callback remains. Install exactly one
					// lifetime listener and fan out locally so completing one turn cannot make
					// later turns on the same workspace silently lose events.
					for (const eventName of ["sessionEvent", "processOutput"] as const) {
						connection.on(eventName, (payload: unknown) => {
							if (
								process.env.GIGACODE_DEBUG &&
								eventName === "sessionEvent"
							) {
								const event = payload as Record<string, unknown> | undefined;
								console.error("[gigacode] received actor session event", {
									sessionId: event?.sessionId,
									type: event?.type,
									sequence: event?.sequence,
								});
							}
							const handlers = this.#eventHandlers
								.get(canonical)
								?.get(eventName);
							for (const handler of [...(handlers ?? [])]) {
								try {
									handler(payload);
								} catch (error) {
									console.error(
										`Gigacode ${eventName} handler failed`,
										error,
									);
								}
							}
						});
					}
					let readyTimer: NodeJS.Timeout | undefined;
					try {
						await Promise.race([
							connection.ready,
							new Promise<never>((_, reject) => {
								readyTimer = setTimeout(
									() =>
										reject(
											new Error(
												`Rivet actor connection did not become ready within ${RIVET_HEALTH_TIMEOUT_MS}ms`,
											),
										),
									RIVET_HEALTH_TIMEOUT_MS,
								);
							}),
						]);
					} finally {
						if (readyTimer) clearTimeout(readyTimer);
					}
					if (process.env.GIGACODE_DEBUG) {
						console.error(
							`[gigacode] actor connection ready (${Math.round(performance.now() - debugStarted)} ms cumulative)`,
						);
					}
					await connection.exists(WORKSPACE_MOUNT_PATH);
					if (process.env.GIGACODE_DEBUG) {
						console.error(
							`[gigacode] actor filesystem probe complete (${Math.round(performance.now() - debugStarted)} ms cumulative)`,
						);
					}
					return connection;
				} catch (error) {
					await connection.dispose().catch(() => undefined);
					throw error;
				}
			})().catch((error) => {
				this.#connections.delete(canonical);
				throw error;
			});
			this.#connections.set(canonical, pending);
		}
		return pending;
	}

	async subscribeEvent(
		directory: string,
		eventName: "sessionEvent" | "processOutput",
		handler: (payload: unknown) => void,
	): Promise<() => void> {
		const canonical = canonicalDirectory(directory);
		await this.connection(canonical);
		let events = this.#eventHandlers.get(canonical);
		if (!events) {
			events = new Map();
			this.#eventHandlers.set(canonical, events);
		}
		let handlers = events.get(eventName);
		if (!handlers) {
			handlers = new Set();
			events.set(eventName, handlers);
		}
		handlers.add(handler);
		return () => {
			handlers?.delete(handler);
			if (handlers?.size === 0) events?.delete(eventName);
			if (events?.size === 0) this.#eventHandlers.delete(canonical);
		};
	}

	async resetConnection(directory: string): Promise<void> {
		const canonical = canonicalDirectory(directory);
		const pending = this.#connections.get(canonical);
		this.#connections.delete(canonical);
		if (!pending) return;
		try {
			await (await pending).dispose();
		} catch {
			// The connection may already be closed when its actor generation changes.
		}
	}

	async withActorTurn<T>(actorId: string, run: () => Promise<T>): Promise<T> {
		const previous = this.#actorTurnTails.get(actorId) ?? Promise.resolve();
		let release = () => {};
		const current = new Promise<void>((resolve) => {
			release = resolve;
		});
		const tail = previous.then(() => current);
		this.#actorTurnTails.set(actorId, tail);
		return await this.#probeGate.foreground(async () => {
			await previous;
			try {
				return await run();
			} finally {
				release();
				if (this.#actorTurnTails.get(actorId) === tail) {
					this.#actorTurnTails.delete(actorId);
				}
			}
		});
	}

	async withModelProbe<T>(run: () => Promise<T>): Promise<T> {
		return await this.#probeGate.background(run);
	}

	async saveSession(meta: SessionMeta): Promise<void> {
		await (await this.#coordinatorHandle()).putSession({
			...meta,
			actorSessionId: meta.actorSessionId ?? null,
			harness: meta.harness ?? null,
			model: meta.model ?? null,
		});
	}

	async deleteSession(sessionId: string): Promise<boolean> {
		return Boolean(
			await (await this.#coordinatorHandle()).deleteSession(sessionId),
		);
	}

	async #coordinatorHandle(): Promise<GigacodeHandle> {
		if (!this.#coordinator) {
			const client = await this.client();
			this.#coordinator = client[COORDINATOR_NAME].getOrCreate(COORDINATOR_KEY);
			await this.#coordinator.resolve();
		}
		return this.#coordinator;
	}

	async shutdown(): Promise<void> {
		const connections = [...this.#connections.values()];
		this.#connections.clear();
		const actorShutdownStarted = performance.now();
		startupLog(`closing ${connections.length} workspace actor connection(s)`);
		const actorShutdownResults = await Promise.allSettled(
			connections.map(async (pending) => {
				const connection = await pending;
				await connection.dispose();
			}),
		);
		for (const result of actorShutdownResults) {
			if (result.status === "rejected") {
				console.error("failed to close workspace actor connection", result.reason);
			}
		}
		startupLog("workspace actor connections are closed", actorShutdownStarted);
		const registryShutdownStarted = performance.now();
		startupLog("draining the RivetKit registry");
		await this.#registry?.shutdown().catch((error) => {
			console.error("failed to stop RivetKit registry", error);
		});
		startupLog("RivetKit registry is drained", registryShutdownStarted);
		// The engine is isolated to this daemon by storage path and port. Stop it
		// only after RivetKit has finished sleeping actors and draining its runner.
		const engineShutdownStarted = performance.now();
		startupLog("stopping the Rivet engine");
		await stopRivetEngine();
		startupLog("Rivet engine is stopped", engineShutdownStarted);
	}

	async #initialize(): Promise<void> {
		const dependencyStarted = performance.now();
		this.#setStartupStage("loading AgentOS and RivetKit modules");
		const [agentos, clientModule, rivetkit, rivetDatabase] = await Promise.all([
			importModule("@rivet-dev/agentos"),
			importModule("@rivet-dev/agentos/client"),
			importModule("rivetkit"),
			importModule("rivetkit/db"),
		]);
		this.#setStartupStage(
			"loaded AgentOS and RivetKit modules",
			dependencyStarted,
		);
		const softwareStarted = performance.now();
		this.#setStartupStage("resolving AgentOS software packages");
		const stableSoftwareDirectory = process.env.GIGACODE_SOFTWARE_DIR;
		const software = stableSoftwareDirectory
			? [
					"coreutils",
					"sed",
					"grep",
					"gawk",
					"findutils",
					"diffutils",
					"tar",
					"gzip",
					"ripgrep",
					"claude-code",
					"codex",
					"opencode",
					"pi",
				].map((name) => ({
					packagePath: join(stableSoftwareDirectory, `${name}.aospkg`),
				}))
			: await Promise.all([
					importModule("@agentos-software/claude-code"),
					importModule("@agentos-software/codex"),
					importModule("@agentos-software/opencode"),
					importModule("@agentos-software/pi"),
					importModule("@agentos-software/ripgrep"),
				]).then((modules) => modules.map((module) => module.default));
		this.#setStartupStage(
			"resolved AgentOS software packages",
			softwareStarted,
		);
		const actorOptions = {
			software,
			defaultSoftware: stableSoftwareDirectory ? false : undefined,
			// OpenCode's generated provider catalog exceeds the default V8 heap
			// during ACP initialization.
			limits: {
				resources: {
					maxFilesystemBytes: MAX_FILESYSTEM_BYTES,
					maxInodeCount: MAX_INODE_COUNT,
				},
				jsRuntime: {
					v8HeapLimitMb: 512,
					// ACP adapters are intentionally long-lived. A cumulative CPU budget
					// eventually terminates a healthy adapter after enough turns.
					cpuTimeLimitMs: 0,
				},
			},
			loopbackExemptPorts: loopbackExemptPorts(),
			// Gigacode is explicitly a trusted local-workspace tool. OpenCode's ACP
			// starts an ephemeral loopback server during initialization, so its VM
			// needs the network permission enabled unless the operator opts out.
			permissions: {
				fs: "allow",
				network: NETWORK_PERMISSION ?? "allow",
				childProcess: "allow",
				process: "allow",
				env: "allow",
				binding: "allow",
			},
			mounts: hostCredentialMounts(),
			...(process.env.GIGACODE_DEBUG
				? {
						onSessionEvent: (
							_c: unknown,
							sessionId: string,
							event: Record<string, unknown>,
						) => {
					console.error("[gigacode] AgentOS actor emitted session event", {
						sessionId,
						type: event.type,
						sequence: event.sequence,
					});
						},
					}
				: {}),
		};
		const coordinator = rivetkit.actor({
			db: rivetDatabase.db({
				onMigrate: async (db: any) => {
					await db.execute(`
						CREATE TABLE IF NOT EXISTS workspaces (
							directory TEXT PRIMARY KEY,
							actor_id TEXT NOT NULL,
							created_at INTEGER NOT NULL,
							updated_at INTEGER NOT NULL
						);
						CREATE UNIQUE INDEX IF NOT EXISTS workspaces_actor_id
							ON workspaces(actor_id);
						CREATE TABLE IF NOT EXISTS sessions (
							id TEXT PRIMARY KEY,
							directory TEXT NOT NULL,
							actor_id TEXT NOT NULL,
							title TEXT NOT NULL,
							created_at INTEGER NOT NULL,
							updated_at INTEGER NOT NULL,
							harness TEXT,
							model TEXT,
							actor_session_id TEXT
						);
						CREATE INDEX IF NOT EXISTS sessions_directory_updated
							ON sessions(directory, updated_at DESC);
					`);
				},
			}),
			actions: {
				listSessions: async (c: any) =>
					await c.db.execute(`
						SELECT id, directory, actor_id AS actorId, title,
							created_at AS createdAt, updated_at AS updatedAt,
							harness, model, actor_session_id AS actorSessionId
						FROM sessions
						ORDER BY updated_at DESC
						LIMIT ${MAX_SESSIONS}
					`),
				putSession: async (c: any, meta: Record<string, unknown>) => {
					const existing = await c.db.execute(
						"SELECT id FROM sessions WHERE id = ? LIMIT 1",
						meta.id,
					);
					if (existing.length === 0) {
						const [{ count = 0 } = {}] = await c.db.execute(
							"SELECT COUNT(*) AS count FROM sessions",
						);
						if (Number(count) >= MAX_SESSIONS) {
							throw new Error(
								`session limit reached (${MAX_SESSIONS}); delete a session before creating another`,
							);
						}
					}
					await c.db.execute(
						`INSERT INTO workspaces (directory, actor_id, created_at, updated_at)
						 VALUES (?, ?, ?, ?)
						 ON CONFLICT(directory) DO UPDATE SET
							actor_id = excluded.actor_id,
							updated_at = excluded.updated_at`,
						meta.directory,
						meta.actorId,
						meta.createdAt,
						meta.updatedAt,
					);
					await c.db.execute(
						`INSERT INTO sessions (
							id, directory, actor_id, title, created_at, updated_at,
							harness, model, actor_session_id
						) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
						ON CONFLICT(id) DO UPDATE SET
							directory = excluded.directory,
							actor_id = excluded.actor_id,
							title = excluded.title,
							updated_at = excluded.updated_at,
							harness = excluded.harness,
							model = excluded.model,
							actor_session_id = excluded.actor_session_id`,
						meta.id,
						meta.directory,
						meta.actorId,
						meta.title,
						meta.createdAt,
						meta.updatedAt,
						meta.harness,
						meta.model,
						meta.actorSessionId,
					);
				},
				deleteSession: async (c: any, sessionId: string) => {
					const deleted = await c.db.execute(
						"DELETE FROM sessions WHERE id = ? RETURNING id",
						sessionId,
					);
					return deleted.length > 0;
				},
			},
		});
		const engineStarted = performance.now();
		this.#setStartupStage(`starting Rivet engine on ${HOST}:${RIVET_PORT}`);
		const vm = agentos.agentOS(actorOptions);
		const catalog = agentos.agentOS(actorOptions);
		const registryStartedAt = Date.now();
		// Rivet's actor-ready and route-dispatch guards are shorter than
		// GigaCode's one-attempt startup budget. Align them so a slow durable wake
		// reports its real result instead of being cut off by an internal guard.
		process.env.RIVET__GUARD__ACTOR_READY_TIMEOUT_MS ??= String(
			RIVET_ACTOR_READY_TIMEOUT_MS,
		);
		process.env.RIVET__GUARD__ROUTE_DISPATCH_TIMEOUT_MS ??= String(
			RIVET_HEALTH_TIMEOUT_MS,
		);
		// GigaCode has one local runner. After a daemon restart there is no other
		// envoy that can accept a durable actor, so Rivet's production-scale stop
		// and exponential reallocation windows only delay that actor's next wake.
		process.env.RIVET__PEGBOARD__ACTOR_STOP_THRESHOLD ??= String(
			RIVET_ACTOR_STOP_THRESHOLD_MS,
		);
		process.env.RIVET__PEGBOARD__RESCHEDULE_BACKOFF_MAX_EXPONENT ??= String(
			RIVET_RESCHEDULE_BACKOFF_MAX_EXPONENT,
		);
		const registry = agentos.setup({
			use: { vm, catalog, coordinator },
			envoy: { totalSlots: 64 },
			startEngine: true,
			engineHost: HOST,
			enginePort: RIVET_PORT,
			shutdown: {
				disableSignalHandlers: true,
				gracePeriodMs: RIVET_SHUTDOWN_TIMEOUT_MS,
			},
		});
		this.#registry = registry;
		try {
			registry.start();
			const deadline = Date.now() + RIVET_HEALTH_TIMEOUT_MS;
			while (Date.now() < deadline) {
				try {
					const response = await fetch(`${RIVET_ENDPOINT}/health`);
					if (response.ok) break;
				} catch (error) {
					if (process.env.GIGACODE_DEBUG)
						console.error("waiting for Rivet engine", error);
				}
				await new Promise((resolve) => setTimeout(resolve, 100));
			}
			const health = await fetch(`${RIVET_ENDPOINT}/health`).catch(
				() => undefined,
			);
			if (!health?.ok)
				throw new Error(
					`Rivet engine did not become ready at ${RIVET_ENDPOINT}`,
				);
			this.#setStartupStage("Rivet engine is healthy", engineStarted);
			const runnerStarted = performance.now();
			this.#setStartupStage("configuring the local Rivet runner");
			await ensureRunnerReady(registryStartedAt);
			this.#setStartupStage("local Rivet runner is ready", runnerStarted);
			this.#client = clientModule.createClient({ endpoint: RIVET_ENDPOINT });
			const coordinatorStarted = performance.now();
			this.#setStartupStage("starting the SQLite session coordinator");
			this.#coordinator =
				this.#client[COORDINATOR_NAME].getOrCreate(COORDINATOR_KEY);
			await this.#coordinator.resolve();
			this.#setStartupStage(
				"SQLite session coordinator is ready",
				coordinatorStarted,
			);
			this.#ready = true;
			this.#setStartupStage("Rivet runtime is ready");
		} catch (error) {
			await Promise.race([
				registry.shutdown().catch((shutdownError: unknown) =>
					console.error("failed to stop RivetKit after startup", shutdownError),
				),
				new Promise<void>((resolve) =>
					setTimeout(resolve, SHUTDOWN_GRACE_MS),
				),
			]);
			await stopRivetEngine();
			this.#registry = undefined;
			this.#setStartupStage(`Rivet startup failed: ${String(error)}`);
			throw error;
		}
	}
}

class GlobalModelCatalog {
	#cache?: ModelCache;
	#loaded?: Promise<void>;
	#refreshing?: Promise<ModelCache>;
	#startupStage = "waiting to load the model catalog";

	constructor(private readonly runtime: RivetRuntime) {}

	async start(): Promise<void> {
		await this.#load();
		if (this.#cache) {
			this.#setStartupStage(
				"model catalog is ready from cache; run 'gigacode models refresh' to update it",
			);
			return;
		}
		try {
			await this.#discoverAndCache();
		} catch (error) {
			this.#setStartupStage(
				`model catalog discovery failed: ${detailedErrorMessage(error)}`,
			);
			throw error;
		}
	}

	startupStage(): string {
		return this.#startupStage;
	}

	#setStartupStage(stage: string, startedAt?: number): void {
		this.#startupStage = stage;
		startupLog(stage, startedAt);
	}

	payload(): ReturnType<typeof providerPayload> {
		// The daemon does not expose this payload until first-start discovery has
		// completed or a valid cache has loaded.
		return providerPayload(this.#cache);
	}

	variant(
		harness: Harness,
		modelId: string,
		variantId: string,
	): (AgentModelVariant & { id: string }) | undefined {
		const catalog = this.#cache?.providers[harness];
		const variant =
			catalog?.models.find((model) => model.id === modelId)?.variants?.[
				variantId
			] ?? catalog?.variants?.[variantId];
		return variant ? { id: variantId, ...variant } : undefined;
	}

	modelConfigId(harness: Harness): string | undefined {
		return this.#cache?.providers[harness]?.modelConfigId;
	}

	defaultModel(harness: Harness): string {
		return this.#cache?.providers[harness]?.defaultModel ?? "default";
	}

	manualRefresh(): Promise<ModelCache> {
		this.#refreshing ??= this.#discoverAndCache()
			.catch((error) => {
				this.#setStartupStage(
					`manual model refresh failed: ${detailedErrorMessage(error)}`,
				);
				throw error;
			})
			.finally(() => {
				this.#refreshing = undefined;
			});
		return this.#refreshing;
	}

	async #load(): Promise<void> {
		this.#loaded ??= (async () => {
			let raw: string;
			try {
				raw = await readFile(MODEL_CACHE_FILE, "utf8");
			} catch (error) {
				if ((error as NodeJS.ErrnoException).code === "ENOENT") {
					this.#setStartupStage(
						"no model cache found; running first-start model discovery",
					);
					return;
				}
				throw error;
			}
			if (Buffer.byteLength(raw) > MAX_MODEL_CACHE_BYTES) {
				console.warn(
					`ignoring oversized Gigacode model cache at ${MODEL_CACHE_FILE}`,
				);
				this.#setStartupStage(
					"model cache is oversized; running first-start model discovery",
				);
				return;
			}
			try {
				const parsed = JSON.parse(raw) as Partial<ModelCache>;
				if (
					parsed.version === 2 &&
					typeof parsed.updatedAt === "number" &&
					parsed.providers &&
					typeof parsed.providers === "object"
				) {
					this.#cache = parsed as ModelCache;
					this.#setStartupStage("loaded the cached model catalog");
				}
			} catch (error) {
				console.warn(`ignoring invalid Gigacode model cache: ${String(error)}`);
				this.#setStartupStage(
					"model cache is invalid; running first-start model discovery",
				);
			}
		})();
		await this.#loaded;
	}

	async #discoverAndCache(): Promise<ModelCache> {
		await this.#load();
		const startedAt = performance.now();
		this.#setStartupStage("discovering models from AgentOS harnesses");
		const client = await this.runtime.client();
		const providers: Partial<Record<Harness, AgentModelCatalog>> = {
			...this.#cache?.providers,
		};
		const entries = Object.entries(harnesses) as Array<
			[Harness, (typeof harnesses)[Harness]]
		>;
		// A serialized refresh reuses one warm disposable actor. On the supported
		// local runtime this is materially faster than cold-starting several actor
		// VMs concurrently; the environment override remains available for hosts
		// where parallel cold starts are cheaper.
		await forEachConcurrent(
			entries,
			MODEL_PROBE_CONCURRENCY,
			async ([agentType, harness], index) => {
				const harnessStartedAt = performance.now();
				startupLog(
					`model refresh ${index + 1}/${entries.length}: probing ${harness.label}`,
				);
				const configOptions = await this.runtime.withModelProbe(async () => {
					const handle = client.catalog.getOrCreate(
						MODEL_PROBE_CONCURRENCY === 1 ? "shared" : agentType,
					);
					if (agentType === "pi") await preparePiSession(handle);
					const probeSessionId = id(`model-probe-${agentType}`);
					let opened = false;
					try {
						await handle.openSession({
							sessionId: probeSessionId,
							agent: agentType,
							cwd: "/",
							env: harnessEnvironment(agentType),
							permissionPolicy: "allow_all",
						});
						opened = true;
						const config = await handle.getSessionConfig({
							sessionId: probeSessionId,
						});
						return config.options;
					} finally {
						if (opened) {
							await handle.deleteSession({ sessionId: probeSessionId });
						}
					}
				});
				const catalog = catalogFromConfigOptions(configOptions) ?? {
					defaultModel: "default",
					models: [{ id: "default", name: harness.label }],
				};
				providers[agentType] = catalog;
				startupLog(
					`model refresh ${index + 1}/${entries.length}: ${harness.label} ready with ${catalog.models.length} models`,
					harnessStartedAt,
				);
			},
		);
		const next: ModelCache = { version: 2, updatedAt: now(), providers };
		await mkdir(STATE_DIR, { recursive: true });
		const temporary = `${MODEL_CACHE_FILE}.${process.pid}.tmp`;
		await writeFile(temporary, `${JSON.stringify(next, null, 2)}\n`, {
			mode: 0o600,
		});
		await rename(temporary, MODEL_CACHE_FILE);
		this.#cache = next;
		this.#setStartupStage("model catalog is ready", startedAt);
		return next;
	}
}

async function syncSessions(state: CompatState, runtime: RivetRuntime) {
	state.sessionSyncTail ??= (async () => {
		let sessions = await runtime.listSessions();
		if (sessions.length === 0) {
			const legacy = await readLegacySessionMetadata();
			if (legacy.length > 0) {
				for (const old of legacy) {
					let directory: string;
					try {
						directory = canonicalDirectory(old.directory);
					} catch (error) {
						console.error(
							`skipping legacy Gigacode session ${old.id}: invalid workspace`,
							error,
						);
						continue;
					}
					const { actorId } = await runtime.workspace(directory);
					const migrated: SessionMeta = {
						...old,
						directory,
						actorId,
					};
					await runtime.saveSession(migrated);
				}
				await rename(
					LEGACY_SESSION_METADATA_FILE,
					`${LEGACY_SESSION_METADATA_FILE}.migrated`,
				);
				console.warn(
					`migrated ${legacy.length} legacy Gigacode sessions into the coordinator actor`,
				);
				sessions = await runtime.listSessions();
			}
		}
		if (sessions.length >= Math.floor(MAX_SESSIONS * 0.9)) {
			console.warn(
				`gigacode session count is near limit (${sessions.length}/${MAX_SESSIONS})`,
			);
		}
		const active = new Set(sessions.map((session) => session.id));
		for (const session of sessions) {
			const cached = state.sessions.get(session.id);
			if (cached) Object.assign(cached, session);
			else {
				session.needsResume = Boolean(session.actorSessionId);
				state.sessions.set(session.id, session);
			}
			state.statuses.set(session.id, state.statuses.get(session.id) ?? "idle");
		}
		for (const sessionId of [...state.sessions.keys()]) {
			if (active.has(sessionId)) continue;
			state.sessions.delete(sessionId);
			state.messages.delete(sessionId);
			state.statuses.delete(sessionId);
		}
		if (!state.messagesLoaded) {
			await loadMessages(state);
			state.messagesLoaded = true;
		}
	})().finally(() => {
		state.sessionSyncTail = undefined;
	});
	await state.sessionSyncTail;
}

function extractText(body: Record<string, unknown>) {
	const parts = Array.isArray(body.parts) ? body.parts : [];
	return parts
		.map((part) => {
			if (!part || typeof part !== "object") return "";
			const value = part as Record<string, unknown>;
			if (typeof value.text === "string") return value.text;
			if (value.type === "file") {
				const source =
					value.source && typeof value.source === "object"
						? (value.source as Record<string, unknown>)
						: undefined;
				const location =
					(typeof source?.path === "string" && source.path) ||
					(typeof value.filename === "string" && value.filename) ||
					(typeof value.url === "string" && value.url);
				return location ? `[Attached file: ${location}]` : "[Attached file]";
			}
			if (value.type === "agent" && typeof value.name === "string") {
				return `[Requested agent: ${value.name}]`;
			}
			if (value.type === "subtask" && typeof value.prompt === "string") {
				return value.prompt;
			}
			return "";
		})
		.filter(Boolean)
		.join("\n");
}

function locationValue(directory: string) {
	return {
		directory,
		project: { id: "gigacode-local", directory },
	};
}

async function findFileSystemEntries(
	directory: string,
	query: string,
	type: string | null,
	requestedLimit: string | null,
) {
	const parsedLimit = Number(requestedLimit);
	const limit =
		Number.isSafeInteger(parsedLimit) && parsedLimit > 0
			? Math.min(parsedLimit, MAX_FS_FIND_RESULTS)
			: Math.min(50, MAX_FS_FIND_RESULTS);
	const root = resolve(directory);
	const pending = [root];
	const matches: Array<{ path: string; type: "file" | "directory" }> = [];
	const needle = query.toLocaleLowerCase();
	const exactBasenameQuery =
		needle.length > 0 && !needle.includes("/") && !needle.includes("\\");
	let scanned = 0;
	const excludedDirectories = new Set([
		".git",
		".jj",
		".turbo",
		"node_modules",
		"target",
	]);
	while (pending.length > 0 && matches.length < limit) {
		const current = pending.shift() as string;
		let entries: Dirent[];
		try {
			entries = await readdir(current, { withFileTypes: true });
		} catch (error) {
			if (current === root) throw error;
			console.warn(
				`skipping unreadable file-search directory ${current}`,
				error,
			);
			continue;
		}
		for (const entry of entries) {
			scanned += 1;
			if (scanned > MAX_FS_FIND_SCAN) {
				console.warn(
					`file search reached ${MAX_FS_FIND_SCAN} entries; returning ${matches.length} partial matches`,
				);
				return matches;
			}
			const isDirectory = entry.isDirectory();
			if (isDirectory && excludedDirectories.has(entry.name)) continue;
			if (!isDirectory && !entry.isFile()) continue;
			const absolute = join(current, entry.name);
			const relative = absolute.slice(root.length + 1);
			if (isDirectory) pending.push(absolute);
			const entryType = isDirectory ? "directory" : "file";
			if (type && type !== entryType) continue;
			if (needle && !relative.toLocaleLowerCase().includes(needle)) continue;
			matches.push({ path: relative, type: entryType });
			// A basename query is the common editor/TUI lookup. Once its exact file or
			// directory exists, continuing solely to fill the maximum result count can
			// traverse enormous generated trees without improving the answer.
			if (exactBasenameQuery && entry.name.toLocaleLowerCase() === needle) {
				return matches;
			}
			if (matches.length >= limit) break;
		}
	}
	return matches;
}

function selectedHarness(
	body: Record<string, unknown>,
	fallback?: Harness,
): Harness {
	const model =
		body.model && typeof body.model === "object"
			? (body.model as Record<string, unknown>)
			: {};
	const candidate = body.providerID ?? model.providerID ?? body.agent;
	if (isHarness(candidate)) return candidate;
	return fallback ?? "claude";
}

function selectedModel(
	body: Record<string, unknown>,
	fallback?: string,
): string {
	const model =
		body.model && typeof body.model === "object"
			? (body.model as Record<string, unknown>)
			: {};
	const candidate = body.modelID ?? model.modelID;
	return typeof candidate === "string" && candidate
		? candidate
		: (fallback ?? "default");
}

function selectedVariant(body: Record<string, unknown>): string | undefined {
	return typeof body.variant === "string" && body.variant
		? body.variant
		: undefined;
}

async function preparePiSession(handle: GigacodeHandle): Promise<void> {
	if (!PI_API_KEY && !PI_BASE_URL) return;
	const agentDir = "/home/agentos/.pi/agent";
	await handle.mkdir(agentDir);
	await handle.writeFile(
		`${agentDir}/models.json`,
		`${JSON.stringify({
			providers: {
				anthropic: {
					...(PI_BASE_URL ? { baseUrl: PI_BASE_URL } : {}),
					...(PI_API_KEY ? { apiKey: PI_API_KEY } : {}),
				},
			},
		})}\n`,
	);
}

class TurnCancelledError extends Error {
	constructor() {
		super("The user cancelled this turn");
		this.name = "TurnCancelledError";
	}
}

function structuredUnknownError(error: unknown) {
	return {
		name: "UnknownError",
		data: { message: error instanceof Error ? error.message : String(error) },
	};
}

function detailedErrorMessage(error: unknown): string {
	if (!(error instanceof Error)) return String(error);
	const record = error as Error & {
		cause?: unknown;
		code?: unknown;
		group?: unknown;
		metadata?: unknown;
	};
	const details = [
		error.message,
		record.group || record.code
			? `code=${String(record.group ?? "unknown")}/${String(record.code ?? "unknown")}`
			: "",
		record.metadata && Object.keys(record.metadata as object).length > 0
			? `metadata=${JSON.stringify(record.metadata)}`
			: "",
		record.cause ? `cause=${detailedErrorMessage(record.cause)}` : "",
	].filter(Boolean);
	return details.join("; ");
}

function structuredAbortedError() {
	return {
		name: "MessageAbortedError",
		data: { message: "The user cancelled this turn" },
	};
}

function updateAssistantFailure(
	state: CompatState,
	meta: SessionMeta,
	job: PromptJob,
	error: unknown,
): void {
	const info = job.assistant.info;
	const completed = now();
	info.time = {
		...((info.time as Record<string, unknown> | undefined) ?? {}),
		completed,
	};
	delete info.finish;
	info.error =
		error instanceof TurnCancelledError
			? structuredAbortedError()
			: structuredUnknownError(error);
	for (const part of job.assistant.parts) {
		if (
			error instanceof TurnCancelledError &&
			part.type === "tool" &&
			part.state &&
			typeof part.state === "object"
		) {
			const toolState = part.state as Record<string, unknown>;
			if (toolState.status === "pending" || toolState.status === "running") {
				const metadata =
					toolState.metadata && typeof toolState.metadata === "object"
						? (toolState.metadata as Record<string, unknown>)
						: {};
				const time =
					toolState.time && typeof toolState.time === "object"
						? (toolState.time as Record<string, unknown>)
						: {};
				part.state = {
					status: "error",
					input:
						toolState.input && typeof toolState.input === "object"
							? toolState.input
							: {},
					error: "Tool execution aborted",
					metadata: { ...metadata, interrupted: true },
					time: { start: time.start ?? completed, end: completed },
				};
				state.emit({
					type: "message.part.updated",
					properties: { sessionID: meta.id, part, time: completed },
				});
			}
		}
		if (part.time && typeof part.time === "object") {
			part.time = { ...(part.time as Record<string, unknown>), end: completed };
		}
	}
	state.emit({
		type: "message.updated",
		properties: { sessionID: meta.id, info },
	});
	state.emit({
		type: "session.error",
		properties: { sessionID: meta.id, error: info.error },
	});
}

function enqueuePrompt(
	state: CompatState,
	runtime: RivetRuntime,
	modelCatalog: GlobalModelCatalog,
	meta: SessionMeta,
	body: Record<string, unknown>,
): { accepted: Promise<void>; completion: Promise<OpenCodeMessage> } {
	const text = extractText(body);
	if (!text) throw new Error("prompt must contain at least one text part");
	if (state.activeShells.has(meta.id)) {
		throw new Error("session is busy running a shell command");
	}
	const existingQueue = state.promptQueues.get(meta.id);
	if ((existingQueue?.jobs.length ?? 0) >= MAX_PROMPT_QUEUE_PER_SESSION) {
		throw new Error(
			`prompt queue reached ${MAX_PROMPT_QUEUE_PER_SESSION} turns; wait for work to finish or raise GIGACODE_MAX_PROMPT_QUEUE_PER_SESSION`,
		);
	}
	const harness = selectedHarness(
		body,
		existingQueue?.jobs.at(-1)?.harness ?? meta.harness,
	);
	if (meta.harness && meta.harness !== harness) {
		throw new Error(
			`Gigacode session ${meta.id} uses the ${meta.harness} ACP harness and cannot switch to ${harness}; create a new session for ${harness}`,
		);
	}
	if (!meta.harness) meta.harness = harness;
	const requestedModel = selectedModel(
		body,
		existingQueue?.jobs.at(-1)?.requestedModel ?? meta.model,
	);
	const requestedVariantId =
		selectedVariant(body) ??
		existingQueue?.jobs.at(-1)?.requestedVariant?.id ??
		meta.variant;
	const requestedVariant = requestedVariantId
		? modelCatalog.variant(harness, requestedModel, requestedVariantId)
		: undefined;
	if (requestedVariantId && !requestedVariant) {
		throw new Error(`unknown ${harness} model variant: ${requestedVariantId}`);
	}
	meta.updatedAt = now();

	const messages = state.messages.get(meta.id) ?? [];
	const autoTitle =
		isDefaultSessionTitle(meta.title) &&
		!state.titleJobs.has(meta.id) &&
		!messages.some((message) => message.info.role === "user");
	if (autoTitle) state.titleJobs.add(meta.id);
	if (messages.length + 2 > MAX_MESSAGES_PER_SESSION) {
		throw new Error(
			`session reached ${MAX_MESSAGES_PER_SESSION} messages; create a new session or raise GIGACODE_MAX_MESSAGES_PER_SESSION`,
		);
	}
	const userId =
		typeof body.messageID === "string"
			? body.messageID
			: ascendingOpenCodeId("msg");
	const userPart = {
		id: ascendingOpenCodeId("prt"),
		sessionID: meta.id,
		messageID: userId,
		type: "text",
		text,
	};
	const user: OpenCodeMessage = {
		info: {
			id: userId,
			sessionID: meta.id,
			role: "user",
			time: { created: now() },
			agent: "build",
			model: {
				providerID: harness,
				modelID: requestedModel,
				...(requestedVariant ? { variant: requestedVariant.id } : {}),
			},
		},
		parts: [userPart],
	};
	const assistantId = ascendingOpenCodeId("msg");
	const assistantInfoValue = assistantInfo(meta, assistantId, userId) as Record<
		string,
		unknown
	>;
	assistantInfoValue.providerID = harness;
	assistantInfoValue.modelID = requestedModel;
	if (requestedVariant) assistantInfoValue.variant = requestedVariant.id;
	delete assistantInfoValue.finish;
	const assistant: OpenCodeMessage = {
		info: assistantInfoValue,
		parts: [],
	};

	let resolveCompletion!: (message: OpenCodeMessage) => void;
	let resolveFinished!: () => void;
	const completion = new Promise<OpenCodeMessage>((resolve) => {
		resolveCompletion = resolve;
	});
	const finished = new Promise<void>((resolve) => {
		resolveFinished = resolve;
	});
	const job: PromptJob = {
		body,
		text,
		harness,
		requestedModel,
		requestedModelIsDefault:
			requestedModel === modelCatalog.defaultModel(harness),
		requestedModelConfigId: modelCatalog.modelConfigId(harness),
		requestedVariant,
		autoTitle,
		cancelled: false,
		started: false,
		user,
		assistant,
		finished,
		resolveFinished,
		resolve: resolveCompletion,
	};
	const queue = existingQueue ?? { jobs: [], running: false };
	queue.jobs.push(job);
	state.promptQueues.set(meta.id, queue);
	messages.push(user);
	state.messages.set(meta.id, messages);
	state.emit({
		type: "message.updated",
		properties: { sessionID: meta.id, info: user.info },
	});
	state.emit({
		type: "message.part.updated",
		properties: { sessionID: meta.id, part: userPart, time: now() },
	});
	if (state.statuses.get(meta.id) !== "busy") {
		state.statuses.set(meta.id, "busy");
		state.emit({
			type: "session.status",
			properties: { sessionID: meta.id, status: { type: "busy" } },
		});
	}
	const accepted = Promise.all([
		runtime.saveSession(meta),
		saveMessages(state),
	]).then(() => undefined);
	if (!queue.running) {
		queue.running = true;
		void drainPromptQueue(state, runtime, meta, queue);
	}
	return { accepted, completion };
}

async function activatePromptAssistant(
	state: CompatState,
	meta: SessionMeta,
	job: PromptJob,
): Promise<void> {
	const messages = state.messages.get(meta.id) ?? [];
	if (messages.includes(job.assistant)) return;
	const userIndex = messages.indexOf(job.user);
	if (userIndex === -1) {
		throw new Error(`queued user message is missing for session ${meta.id}`);
	}
	messages.splice(userIndex + 1, 0, job.assistant);
	state.messages.set(meta.id, messages);
	state.emit({
		type: "message.updated",
		properties: { sessionID: meta.id, info: job.assistant.info },
	});
	await saveMessages(state);
}

async function drainPromptQueue(
	state: CompatState,
	runtime: RivetRuntime,
	meta: SessionMeta,
	queue: PromptQueue,
): Promise<void> {
	try {
		while (queue.jobs.length > 0) {
			const job = queue.jobs[0] as PromptJob;
			job.started = true;
			try {
				await activatePromptAssistant(state, meta, job);
				if (job.cancelled) throw new TurnCancelledError();
				await runPrompt(state, runtime, meta, job);
			} catch (error) {
				const normalized = job.cancelled ? new TurnCancelledError() : error;
				updateAssistantFailure(state, meta, job, normalized);
				if (!(normalized instanceof TurnCancelledError)) {
					sessionLog(meta.id).error(
						{ event: "prompt.failed", err: normalized },
						"Gigacode prompt failed",
					);
				}
			} finally {
				queue.jobs.shift();
				await saveMessages(state).catch((error) =>
					console.error(`failed to persist messages for ${meta.id}`, error),
				);
				job.resolve(job.assistant);
				job.resolveFinished();
			}
		}
	} finally {
		let quiescent = true;
		const cancellationBarrier = state.cancellationBarriers.get(meta.id);
		if (cancellationBarrier) {
			try {
				await cancellationBarrier;
				if (state.cancellationBarriers.get(meta.id) === cancellationBarrier) {
					state.cancellationBarriers.delete(meta.id);
				}
			} catch (error) {
				quiescent = false;
				sessionLog(meta.id).error(
					{ event: "agentos.turn.cancel_barrier_failed", err: error },
					"ACP cancellation barrier failed",
				);
			}
		}
		queue.running = false;
		if (state.promptQueues.get(meta.id) === queue) {
			state.promptQueues.delete(meta.id);
		}
		if (quiescent && state.sessions.has(meta.id)) {
			state.statuses.set(meta.id, "idle");
			state.emit({
				type: "session.status",
				properties: { sessionID: meta.id, status: { type: "idle" } },
			});
			state.emit({
				type: "session.idle",
				properties: { sessionID: meta.id },
			});
			sessionLog(meta.id).info({ event: "session.idle" });
		}
	}
}

async function abortPromptQueue(
	state: CompatState,
	runtime: RivetRuntime,
	meta: SessionMeta,
): Promise<boolean> {
	const queue = state.promptQueues.get(meta.id);
	const jobs = [...(queue?.jobs ?? [])];
	for (const job of jobs) {
		job.cancelled = true;
	}
	for (const [permissionId, permission] of state.permissions) {
		if (permission.sessionID !== meta.id) continue;
		state.permissions.delete(permissionId);
		state.emit({
			type: "permission.replied",
			properties: {
				sessionID: meta.id,
				requestID: permissionId,
				reply: "reject",
			},
		});
	}
	const active =
		Boolean(jobs.some((job) => job.started)) || state.activeShells.has(meta.id);
	if (active && !state.cancellationBarriers.has(meta.id)) {
		let containedCancellationError: unknown;
		const log = sessionLog(meta.id);
		const handle = await resolveActiveSessionControlHandle(runtime, meta);
		const actorId = runtimeActorId(meta);
		const actorSessionId = meta.actorSessionId;
		const shellPid = state.activeShellPids.get(meta.id);
		const barrier = (async () => {
			let cancelledQuiescently = false;
			if (shellPid !== undefined) await handle.killProcess(shellPid);

			if (jobs.some((job) => job.started)) {
				const staleActorSessionId = meta.actorSessionId;
				if (staleActorSessionId) {
					let timer: NodeJS.Timeout | undefined;
					try {
						await Promise.race([
							(async () => {
								const cancelResponse = await handle.cancelPrompt({
									sessionId: staleActorSessionId,
								});
								if (
									cancelResponse?.status !== "cancelled" &&
									cancelResponse?.status !== "no_active_prompt"
								) {
									throw new Error(
										"AgentOS cancellation returned an invalid status",
									);
								}
								// The prompt result is the durable terminal-commit barrier. Keep its
								// listener alive so idle is never emitted before AgentOS is quiescent.
								await Promise.all(jobs.map((job) => job.finished));
							})(),
							new Promise<never>((_, reject) => {
								timer = setTimeout(
									() =>
										reject(
											new Error("ACP cancellation did not quiesce in time"),
										),
									CANCEL_QUIESCE_TIMEOUT_MS,
								);
							}),
						]);
						cancelledQuiescently = true;
					} catch (error) {
						containedCancellationError = error;
						log.warn({
							event: "agentos.turn.cancel_not_quiescent",
							actorSessionId: staleActorSessionId,
							err: error,
						});
					} finally {
						if (timer) clearTimeout(timer);
					}
				}

				// ACP cancellation currently acknowledges before every adapter has stopped
				// producing tool calls. Always unload the live adapter after a cancelled
				// turn; the next prompt resumes AgentOS's durable logical session.
				if (staleActorSessionId) {
					try {
						await handle.unloadSession({ sessionId: staleActorSessionId });
						meta.needsResume = true;
						await runtime.saveSession(meta);
						containedCancellationError = undefined;
					} catch (error) {
						containedCancellationError = error;
					}
				}
				for (const job of jobs) job.abortAfterSessionClose?.();
				await Promise.all(jobs.map((job) => job.finished));
				log.warn({
					event: "agentos.session.unloaded_after_cancel",
					actorSessionId: staleActorSessionId,
					promptListenerFinished: cancelledQuiescently,
				});
			}

			sessionLog(meta.id).info({
				event: "agentos.turn.cancelled",
				actorSessionId,
				shellPid,
				via: jobs.some((job) => job.started)
					? "session-unloaded"
					: "process-killed",
			});
		})();
		state.cancellationBarriers.set(meta.id, barrier);
		sessionLog(meta.id).info({
			event: "agentos.turn.cancel_started",
			actorId,
			actorSessionId,
			shellPid,
		});
		await barrier;
		if (containedCancellationError) {
			throw new Error(
				`AgentOS cancellation did not terminate cleanly; the ACP session could not be unloaded safely: ${detailedErrorMessage(containedCancellationError)}`,
				{ cause: containedCancellationError },
			);
		}
	} else if (active) {
		await state.cancellationBarriers.get(meta.id);
	}
	return active || Boolean(queue?.jobs.length);
}

function acpContentText(value: unknown): string {
	if (!value || typeof value !== "object") return "";
	const content = value as Record<string, unknown>;
	return typeof content.text === "string" ? content.text : "";
}

function acpToolOutput(value: unknown): string {
	if (!Array.isArray(value)) return "";
	return value
		.map((item) => {
			if (!item || typeof item !== "object") return "";
			const record = item as Record<string, unknown>;
			return acpContentText(record.content) || acpContentText(record);
		})
		.filter(Boolean)
		.join("\n");
}

function acpRawToolOutput(value: unknown): {
	output?: string;
	metadata?: Record<string, unknown>;
} {
	if (!value || typeof value !== "object" || Array.isArray(value)) return {};
	const result = value as Record<string, unknown>;
	return {
		...(typeof result.output === "string" ? { output: result.output } : {}),
		...(result.metadata &&
		typeof result.metadata === "object" &&
		!Array.isArray(result.metadata)
			? { metadata: result.metadata as Record<string, unknown> }
			: {}),
	};
}

function openCodeToolInput(value: Record<string, unknown>): Record<string, unknown> {
	return Object.fromEntries(
		Object.entries(value).map(([key, item]) => [
			key.replace(/_([a-z])/g, (_, letter: string) => letter.toUpperCase()),
			item,
		]),
	);
}

function acpDiffContent(value: unknown): Record<string, unknown> | undefined {
	if (!Array.isArray(value)) return undefined;
	return value.find(
		(item): item is Record<string, unknown> =>
			Boolean(item) && typeof item === "object" && item.type === "diff",
	);
}

function openCodePatchText(
	input: Record<string, unknown>,
	update: Record<string, unknown>,
): string | undefined {
	if (typeof input.patchText === "string") return input.patchText;
	if (typeof input.filePath !== "string") return undefined;
	const diff = acpDiffContent(update.content);
	const oldText = typeof diff?.oldText === "string" ? diff.oldText : undefined;
	const newText =
		typeof diff?.newText === "string"
			? diff.newText
			: typeof input.content === "string"
				? input.content
				: undefined;
	if (newText === undefined) return undefined;
	if (oldText === undefined) {
		return [
			"*** Begin Patch",
			`*** Add File: ${input.filePath}`,
			...newText.split("\n").map((line) => `+${line}`),
			"*** End Patch",
		].join("\n");
	}
	return [
		"*** Begin Patch",
		`*** Update File: ${input.filePath}`,
		"@@",
		...oldText.split("\n").map((line) => `-${line}`),
		...newText.split("\n").map((line) => `+${line}`),
		"*** End Patch",
	].join("\n");
}

function lineCount(value: string): number {
	return value.length === 0 ? 0 : value.split("\n").length;
}

function openCodePatchResult(
	input: Record<string, unknown>,
	update: Record<string, unknown>,
): { output: string; metadata: Record<string, unknown> } | undefined {
	if (typeof input.filePath !== "string") return undefined;
	const diff = acpDiffContent(update.content);
	const oldText = typeof diff?.oldText === "string" ? diff.oldText : "";
	const newText =
		typeof diff?.newText === "string"
			? diff.newText
			: typeof input.content === "string"
				? input.content
				: "";
	const type = diff?.oldText == null ? "add" : "update";
	const relativePath = input.filePath.replace(/^\//, "");
	const unifiedDiff = [
		`Index: ${input.filePath}`,
		"===================================================================",
		`--- ${input.filePath}`,
		`+++ ${input.filePath}`,
		`@@ -1,${lineCount(oldText)} +1,${lineCount(newText)} @@`,
		...oldText.split("\n").filter(Boolean).map((line) => `-${line}`),
		...newText.split("\n").filter(Boolean).map((line) => `+${line}`),
		"",
	].join("\n");
	const action = type === "add" ? "A" : "M";
	return {
		output: `Success. Updated the following files:\n${action} ${relativePath}`,
		metadata: {
			diff: unifiedDiff,
			files: [
				{
					filePath: input.filePath,
					relativePath,
					type,
					patch: unifiedDiff.trimEnd(),
					additions: lineCount(newText),
					deletions: lineCount(oldText),
				},
			],
			diagnostics: {},
			truncated: false,
		},
	};
}

function openCodeToolName(
	kind: string,
	title: string,
	input: Record<string, unknown>,
): string {
	const operation = title.trim().split(/\s+/, 1)[0]?.toLowerCase();
	if (kind === "execute") return "bash";
	if (kind === "edit") {
		if (typeof input.patchText === "string") return "apply_patch";
		// ACP omits a canonical tool name, but filePath + content is OpenCode's
		// native write contract. Preserve it instead of manufacturing a patch.
		if (typeof input.filePath === "string" && typeof input.content === "string")
			return "write";
		if (operation === "write") return "write";
		if (operation === "edit") return "edit";
		return "oldString" in input || "newString" in input ? "edit" : "write";
	}
	if (kind === "search") return operation === "grep" ? "grep" : "glob";
	if (kind === "fetch") return "query" in input ? "websearch" : "webfetch";
	if (kind === "other") return "tool";
	return kind;
}

function openCodeToolTitle(
	tool: string,
	title: string,
	input: Record<string, unknown>,
): string {
	if (tool === "bash" && typeof input.command === "string") return input.command;
	if (
		["write", "edit", "read"].includes(tool) &&
		typeof input.filePath === "string"
	) {
		return input.filePath.replace(/^\//, "");
	}
	return title;
}

function openCodeToolOutput(tool: string, output: string): string {
	if (tool !== "bash") return output;
	const consoleBlock = output.match(/^```console\n([\s\S]*?)\n```$/);
	return consoleBlock?.[1] ?? output;
}

function acpTerminalResult(update: Record<string, unknown>): {
	output?: string;
	exit?: number;
} {
	const meta =
		update._meta && typeof update._meta === "object"
			? (update._meta as Record<string, unknown>)
			: undefined;
	const terminalOutput =
		meta?.terminal_output && typeof meta.terminal_output === "object"
			? (meta.terminal_output as Record<string, unknown>)
			: undefined;
	const terminalExit =
		meta?.terminal_exit && typeof meta.terminal_exit === "object"
			? (meta.terminal_exit as Record<string, unknown>)
			: undefined;
	return {
		...(typeof terminalOutput?.data === "string"
			? { output: terminalOutput.data }
			: {}),
		...(typeof terminalExit?.exit_code === "number"
			? { exit: terminalExit.exit_code }
			: {}),
	};
}

function openCodeToolMetadata(
	tool: string,
	input: Record<string, unknown>,
	update: Record<string, unknown>,
	output: string,
	previous: Record<string, unknown>,
): Record<string, unknown> {
	const terminal = acpTerminalResult(update);
	const native = acpRawToolOutput(update.rawOutput);
	if (tool === "bash") {
		const previousOutput =
			typeof previous.output === "string" ? previous.output : "";
		const previousExit =
			typeof previous.exit === "number" ? previous.exit : undefined;
		return {
			...native.metadata,
			// Some adapters emit the command output before a terminal update whose
			// output field is present but empty. Do not erase the earlier payload.
			output: terminal.output || native.output || output || previousOutput,
			...(terminal.exit !== undefined || previousExit !== undefined
				? { exit: terminal.exit ?? previousExit }
				: typeof native.metadata?.exit === "number"
					? { exit: native.metadata.exit }
				: update.status === "completed"
					? { exit: 0 }
					: {}),
			truncated: false,
		};
	}
	if (tool === "apply_patch") {
		return {
			...openCodePatchResult(input, update)?.metadata,
			...native.metadata,
		};
	}
	const diff = acpDiffContent(update.content);
	if (tool === "write") {
		return {
			diagnostics: {},
			...(typeof input.filePath === "string"
				? { filepath: input.filePath }
				: {}),
			exists: diff ? diff.oldText !== null : false,
			truncated: false,
		};
	}
	if (tool === "edit" && diff) {
		const path =
			typeof diff.path === "string"
				? diff.path
				: typeof input.filePath === "string"
					? input.filePath
					: "file";
		const oldText = typeof diff.oldText === "string" ? diff.oldText : "";
		const newText = typeof diff.newText === "string" ? diff.newText : "";
		const removedText =
			typeof input.oldString === "string" ? input.oldString : oldText;
		const addedText =
			typeof input.newString === "string" ? input.newString : newText;
		const patch = `--- ${path}\n+++ ${path}\n@@\n-${oldText}\n+${newText}\n`;
		return {
			diagnostics: {},
			diff: patch,
			filediff: {
				file: path,
				patch,
				additions: addedText ? addedText.split("\n").length : 0,
				deletions: removedText ? removedText.split("\n").length : 0,
			},
			truncated: false,
		};
	}
	return { ...previous, ...(output ? { output } : {}) };
}

function updateStreamingPart(
	state: CompatState,
	meta: SessionMeta,
	job: PromptJob,
	type: "text" | "reasoning",
	delta: string,
): void {
	if (!delta) return;
	const assistantId = job.assistant.info.id as string;
	let part = job.assistant.parts.find((candidate) => candidate.type === type);
	if (!part) {
		part = {
			id: ascendingOpenCodeId("prt"),
			sessionID: meta.id,
			messageID: assistantId,
			type,
			text: "",
			time: { start: now() },
		};
		job.assistant.parts.push(part);
	}
	part.text = `${typeof part.text === "string" ? part.text : ""}${delta}`;
	state.emit({
		type: "message.part.delta",
		properties: {
			sessionID: meta.id,
			messageID: assistantId,
			partID: part.id,
			field: "text",
			delta,
		},
	});
}

export function reconcileSessionStreamDelta(
	pending: PendingStreamChunk[],
	wrapper: Record<string, unknown>,
	update: Record<string, unknown>,
	type: "text" | "reasoning",
	delta: string,
): string {
	const messageId =
		typeof update.messageId === "string" ? update.messageId : undefined;
	if (
		wrapper.durability === "ephemeral" &&
		typeof wrapper.afterSequence === "number"
	) {
		const existing = pending.find(
			(chunk) =>
				chunk.type === type &&
				chunk.messageId === messageId &&
				chunk.afterSequence === wrapper.afterSequence,
		);
		if (existing) existing.text += delta;
		else {
			pending.push({
				type,
				messageId,
				afterSequence: wrapper.afterSequence,
				text: delta,
			});
		}
		return delta;
	}
	if (
		wrapper.durability !== "durable" ||
		typeof wrapper.sequence !== "number"
	) {
		return delta;
	}
	const sequence = wrapper.sequence;
	const matches = pending.filter(
		(chunk) =>
			chunk.type === type &&
			chunk.messageId === messageId &&
			chunk.afterSequence < sequence,
	);
	if (matches.length === 0) return delta;
	const matched = new Set(matches);
	for (let index = pending.length - 1; index >= 0; index--) {
		if (matched.has(pending[index])) pending.splice(index, 1);
	}
	const ephemeralText = matches
		.sort((left, right) => left.afterSequence - right.afterSequence)
		.map((chunk) => chunk.text)
		.join("");
	const limit = Math.min(ephemeralText.length, delta.length);
	for (let overlap = limit; overlap > 0; overlap--) {
		if (ephemeralText.endsWith(delta.slice(0, overlap))) {
			return delta.slice(overlap);
		}
	}
	return delta;
}

function updateStreamingTool(
	state: CompatState,
	meta: SessionMeta,
	job: PromptJob,
	update: Record<string, unknown>,
): void {
	const callId =
		typeof update.toolCallId === "string" && update.toolCallId
			? update.toolCallId
			: id("call");
	let part = job.assistant.parts.find(
		(candidate) => candidate.type === "tool" && candidate.callID === callId,
	);
	const timestamp = now();
	const previousState =
		part?.state && typeof part.state === "object"
			? (part.state as Record<string, unknown>)
			: undefined;
	const previousPartMetadata =
		part?.metadata && typeof part.metadata === "object"
			? (part.metadata as Record<string, unknown>)
			: {};
	const previousAcpMetadata =
		previousPartMetadata.acp && typeof previousPartMetadata.acp === "object"
			? (previousPartMetadata.acp as Record<string, unknown>)
			: {};
	const acpInput = openCodeToolInput(
		update.rawInput && typeof update.rawInput === "object"
			? (update.rawInput as Record<string, unknown>)
			: previousAcpMetadata.rawInput &&
					typeof previousAcpMetadata.rawInput === "object"
				? (previousAcpMetadata.rawInput as Record<string, unknown>)
				: ((previousState?.input as Record<string, unknown> | undefined) ?? {}),
	);
	const title =
		typeof update.title === "string"
			? update.title
			: typeof previousAcpMetadata.title === "string"
				? previousAcpMetadata.title
				: typeof previousState?.title === "string"
					? previousState.title
					: "tool";
	const kind =
		typeof update.kind === "string" && update.kind
			? update.kind
			: typeof previousAcpMetadata.kind === "string" && previousAcpMetadata.kind
				? previousAcpMetadata.kind
				: "other";
	const tool = openCodeToolName(kind, title, acpInput);
	const input =
		tool === "apply_patch"
			? {
					patchText:
						openCodePatchText(acpInput, update) ??
						(typeof acpInput.patchText === "string" ? acpInput.patchText : ""),
				}
			: acpInput;
	const displayTitle = openCodeToolTitle(tool, title, input);
	const started =
		(previousState?.time as { start?: number } | undefined)?.start ?? timestamp;
	const terminal = acpTerminalResult(update);
	const rawResult = acpRawToolOutput(update.rawOutput);
	const latestOutput =
		terminal.output ||
		rawResult.output ||
		acpToolOutput(update.content) ||
		(typeof update.rawOutput === "string"
			? update.rawOutput
			: update.rawOutput === undefined
				? ""
				: JSON.stringify(update.rawOutput));
	const previousOutput =
		typeof previousState?.output === "string"
			? previousState.output
			: previousState?.metadata &&
					typeof previousState.metadata === "object" &&
					typeof (previousState.metadata as Record<string, unknown>).output ===
						"string"
				? ((previousState.metadata as Record<string, unknown>).output as string)
				: "";
	const patchResult =
		tool === "apply_patch" ? openCodePatchResult(acpInput, update) : undefined;
	const output =
		patchResult?.output ??
		openCodeToolOutput(tool, latestOutput || previousOutput);
	const acpMetadata = {
		...previousAcpMetadata,
		toolCallId: callId,
		title,
		kind,
		...(update.locations !== undefined ? { locations: update.locations } : {}),
		...(update.rawInput !== undefined ? { rawInput: update.rawInput } : {}),
		...(update.rawOutput !== undefined ? { rawOutput: update.rawOutput } : {}),
		...(update.content !== undefined ? { content: update.content } : {}),
		...(update._meta !== undefined ? { _meta: update._meta } : {}),
	};
	const previousMetadata =
		previousState?.metadata && typeof previousState.metadata === "object"
			? (previousState.metadata as Record<string, unknown>)
			: {};
	const metadata = openCodeToolMetadata(
		tool,
		acpInput,
		{
			...update,
			content: acpMetadata.content,
			_meta: acpMetadata._meta,
		},
		output,
		previousMetadata,
	);
	const status = String(
		update.status ??
			(previousState?.status === "running"
				? "in_progress"
				: previousState?.status === "pending"
					? "in_progress"
				: previousState?.status === "error"
					? "failed"
					: (previousState?.status ?? "pending")),
	);
	const interrupted =
		job.cancelled &&
		/<shell_metadata>[\s\S]*\b(?:user aborted the command|command (?:was )?interrupted)\b[\s\S]*<\/shell_metadata>/i.test(
			output,
		);
	let toolState: Record<string, unknown>;
	if (interrupted) {
		toolState = {
			status: "completed",
			input,
			output,
			title: displayTitle,
			metadata: { ...metadata, interrupted: true },
			time: { start: started, end: timestamp },
		};
	} else if (status === "completed") {
		const completedOutput =
			tool === "write"
				? "Wrote file successfully."
				: tool === "edit"
					? "Edit applied successfully."
					: output;
		toolState = {
			status: "completed",
			input,
			output: completedOutput,
			title: tool === "apply_patch" ? completedOutput : displayTitle,
			metadata,
			time: { start: started, end: timestamp },
		};
	} else if (status === "failed") {
		toolState = {
			status: "error",
			input,
			error: output || `${title} failed`,
			metadata,
			time: { start: started, end: timestamp },
		};
	} else if (status === "in_progress") {
		toolState = {
			status: "running",
			input,
			metadata,
			time: { start: started },
		};
	} else {
		toolState = { status: "pending", input: {}, raw: JSON.stringify(input) };
	}
	if (!part) {
		part = {
			id: ascendingOpenCodeId("prt"),
			sessionID: meta.id,
			messageID: job.assistant.info.id,
			type: "tool",
			callID: callId,
			tool,
			state: toolState,
			metadata: { ...previousPartMetadata, acp: acpMetadata },
		};
		job.assistant.parts.push(part);
	} else {
		part.tool = tool;
		part.state = toolState;
		part.metadata = { ...previousPartMetadata, acp: acpMetadata };
	}
	state.emit({
		type: "message.part.updated",
		properties: { sessionID: meta.id, part, time: timestamp },
	});
}

function applySessionEvent(
	state: CompatState,
	meta: SessionMeta,
	job: PromptJob,
	payload: unknown,
): void {
	if (!payload || typeof payload !== "object") return;
	const wrapper = payload as Record<string, unknown>;
	if (
		typeof wrapper.sessionId === "string" &&
		wrapper.sessionId !== meta.actorSessionId
	)
		return;
	const sequence =
		typeof wrapper.sequence === "number" ? wrapper.sequence : undefined;
	if (sequence !== undefined) {
		job.appliedActorSequences ??= new Set();
		if (job.appliedActorSequences.has(sequence)) return;
		job.appliedActorSequences.add(sequence);
		job.lastActorSequence = Math.max(job.lastActorSequence ?? 0, sequence);
		meta.actorSequence = Math.max(meta.actorSequence ?? 0, sequence);
	}
	const event =
		wrapper.event && typeof wrapper.event === "object"
			? (wrapper.event as Record<string, unknown>)
			: undefined;
	const params =
		event?.params && typeof event.params === "object"
			? (event.params as Record<string, unknown>)
			: undefined;
	const legacyUpdate =
		params?.update && typeof params.update === "object"
			? (params.update as Record<string, unknown>)
			: undefined;
	const update =
		event?.method === "session/update"
			? legacyUpdate
			: typeof wrapper.type === "string"
				? wrapper
				: undefined;
	if (!update) return;
	switch (update.sessionUpdate ?? update.type) {
		case "agent_message_chunk": {
			job.pendingStreamChunks ??= [];
			const delta = reconcileSessionStreamDelta(
				job.pendingStreamChunks,
				wrapper,
				update,
				"text",
				acpContentText(update.content),
			);
			updateStreamingPart(
				state,
				meta,
				job,
				"text",
				delta,
			);
			break;
		}
		case "agent_thought_chunk": {
			job.pendingStreamChunks ??= [];
			const delta = reconcileSessionStreamDelta(
				job.pendingStreamChunks,
				wrapper,
				update,
				"reasoning",
				acpContentText(update.content),
			);
			updateStreamingPart(
				state,
				meta,
				job,
				"reasoning",
				delta,
			);
			break;
		}
		case "tool_call":
		case "tool_call_update":
			if (update.status === "completed" || update.status === "failed") {
				const callId =
					typeof update.toolCallId === "string" ? update.toolCallId : undefined;
				const pending = job.assistant.parts.find(
					(part) =>
						part.type === "tool" &&
						part.callID === callId &&
						part.state &&
						typeof part.state === "object" &&
						(part.state as Record<string, unknown>).status === "pending",
				);
				if (pending) {
					updateStreamingTool(state, meta, job, {
						toolCallId: callId,
						status: "in_progress",
					});
				}
			}
			updateStreamingTool(state, meta, job, update);
			break;
	}
}

function recordPermissionEvent(
	state: CompatState,
	meta: SessionMeta,
	job: PromptJob,
	payload: unknown,
): void {
	if (!payload || typeof payload !== "object") return;
	const event = payload as Record<string, unknown>;
	if (
		event.type !== "permission_request" ||
		event.sessionId !== meta.actorSessionId ||
		typeof event.requestId !== "string"
	) {
		return;
	}
	const toolCall =
		event.toolCall && typeof event.toolCall === "object"
			? (event.toolCall as Record<string, unknown>)
			: {};
	const options = Array.isArray(event.options)
		? event.options.filter(
				(option): option is Record<string, unknown> =>
					Boolean(option) && typeof option === "object",
			)
		: [];
	const permission: PermissionRecord = {
		id: id("per"),
		acpPermissionId: event.requestId,
		sessionID: meta.id,
		actorSessionId: meta.actorSessionId as string,
		messageID: job.assistant.info.id as string,
		createdAt: now(),
		description:
			typeof toolCall.title === "string"
				? toolCall.title
				: typeof toolCall.kind === "string"
					? toolCall.kind
					: "AgentOS permission",
		options,
		params: event,
	};
	state.permissions.set(permission.id, permission);
	state.emit({
		type: "permission.asked",
		properties: {
			id: permission.id,
			sessionID: meta.id,
			permission: permission.description,
			patterns: ["*"],
			metadata: permission.params,
			always: [],
		},
	});
}

function permissionOptionId(
	permission: PermissionRecord,
	reply: "always" | "once" | "reject",
): string {
	const preferredKinds =
		reply === "always"
			? ["allow_always", "allow_once"]
			: reply === "once"
				? ["allow_once", "allow_always"]
				: ["reject_once", "reject_always"];
	for (const kind of preferredKinds) {
		const option = permission.options.find((candidate) => candidate.kind === kind);
		if (typeof option?.optionId === "string") return option.optionId;
	}
	throw new Error(
		`ACP permission ${permission.acpPermissionId} did not offer an option for ${reply}`,
	);
}

async function resolveSessionWorkspace(
	state: CompatState,
	runtime: RivetRuntime,
	meta: SessionMeta,
	log: Logger,
): Promise<{ handle: GigacodeHandle; actorId: string }> {
	const previousActorId = runtimeActorId(meta);
	const resolved = await runtime.workspace(meta.directory);
	if (resolved.actorId === previousActorId) return resolved;

	await runtime.resetConnection(meta.directory);
	for (const session of state.sessions.values()) {
		if (
			session.directory !== meta.directory ||
			runtimeActorId(session) !== previousActorId
		) {
			continue;
		}
		session.actorId = resolved.actorId;
		session.needsResume = Boolean(session.actorSessionId);
		// Durable event sequences are actor-local. Re-read the restored session's
		// cursor before the next prompt instead of carrying a cursor across actors.
		session.actorSequence = undefined;
		session.updatedAt = now();
		await runtime.saveSession(session);
	}
	log.info({
		event: "rivet.actor.resolved_changed",
		previousActorId,
		actorId: resolved.actorId,
	});
	return resolved;
}

async function resolveActiveSessionControlHandle(
	runtime: RivetRuntime,
	meta: SessionMeta,
): Promise<GigacodeHandle> {
	// Permission replies and cancellation are control-lane operations for a
	// prompt that is already in flight. Do not put listMounts or any other normal
	// actor action in front of them: that action queues behind the blocked prompt
	// and creates a deadlock. The mount was established before the prompt began.
	const resolved = await runtime.resolvedWorkspace(meta.directory);
	const expectedActorId = runtimeActorId(meta);
	if (resolved.actorId !== expectedActorId) {
		throw new Error(
			`workspace actor changed during the active turn (${expectedActorId} -> ${resolved.actorId})`,
		);
	}
	// The ordinary handle sends an HTTP action, which Rivet serializes behind the
	// in-flight prompt. Permission replies and cancellation must travel over the
	// actor connection so they can reach that prompt's control lane.
	return await runtime.connection(meta.directory);
}

async function createActorSession(
	handle: GigacodeHandle,
	harness: Harness,
	directory: string,
	log: Logger,
): Promise<string> {
	const started = performance.now();
	const actorSessionId = id("acp");
	try {
		await handle.openSession({
			sessionId: actorSessionId,
			agent: harness,
			cwd: WORKSPACE_MOUNT_PATH,
			env: harnessEnvironment(harness),
			permissionPolicy: "ask",
			additionalInstructions: localSandboxInstructions(directory),
		});
		log.info({
			event: "agentos.session.created",
			durationMs: performance.now() - started,
			actorSessionId,
		});
		return actorSessionId;
	} catch (error) {
		log.error(
			{
				event: "agentos.session.create_failed",
				durationMs: performance.now() - started,
				err: error,
				detail: detailedErrorMessage(error),
			},
			"ACP session creation failed",
		);
		throw error;
	}
}

async function generateSessionTitle(
	state: CompatState,
	runtime: RivetRuntime,
	handle: GigacodeHandle,
	meta: SessionMeta,
	job: PromptJob,
): Promise<void> {
	const log = sessionLog(meta.id);
	const started = performance.now();
	const titleSessionId = id("title");
	let opened = false;
	try {
		await handle.openSession({
			sessionId: titleSessionId,
			agent: job.harness,
			cwd: WORKSPACE_MOUNT_PATH,
			env: harnessEnvironment(job.harness),
			permissionPolicy: "reject_all",
			skipOsInstructions: true,
			additionalInstructions: TITLE_GENERATOR_INSTRUCTIONS,
		});
		opened = true;
		if (!job.requestedModelIsDefault && job.requestedModelConfigId) {
			await handle.setSessionConfigOption({
				sessionId: titleSessionId,
				configId: job.requestedModelConfigId,
				value: job.requestedModel,
			});
		}
		if (job.requestedVariant) {
			await handle.setSessionConfigOption({
				sessionId: titleSessionId,
				configId: job.requestedVariant.configId,
				value: job.requestedVariant.value,
			});
		}
		await handle.prompt({
			sessionId: titleSessionId,
			idempotencyKey: id("title-prompt"),
			content: [
				{
					type: "text",
					text: `Generate a title for this conversation:\n\n${job.text}`,
				},
			],
		});
		let cursor = 0;
		let generated = "";
		for (;;) {
			const page = await handle.readHistory({
				sessionId: titleSessionId,
				after: cursor,
				limit: 1_000,
			});
			for (const event of page?.events ?? []) {
				generated += agentMessageText(event);
				if (typeof event.sequence === "number") {
					cursor = Math.max(cursor, event.sequence);
				}
			}
			if (!page?.hasMoreAfter) break;
		}
		const title = cleanGeneratedSessionTitle(generated);
		if (
			!title ||
			!state.sessions.has(meta.id) ||
			!isDefaultSessionTitle(meta.title)
		) {
			return;
		}
		meta.title = title;
		await runtime.saveSession(meta);
		state.emit({
			type: "session.updated",
			properties: { sessionID: meta.id, info: sessionValue(meta) },
		});
		log.info({
			event: "session.title.generated",
			durationMs: performance.now() - started,
			title,
		});
	} catch (error) {
		log.error(
			{
				event: "session.title.generation_failed",
				durationMs: performance.now() - started,
				err: error,
			},
			"Gigacode session title generation failed",
		);
	} finally {
		if (opened) {
			await handle.deleteSession({ sessionId: titleSessionId }).catch((error: unknown) =>
				log.error(
					{ event: "session.title.cleanup_failed", err: error },
					"Gigacode title session cleanup failed",
				),
			);
		}
	}
}

async function resumeActorSession(
	runtime: RivetRuntime,
	handle: GigacodeHandle,
	meta: SessionMeta,
	harness: Harness,
	log: Logger,
): Promise<void> {
	const persistedSessionId = meta.actorSessionId;
	if (!persistedSessionId || !meta.needsResume) return;
	if (harness === "pi") await preparePiSession(handle);
	const started = performance.now();
	await handle.openSession({
		sessionId: persistedSessionId,
		agent: harness,
		cwd: WORKSPACE_MOUNT_PATH,
		env: harnessEnvironment(harness),
		permissionPolicy: "ask",
		additionalInstructions: localSandboxInstructions(meta.directory),
	});
	meta.needsResume = false;
	meta.updatedAt = now();
	await runtime.saveSession(meta);
	log.info({
		event: "agentos.session.resumed",
		durationMs: performance.now() - started,
		persistedActorSessionId: persistedSessionId,
		actorSessionId: persistedSessionId,
		mode: "durable",
	});
}

async function runPromptInActor(
	state: CompatState,
	runtime: RivetRuntime,
	meta: SessionMeta,
	job: PromptJob,
) {
	const {
		text,
		harness,
		requestedModel,
		requestedModelIsDefault,
		requestedModelConfigId,
		requestedVariant,
	} = job;
	if (meta.harness && meta.harness !== harness) {
		throw new Error(
			`Gigacode session ${meta.id} uses the ${meta.harness} ACP harness and cannot switch to ${harness}; create a new session for ${harness}`,
		);
	}
	const cancellationBarrier = state.cancellationBarriers.get(meta.id);
	if (cancellationBarrier) {
		try {
			await cancellationBarrier;
		} finally {
			if (state.cancellationBarriers.get(meta.id) === cancellationBarrier) {
				state.cancellationBarriers.delete(meta.id);
			}
		}
	}
	meta.harness = harness;
	meta.updatedAt = now();
	const log = sessionLog(meta.id);
	const workspace = await resolveSessionWorkspace(state, runtime, meta, log);
	const handle = workspace.handle;
	const promptStarted = performance.now();
	log.info({
		event: "prompt.received",
		harness,
		textBytes: Buffer.byteLength(text),
	});
	if (!meta.actorSessionId) {
		if (harness === "pi") {
			const started = performance.now();
			await preparePiSession(handle);
			log.info({
				event: "pi.configuration.prepared",
				durationMs: performance.now() - started,
			});
		}
		meta.actorSessionId = await createActorSession(
			handle,
			harness,
			meta.directory,
			log,
		);
		meta.actorSequence = undefined;
		meta.needsResume = false;
		await runtime.saveSession(meta);
	} else {
		await resumeActorSession(runtime, handle, meta, harness, log);
	}
	const firstModelSelection = meta.model === undefined;
	if (
		!meta.model &&
		(requestedModel === "default" || requestedModelIsDefault)
	) {
		meta.model = requestedModel;
		await runtime.saveSession(meta);
		log.info({
			event: "agentos.session.model.selected",
			model: requestedModel,
			durationMs: 0,
			via: "agent-default",
		});
	} else if (meta.model !== requestedModel) {
		if (!requestedModelConfigId) {
			throw new Error(
				`${harness} did not expose a model configuration selector`,
			);
		}
		const modelSelectionStarted = performance.now();
		await handle.setSessionConfigOption({
			sessionId: meta.actorSessionId as string,
			configId: requestedModelConfigId,
			value: requestedModel,
		});
		meta.model = requestedModel;
		meta.variant = undefined;
		await runtime.saveSession(meta);
		log.info({
			event: "agentos.session.model.selected",
			model: requestedModel,
			durationMs: performance.now() - modelSelectionStarted,
		});
	}
	if (requestedVariant && meta.variant !== requestedVariant.id) {
		await handle.setSessionConfigOption({
			sessionId: meta.actorSessionId as string,
			configId: requestedVariant.configId,
			value: requestedVariant.value,
		});
		meta.variant = requestedVariant.id;
		await runtime.saveSession(meta);
		log.info({
			event: "agentos.session.variant.selected",
			variant: requestedVariant.id,
			configId: requestedVariant.configId,
			value: requestedVariant.value,
		});
	}
	if (firstModelSelection) {
		state.emit({
			type: "session.updated",
			properties: { sessionID: meta.id, info: sessionValue(meta) },
		});
	}
	if (job.cancelled) throw new TurnCancelledError();
	const userId = job.user.info.id as string;
	const assistantId = job.assistant.info.id as string;
	if (meta.actorSequence === undefined) {
		const actorSession = await handle.getSession({
			sessionId: meta.actorSessionId,
		});
		meta.actorSequence =
			typeof actorSession?.latestSequence === "number"
				? actorSession.latestSequence
				: 0;
	}
	job.actorSequenceBaseline = meta.actorSequence;
	job.lastActorSequence = meta.actorSequence;
	job.appliedActorSequences = new Set();
	const unsubscribeSessionEvents = await runtime.subscribeEvent(
		meta.directory,
		"sessionEvent",
		(payload: unknown) => {
			try {
				recordPermissionEvent(state, meta, job, payload);
				applySessionEvent(state, meta, job, payload);
			} catch (error) {
				log.error(
					{ event: "agentos.session_event.invalid", err: error },
					"Failed to translate an AgentOS session event",
				);
			}
		},
	);
	const promptRequestId = id("prompt");
	try {
		const sendStarted = performance.now();
		log.info({
			event: "agentos.prompt.started",
			actorSessionId: meta.actorSessionId,
		});
		// AgentOS resolves this action only after the prompt's terminal result and
		// ordered durable events have committed. That result is also cancellation's
		// quiescence barrier; do not replace it with an acknowledgement-only event.
		const prompt = handle.prompt({
			sessionId: meta.actorSessionId,
			idempotencyKey: promptRequestId,
			content: [{ type: "text", text }],
		});
		if (job.autoTitle) {
			void generateSessionTitle(state, runtime, handle, meta, job);
		}
		const result = await prompt;
		if (job.cancelled) throw new TurnCancelledError();
		// Rivet action responses and broadcasts use separate transports. The ACP
		// prompt result is ordered after its durable SQLite commits, but can reach
		// us before the corresponding live broadcasts. Replay only the missing
		// committed range before finalizing the OpenCode assistant message.
		let historyCursor = job.actorSequenceBaseline ?? 0;
		for (;;) {
			const page = await handle.readHistory({
				sessionId: meta.actorSessionId,
				after: historyCursor,
				limit: 1_000,
			});
			for (const event of page?.events ?? []) {
				applySessionEvent(state, meta, job, event);
			}
			if (!page?.hasMoreAfter) break;
			const nextCursor = Math.max(
				...((page?.events ?? []).map((event: Record<string, unknown>) =>
					typeof event.sequence === "number" ? event.sequence : historyCursor,
				)),
			);
			if (nextCursor <= historyCursor) {
				throw new Error(
					`AgentOS history pagination did not advance after sequence ${historyCursor}`,
				);
			}
			historyCursor = nextCursor;
		}
		// Some ACP adapters finish a turn after publishing tool output without a
		// terminal tool_call_update. The prompt completion event is ordered after
		// every session event, so a running tool with output is complete here.
		for (const part of job.assistant.parts) {
			if (part.type !== "tool" || !part.state || typeof part.state !== "object") {
				continue;
			}
			const toolState = part.state as Record<string, unknown>;
			const metadata =
				toolState.metadata && typeof toolState.metadata === "object"
					? (toolState.metadata as Record<string, unknown>)
					: undefined;
			if (
				toolState.status === "running" &&
				(typeof toolState.output === "string" ||
					typeof metadata?.output === "string")
			) {
				updateStreamingTool(state, meta, job, {
					toolCallId: part.callID,
					status: "completed",
				});
			}
		}
		const completed = now();
		const info = job.assistant.info;
		Object.assign(info, assistantInfo(meta, assistantId, userId, completed));
		let part = job.assistant.parts.find(
			(candidate) => candidate.type === "text",
		);
		const streamedText = typeof part?.text === "string" ? part.text : "";
		// ACP text is primarily delivered through ordered session/update events.
		// Some adapters (notably OpenCode) leave the aggregate prompt result empty,
		// so never erase text that has already arrived on the event stream.
		const aggregateText = Array.isArray(result?.message?.content)
			? result.message.content.map(acpContentText).join("")
			: "";
		const finalText = aggregateText || streamedText;
		log.info({
			event: "agentos.prompt.completed",
			durationMs: performance.now() - sendStarted,
			responseBytes: Buffer.byteLength(finalText),
		});
		const delta = finalText.startsWith(streamedText)
			? finalText.slice(streamedText.length)
			: undefined;
		if (!part) {
			part = {
				id: ascendingOpenCodeId("prt"),
				sessionID: meta.id,
				messageID: assistantId,
				type: "text",
				text: finalText,
				time: { start: completed, end: completed },
			};
			job.assistant.parts.push(part);
		} else {
			part.text = finalText;
			part.time = {
				...((part.time as Record<string, unknown> | undefined) ?? {
					start: completed,
				}),
				end: completed,
			};
		}
		for (const reasoning of job.assistant.parts.filter(
			(candidate) => candidate.type === "reasoning",
		)) {
			reasoning.time = {
				...((reasoning.time as Record<string, unknown> | undefined) ?? {
					start: completed,
				}),
				end: completed,
			};
		}
		state.emit({
			type: "message.updated",
			properties: { info, sessionID: meta.id },
		});
		if (delta) {
			state.emit({
				type: "message.part.delta",
				properties: {
					sessionID: meta.id,
					messageID: assistantId,
					partID: part.id,
					field: "text",
					delta,
				},
			});
		}
		state.emit({
			type: "message.part.updated",
			properties: {
				sessionID: meta.id,
				part,
				time: completed,
			},
		});
		log.info({
			event: "prompt.completed",
			durationMs: performance.now() - promptStarted,
			messageId: assistantId,
		});
		return job.assistant;
	} catch (error) {
		if (!(error instanceof TurnCancelledError)) {
			log.error(
				{
					event: "prompt.failed",
					durationMs: performance.now() - promptStarted,
					err: error,
				},
				"Gigacode prompt failed",
			);
		}
		throw error;
	} finally {
		delete job.abortAfterSessionClose;
		unsubscribeSessionEvents();
	}
}

async function runPrompt(
	state: CompatState,
	runtime: RivetRuntime,
	meta: SessionMeta,
	job: PromptJob,
) {
	return await runtime.withActorTurn(meta.directory, () =>
		runPromptInActor(state, runtime, meta, job),
	);
}

async function runShellCommand(
	state: CompatState,
	runtime: RivetRuntime,
	meta: SessionMeta,
	body: Record<string, unknown>,
) {
	if (state.promptQueues.has(meta.id) || state.activeShells.has(meta.id)) {
		throw new Error("session is busy");
	}
	if (typeof body.command !== "string" || !body.command.trim()) {
		throw new Error("shell command must be a non-empty string");
	}
	if (typeof body.agent !== "string" || !body.agent) {
		throw new Error("shell agent must be a non-empty string");
	}
	meta.updatedAt = now();

	const command = body.command;
	const displayHarness = meta.harness ?? "claude";
	const started = now();
	const userId =
		typeof body.messageID === "string"
			? body.messageID
			: ascendingOpenCodeId("msg");
	const userPart = {
		id: ascendingOpenCodeId("prt"),
		sessionID: meta.id,
		messageID: userId,
		type: "text",
		text: "The following tool was executed by the user",
		synthetic: true,
	};
	const user: OpenCodeMessage = {
		info: {
			id: userId,
			sessionID: meta.id,
			role: "user",
			time: { created: started },
			agent: body.agent,
			model: { providerID: displayHarness, modelID: "default" },
		},
		parts: [userPart],
	};

	const assistantId = ascendingOpenCodeId("msg");
	const info = assistantInfo(meta, assistantId, userId) as Record<
		string,
		unknown
	>;
	delete info.finish;
	const toolPart: Record<string, unknown> = {
		id: ascendingOpenCodeId("prt"),
		sessionID: meta.id,
		messageID: assistantId,
		type: "tool",
		tool: "bash",
		callID: id("call"),
		state: {
			status: "running",
			time: { start: started },
			input: { command },
		},
	};
	const assistant: OpenCodeMessage = { info, parts: [toolPart] };
	const messages = state.messages.get(meta.id) ?? [];
	if (messages.length + 2 > MAX_MESSAGES_PER_SESSION) {
		throw new Error(
			`session reached ${MAX_MESSAGES_PER_SESSION} messages; create a new session or raise GIGACODE_MAX_MESSAGES_PER_SESSION`,
		);
	}
	messages.push(user, assistant);
	state.messages.set(meta.id, messages);
	state.activeShells.add(meta.id);
	state.emit({
		type: "message.updated",
		properties: { info: user.info, sessionID: meta.id },
	});
	state.emit({ type: "message.part.updated", properties: { part: userPart } });
	state.emit({
		type: "message.updated",
		properties: { info, sessionID: meta.id },
	});
	state.emit({
		type: "message.part.updated",
		properties: { part: toolPart },
	});

	const log = sessionLog(meta.id);
	const executionStarted = performance.now();
	log.info({
		event: "agentos.shell.started",
		commandBytes: Buffer.byteLength(command),
	});
	await resolveSessionWorkspace(state, runtime, meta, log);
	const connection = await runtime.connection(meta.directory);
	const processOutput = new Map<
		number,
		{ stdout: Buffer[]; stderr: Buffer[] }
	>();
	const unsubscribeProcessOutput = await runtime.subscribeEvent(
		meta.directory,
		"processOutput",
		(payload: unknown) => {
			if (!payload || typeof payload !== "object") return;
			const value = payload as Record<string, unknown>;
			if (typeof value.pid !== "number") return;
			const chunks = processOutput.get(value.pid) ?? { stdout: [], stderr: [] };
			let data: Buffer;
			try {
				data = Buffer.from(value.data as Uint8Array);
			} catch (error) {
				log.error(
					{ event: "agentos.shell.output_invalid", err: error },
					"Failed to decode shell output",
				);
				return;
			}
			if (value.stream === "stderr") chunks.stderr.push(data);
			else chunks.stdout.push(data);
			processOutput.set(value.pid, chunks);
		},
	);
	try {
		await Promise.all([runtime.saveSession(meta), saveMessages(state)]);
		const spawned = await connection.spawn("sh", ["-lc", command], {
			cwd: WORKSPACE_MOUNT_PATH,
			env: SESSION_ENV,
		});
		state.activeShellPids.set(meta.id, spawned.pid);
		state.statuses.set(meta.id, "busy");
		state.emit({
			type: "session.status",
			properties: { sessionID: meta.id, status: { type: "busy" } },
		});
		const exitCode = await connection.waitProcess(spawned.pid);
		const chunks = processOutput.get(spawned.pid) ?? {
			stdout: [],
			stderr: [],
		};
		const result = {
			stdout: Buffer.concat(chunks.stdout).toString("utf8"),
			stderr: Buffer.concat(chunks.stderr).toString("utf8"),
			exitCode,
		};
		const completed = now();
		const output = `${result.stdout}${result.stderr}`;
		info.time = { created: started, completed };
		if (result.exitCode === 0) {
			info.finish = "stop";
			toolPart.state = {
				status: "completed",
				time: { start: started, end: completed },
				input: { command },
				title: command,
				metadata: { output, exit: result.exitCode, truncated: false },
				output,
			};
		} else {
			info.finish = "error";
			info.error = structuredUnknownError(
				new Error(`shell command exited with status ${result.exitCode}`),
			);
			toolPart.state = {
				status: "error",
				time: { start: started, end: completed },
				input: { command },
				metadata: { output, exit: result.exitCode, truncated: false },
				error: output || `Command exited with status ${result.exitCode}`,
			};
		}
		state.emit({
			type: "message.updated",
			properties: { info, sessionID: meta.id },
		});
		state.emit({
			type: "message.part.updated",
			properties: { part: toolPart },
		});
		log.info({
			event: "agentos.shell.completed",
			durationMs: performance.now() - executionStarted,
			exitCode: result.exitCode,
			outputBytes: Buffer.byteLength(output),
		});
		return assistant;
	} catch (error) {
		const completed = now();
		info.time = { created: started, completed };
		info.finish = "error";
		toolPart.state = {
			status: "error",
			time: { start: started, end: completed },
			input: { command },
			error: error instanceof Error ? error.message : String(error),
		};
		state.emit({
			type: "message.updated",
			properties: { info, sessionID: meta.id },
		});
		state.emit({
			type: "message.part.updated",
			properties: { part: toolPart },
		});
		log.error(
			{
				event: "agentos.shell.failed",
				durationMs: performance.now() - executionStarted,
				err: error,
			},
			"Gigacode shell command failed",
		);
		throw error;
	} finally {
		state.activeShellPids.delete(meta.id);
		state.activeShells.delete(meta.id);
		if (!state.promptQueues.has(meta.id) && state.sessions.has(meta.id)) {
			state.statuses.set(meta.id, "idle");
			state.emit({
				type: "session.status",
				properties: { sessionID: meta.id, status: { type: "idle" } },
			});
			state.emit({
				type: "session.idle",
				properties: { sessionID: meta.id },
			});
		}
		await saveMessages(state).catch((error) =>
			console.error(`failed to persist shell messages for ${meta.id}`, error),
		);
		unsubscribeProcessOutput();
	}
}

function openUrl(url: string) {
	const [command, args] =
		process.platform === "darwin"
			? ["open", [url]]
			: process.platform === "win32"
				? ["cmd", ["/c", "start", "", url]]
				: ["xdg-open", [url]];
	const child = spawn(command, args, { detached: true, stdio: "ignore" });
	child.once("error", (error) =>
		console.error(`failed to open Gigacode debugger URL ${url}`, error),
	);
	child.unref();
}

function debuggerUrl(actorId?: string) {
	const url = new URL(INSPECTOR_URL);
	url.searchParams.set("u", RIVET_ENDPOINT);
	if (actorId) url.searchParams.set("actorId", actorId);
	return url.toString();
}

async function serveCompat(
	state: CompatState,
	runtime: RivetRuntime,
	modelCatalog: GlobalModelCatalog,
) {
	const server = createServer(async (req, res) => {
		try {
			if (!req.url || !req.method)
				return errorJson(res, 400, "invalid request");
			if (req.method === "OPTIONS") return json(res, true);
			const url = new URL(req.url, API_ENDPOINT);
			const path = url.pathname.replace(/^\/opencode(?=\/|$)/, "") || "/";

			if (
				(path === "/global/health" || path === "/health") &&
				req.method === "GET"
			) {
				return json(res, {
					healthy: true,
					version: VERSION,
					rivetEndpoint: RIVET_ENDPOINT,
					workspaceRoot: WORKSPACE_MOUNT_PATH,
					rivetReady: runtime.isReady(),
					rivetStartupStage: runtime.startupStage(),
					modelCatalogStage: modelCatalog.startupStage(),
					workspaceMountPath: WORKSPACE_MOUNT_PATH,
				});
			}
			if (path === "/event" || path === "/global/event") {
				res.writeHead(200, {
					"content-type": "text/event-stream",
					"cache-control": "no-cache",
					connection: "keep-alive",
					"access-control-allow-origin": "*",
				});
				const global = path === "/global/event";
				const directory = directoryFor(req, url);
				const subscription = { global, directory };
				state.eventClients.set(res, subscription);
				const lastEventId = Number(req.headers["last-event-id"] ?? 0);
				const connected = {
					id: String(Math.max(0, state.nextId - 1)),
					type: "server.connected",
					properties: {},
				};
				res.write(
					`data: ${JSON.stringify(
						global ? { directory, payload: connected } : connected,
					)}\n\n`,
				);
				if (Number.isSafeInteger(lastEventId) && lastEventId > 0) {
					state.replay(res, subscription, lastEventId);
				}
				const heartbeat = setInterval(
					() => res.write(": heartbeat\n\n"),
					15_000,
				);
				req.on("close", () => {
					clearInterval(heartbeat);
					state.eventClients.delete(res);
				});
				return;
			}
			if (path === "/_gigacode/models/refresh" && req.method === "POST") {
				const refreshed = await modelCatalog.manualRefresh();
				return json(res, providerPayload(refreshed));
			}
			if (
				(path === "/provider" || path === "/config/providers") &&
				req.method === "GET"
			)
				return json(res, await modelCatalog.payload());
			if (path === "/provider/auth")
				return json(
					res,
					Object.fromEntries(Object.keys(harnesses).map((name) => [name, []])),
				);
			if (path === "/agent") {
				return json(res, [
					{
						name: "build",
						description: "AgentOS coding harness",
						mode: "primary",
						native: false,
						permission: [],
						options: {},
					},
				]);
			}
			const commands = [
				{
					name: "gigacode-debugger",
					description: "Open this AgentOS actor in the Rivet inspector",
					template: "",
					hints: [],
					source: "command",
				},
			];
			if (path === "/command") return json(res, commands);
			if (path === "/api/command") {
				const directory = directoryFor(req, url);
				return json(res, {
					location: locationValue(directory),
					data: commands.map(
						({ hints: _hints, source: _source, ...command }) => command,
					),
				});
			}
			if (path === "/api/fs/find" && req.method === "GET") {
				const directory = directoryFor(req, url);
				const query = url.searchParams.get("query");
				if (query === null) return errorJson(res, 400, "query is required");
				return json(res, {
					location: locationValue(directory),
					data: await findFileSystemEntries(
						directory,
						query,
						url.searchParams.get("type"),
						url.searchParams.get("limit"),
					),
				});
			}
			if (
				(path === "/config" || path === "/global/config") &&
				req.method === "GET"
			) {
				return json(res, {
					mcp: {},
					agent: {},
					provider: {},
					command: {},
					model: "claude/default",
					share: "disabled",
					username: process.env.USER ?? "gigacode",
				});
			}
			if (
				(path === "/config" || path === "/global/config") &&
				req.method === "PATCH"
			)
				return json(res, await readJson(req));
			if (path === "/path") {
				const directory = directoryFor(req, url);
				return json(res, {
					home: homedir(),
					state: STATE_DIR,
					config: STATE_DIR,
					worktree: directory,
					directory,
				});
			}
			if (path === "/vcs") return json(res, { branch: "main" });
			if (path === "/vcs/diff" || path === "/vcs/status") return json(res, []);
			if (path === "/vcs/diff/raw") return json(res, "");
			if (path === "/project" || path === "/project/current") {
				const directory = directoryFor(req, url);
				const project = {
					id: "gigacode-local",
					worktree: directory,
					vcs: "git",
					name: "gigacode",
					time: { created: now(), updated: now() },
				};
				return json(
					res,
					path === "/project" && req.method === "GET" ? [project] : project,
				);
			}
			const projectDirectoriesMatch = path.match(
				/^\/project\/([^/]+)\/directories$/,
			);
			if (projectDirectoriesMatch && req.method === "GET") {
				return json(res, [
					{ directory: directoryFor(req, url), strategy: "local" },
				]);
			}
			const projectCopyRefreshMatch = path.match(
				/^\/experimental\/project\/([^/]+)\/copy\/refresh$/,
			);
			if (projectCopyRefreshMatch && req.method === "POST") {
				return noContent(res);
			}

			if (path === "/session" && req.method === "GET") {
				await syncSessions(state, runtime);
				const directory = directoryFor(req, url);
				return json(
					res,
					[...state.sessions.values()]
						.filter((session) => session.directory === directory)
						.sort((left, right) => right.updatedAt - left.updatedAt)
						.map(sessionValue),
				);
			}
			if (path === "/session" && req.method === "POST") {
				await syncSessions(state, runtime);
				if (state.sessions.size >= MAX_SESSIONS)
					throw new Error(
						`session limit reached (${MAX_SESSIONS}); delete a session before creating another`,
					);
				const body = await readJson(req);
				const directory = directoryFor(req, url);
				const actorStarted = performance.now();
				const { actorId } = await runtime.workspace(directory);
				const actorDurationMs = performance.now() - actorStarted;
				const timestamp = now();
				const meta: SessionMeta = {
					id: id("ses"),
					actorId,
					directory,
					title:
						typeof body.title === "string"
							? body.title
							: `New session - ${new Date(timestamp).toISOString()}`,
					createdAt: timestamp,
					updatedAt: timestamp,
				};
				state.sessions.set(meta.id, meta);
				state.statuses.set(meta.id, "idle");
				await runtime.saveSession(meta);
				const log = sessionLog(meta.id);
				log.info({
					event: "rivet.actor.resolved",
					actorId,
					durationMs: actorDurationMs,
				});
				log.info({ event: "session.created" });
				state.emit({
					type: "session.created",
					properties: { sessionID: meta.id, info: sessionValue(meta) },
				});
				return json(res, sessionValue(meta));
			}
			if (path === "/session/status") {
				const directory = directoryFor(req, url);
				return json(
					res,
					Object.fromEntries(
						[...state.statuses]
							.filter(
								([key, value]) =>
									value === "busy" &&
									state.sessions.get(key)?.directory === directory,
							)
							.map(([key, value]) => [key, { type: value }]),
					),
				);
			}

			const sessionMatch = path.match(/^\/session\/([^/]+)(.*)$/);
			if (sessionMatch) {
				const sessionId = decodeURIComponent(sessionMatch[1] as string);
				const suffix = sessionMatch[2] as string;
				await syncSessions(state, runtime);
				const meta = state.sessions.get(sessionId);
				if (!meta) return errorJson(res, 404, "session not found");
				if (suffix === "" && req.method === "GET")
					return json(res, sessionValue(meta));
				if (suffix === "" && req.method === "PATCH") {
					const body = await readJson(req);
					if (typeof body.title === "string") meta.title = body.title;
					meta.updatedAt = now();
					await runtime.saveSession(meta);
					state.emit({
						type: "session.updated",
						properties: { info: sessionValue(meta) },
					});
					return json(res, sessionValue(meta));
				}
				if (suffix === "" && req.method === "DELETE") {
					await abortPromptQueue(state, runtime, meta);
					if (meta.actorSessionId && !meta.needsResume) {
						const { handle } = await resolveSessionWorkspace(
							state,
							runtime,
							meta,
							sessionLog(meta.id),
						);
					await handle.deleteSession({ sessionId: meta.actorSessionId });
					}
					state.emit({
						type: "session.deleted",
						properties: { sessionID: sessionId, info: sessionValue(meta) },
					});
					state.sessions.delete(sessionId);
					state.messages.delete(sessionId);
					state.statuses.delete(sessionId);
					state.cancellationBarriers.delete(sessionId);
					for (const [permissionId, permission] of state.permissions) {
						if (permission.sessionID === sessionId)
							state.permissions.delete(permissionId);
					}
					await Promise.all([
						runtime.deleteSession(sessionId),
						saveMessages(state),
					]);
					sessionLog(sessionId).info({ event: "session.deleted" });
					closeSessionLog(sessionId);
					return json(res, true);
				}
				if (suffix === "/message" && req.method === "GET") {
					const messages = state.messages.get(sessionId) ?? [];
					const requestedLimit = Number(url.searchParams.get("limit"));
					const limit =
						Number.isSafeInteger(requestedLimit) && requestedLimit > 0
							? Math.min(requestedLimit, MAX_MESSAGES_PER_SESSION)
							: messages.length;
					return json(res, messages.slice(-limit));
				}
				const messageMatch = suffix.match(/^\/message\/([^/]+)$/);
				if (messageMatch && req.method === "GET") {
					const messageId = decodeURIComponent(messageMatch[1] as string);
					const message = (state.messages.get(sessionId) ?? []).find(
						(item) => item.info.id === messageId,
					);
					return message
						? json(res, message)
						: errorJson(res, 404, "message not found");
				}
				if (suffix === "/init" && req.method === "POST") {
					const body = await readJson(req);
					const submitted = enqueuePrompt(state, runtime, modelCatalog, meta, {
						...body,
						agent: "build",
						parts: [
							{
								type: "text",
								text: "Analyze this project and create or update AGENTS.md with concise, project-specific instructions for coding agents.",
							},
						],
					});
					await submitted.accepted;
					void submitted.completion;
					return json(res, true);
				}
				if (suffix === "/command" && req.method === "POST") {
					const body = await readJson(req);
					if (body.command !== "gigacode-debugger") {
						return errorJson(
							res,
							400,
							`unsupported Gigacode command: ${String(body.command ?? "")}`,
						);
					}
					const completed = now();
					const messageId = ascendingOpenCodeId("msg");
					const parentId =
						typeof body.messageID === "string"
							? body.messageID
							: ascendingOpenCodeId("msg");
					const info = assistantInfo(meta, messageId, parentId, completed);
					const inspector = debuggerUrl(sessionId);
					const part = {
						id: ascendingOpenCodeId("prt"),
						sessionID: sessionId,
						messageID: messageId,
						type: "text",
						text: `Rivet inspector: ${inspector}`,
						time: { start: completed, end: completed },
					};
					const message = { info, parts: [part] };
					const messages = state.messages.get(sessionId) ?? [];
					messages.push(message);
					state.messages.set(sessionId, messages);
					state.emit({
						type: "command.executed",
						properties: {
							name: body.command,
							sessionID: sessionId,
							arguments:
								typeof body.arguments === "string" ? body.arguments : "",
							messageID: messageId,
						},
					});
					state.emit({
						type: "message.updated",
						properties: { sessionID: sessionId, info },
					});
					state.emit({
						type: "message.part.updated",
						properties: { sessionID: sessionId, part, time: completed },
					});
					await saveMessages(state);
					if (process.env.GIGACODE_DISABLE_OPEN_URL !== "1") openUrl(inspector);
					return json(res, message);
				}
				if (
					(suffix === "/message" || suffix === "/prompt_async") &&
					req.method === "POST"
				) {
					const body = await readJson(req);
					const submitted = enqueuePrompt(
						state,
						runtime,
						modelCatalog,
						meta,
						body,
					);
					await submitted.accepted;
					if (suffix === "/prompt_async") {
						return noContent(res);
					}
					return json(res, await submitted.completion);
				}
				if (suffix === "/shell" && req.method === "POST") {
					const body = await readJson(req);
					const message = await runShellCommand(state, runtime, meta, body);
					return json(res, message.info);
				}
				if (suffix === "/abort" && req.method === "POST") {
					return json(res, await abortPromptQueue(state, runtime, meta));
				}
				const legacyPermissionMatch = suffix.match(/^\/permissions\/([^/]+)$/);
				if (legacyPermissionMatch && req.method === "POST") {
					const permissionId = decodeURIComponent(
						legacyPermissionMatch[1] as string,
					);
					const permission = state.permissions.get(permissionId);
					if (!permission || permission.sessionID !== sessionId)
						return errorJson(res, 404, "permission not found");
					const body = await readJson(req);
					const reply =
						body.response === "always"
							? "always"
							: body.response === "once"
								? "once"
								: "reject";
					const handle = await resolveActiveSessionControlHandle(runtime, meta);
					await handle.respondPermission({
						sessionId: permission.actorSessionId,
						requestId: permission.acpPermissionId,
						optionId: permissionOptionId(permission, reply),
					});
					state.permissions.delete(permissionId);
					state.emit({
						type: "permission.replied",
						properties: {
							sessionID: sessionId,
							requestID: permissionId,
							reply,
						},
					});
					return json(res, true);
				}
				if (["/children", "/diff", "/todo"].includes(suffix))
					return json(res, []);
				if (suffix === "/summarize" && req.method === "POST")
					return errorJson(
						res,
						501,
						"AgentOS does not yet expose conversation compaction",
					);
			}

			if (path === "/permission")
				return json(
					res,
					[...state.permissions.values()].map((item) => ({
						id: item.id,
						sessionID: item.sessionID,
						permission: item.description ?? "AgentOS permission",
						patterns: ["*"],
						metadata: item.params,
						always: [],
					})),
				);
			const permissionMatch = path.match(/^\/permission\/([^/]+)\/reply$/);
			if (permissionMatch && req.method === "POST") {
				const permissionId = decodeURIComponent(permissionMatch[1] as string);
				const permission = state.permissions.get(permissionId);
				if (!permission) return errorJson(res, 404, "permission not found");
				const body = await readJson(req);
				const permissionSession = state.sessions.get(permission.sessionID);
				if (!permissionSession) return errorJson(res, 404, "session not found");
				const reply =
					body.reply === "always"
						? "always"
						: body.reply === "once"
							? "once"
							: "reject";
				const handle = await resolveActiveSessionControlHandle(
					runtime,
					permissionSession,
				);
				await handle.respondPermission({
					sessionId: permission.actorSessionId,
					requestId: permission.acpPermissionId,
					optionId: permissionOptionId(permission, reply),
				});
				state.permissions.delete(permissionId);
				state.emit({
					type: "permission.replied",
					properties: {
						sessionID: permission.sessionID,
						requestID: permissionId,
						reply,
					},
				});
				return json(res, true);
			}

			if (["/mcp", "/session/status"].includes(path)) return json(res, {});
			if (
				[
					"/lsp",
					"/formatter",
					"/skill",
					"/experimental/resource",
					"/question",
				].includes(path)
			)
				return json(res, []);
			if (
				path.startsWith("/tui/") ||
				path === "/global/dispose" ||
				path === "/instance/dispose"
			)
				return json(res, true);
			return errorJson(
				res,
				404,
				`unsupported OpenCode route: ${req.method} ${path}`,
			);
		} catch (error) {
			console.error("gigacode request failed", error);
			return errorJson(
				res,
				500,
				error instanceof Error ? error.message : String(error),
			);
		}
	});
	await new Promise<void>((resolve, reject) => {
		server.once("error", reject);
		server.listen(API_PORT, HOST, resolve);
	});
	return server;
}

async function writePid() {
	await mkdir(STATE_DIR, { recursive: true });
	await writeFile(PID_FILE, `${process.pid}\n`, { mode: 0o600 });
}

async function readPid(): Promise<number | undefined> {
	try {
		const value = Number((await readFile(PID_FILE, "utf8")).trim());
		return Number.isInteger(value) && value > 0 ? value : undefined;
	} catch (error) {
		if ((error as NodeJS.ErrnoException).code === "ENOENT") return undefined;
		throw error;
	}
}

function processExists(pid: number) {
	try {
		process.kill(pid, 0);
		return true;
	} catch (error) {
		const code = (error as NodeJS.ErrnoException).code;
		if (code === "ESRCH") return false;
		if (code === "EPERM") return true;
		throw error;
	}
}

async function gigacodeEnginePids(): Promise<number[]> {
	if (process.platform !== "linux") return [];
	const entries = await readdir("/proc", { withFileTypes: true }).catch(
		() => [],
	);
	const storageMarker = `RIVETKIT_STORAGE_PATH=${RIVET_STORAGE_PATH}`;
	const matches = await Promise.all(
		entries.flatMap((entry) => {
			if (!entry.isDirectory() || !/^\d+$/.test(entry.name)) return [];
			const pid = Number(entry.name);
			if (pid === process.pid) return [];
			return [
				Promise.all([
					readFile(`/proc/${pid}/comm`, "utf8"),
					readFile(`/proc/${pid}/environ`, "utf8"),
				])
					.then(([comm, environment]) => {
						const variables = environment.split("\0");
						return comm.trim() === "rivet-engine" &&
							variables.includes(storageMarker)
							? pid
							: undefined;
					})
					.catch(() => undefined),
			];
		}),
	);
	return matches.filter((pid): pid is number => pid !== undefined);
}

async function stopRivetEngine(): Promise<void> {
	let pids = await gigacodeEnginePids();
	for (const pid of pids) {
		try {
			process.kill(pid, "SIGTERM");
		} catch (error) {
			if ((error as NodeJS.ErrnoException).code !== "ESRCH") throw error;
		}
	}
	const deadline = Date.now() + 5_000;
	while (pids.some(processExists) && Date.now() < deadline) {
		await new Promise((resolve) => setTimeout(resolve, 100));
	}
	pids = pids.filter(processExists);
	for (const pid of pids) {
		try {
			process.kill(pid, "SIGKILL");
		} catch (error) {
			if ((error as NodeJS.ErrnoException).code !== "ESRCH") throw error;
		}
	}
}

type DaemonHealth = {
	healthy: true;
	version: string;
	rivetEndpoint: string;
	rivetReady?: boolean;
	rivetStartupStage?: string;
	modelCatalogStage?: string;
	workspaceRoot?: string;
};

async function daemonHealth(): Promise<DaemonHealth | undefined> {
	try {
		const response = await fetch(`${API_ENDPOINT}/global/health`, {
			signal: AbortSignal.timeout(1_000),
		});
		if (!response.ok) return undefined;
		return (await response.json()) as DaemonHealth;
	} catch (error) {
		if (process.env.GIGACODE_DEBUG)
			console.error("Gigacode health check failed", error);
		return undefined;
	}
}

async function healthy() {
	return Boolean(await daemonHealth());
}

async function printStartupLogUntil(
	startOffset: number,
	ready: Promise<Response>,
): Promise<void> {
	const log = await open(LOG_FILE, "r");
	let offset = startOffset;
	let pending = "";
	let settled = false;
	const settlement = ready.then(
		() => {
			settled = true;
		},
		() => {
			settled = true;
		},
	);
	const printAvailable = async () => {
		const { size } = await log.stat();
		while (offset < size) {
			const buffer = Buffer.allocUnsafe(Math.min(64 * 1024, size - offset));
			const { bytesRead } = await log.read(buffer, 0, buffer.length, offset);
			if (bytesRead === 0) break;
			offset += bytesRead;
			pending += buffer.subarray(0, bytesRead).toString("utf8");
			const lines = pending.split("\n");
			pending = lines.pop() ?? "";
			for (const line of lines) {
				if (line.startsWith("[gigacode]")) console.error(line);
			}
		}
	};
	try {
		while (!settled) {
			await printAvailable();
			await Promise.race([
				settlement,
				new Promise((resolve) => setTimeout(resolve, 100)),
			]);
		}
		await printAvailable();
		if (pending.startsWith("[gigacode]")) console.error(pending);
		const response = await ready;
		if (!response.ok) {
			throw new Error(
				`Gigacode provider bootstrap failed (${response.status}); see ${LOG_FILE}`,
			);
		}
		await response.arrayBuffer();
	} finally {
		await log.close();
	}
}

async function waitForDaemonHealth(
	expectedPid?: number,
): Promise<DaemonHealth> {
	const deadline = Date.now() + STARTUP_TIMEOUT_MS;
	while (Date.now() < deadline) {
		const health = await daemonHealth();
		if (health) return health;
		if (expectedPid && !processExists(expectedPid)) {
			throw new Error(`Gigacode daemon exited during startup; see ${LOG_FILE}`);
		}
		await new Promise((resolve) => setTimeout(resolve, 100));
	}
	throw new Error(`Gigacode daemon did not become healthy; see ${LOG_FILE}`);
}

async function waitForRivetRuntime(): Promise<Response> {
	const deadline = Date.now() + STARTUP_TIMEOUT_MS;
	while (Date.now() < deadline) {
		const health = await daemonHealth();
		if (health?.rivetReady) {
			return await fetch(`${API_ENDPOINT}/opencode/provider`, {
				signal: AbortSignal.timeout(1_000),
			});
		}
		const stage = health?.rivetStartupStage;
		if (stage?.startsWith("Rivet startup failed:")) {
			throw new Error(stage);
		}
		await new Promise((resolve) => setTimeout(resolve, 100));
	}
	throw new Error(
		`Gigacode Rivet runtime did not become ready within ${STARTUP_TIMEOUT_MS}ms; see ${LOG_FILE}`,
	);
}

async function startDaemon(
	detached: boolean,
	streamStartup = false,
): Promise<number | undefined> {
	const running = await daemonHealth();
	if (running) return;
	if (!detached) {
		await runDaemon();
		return;
	}
	await mkdir(STATE_DIR, { recursive: true });
	console.error(`[gigacode] starting the local daemon; log: ${LOG_FILE}`);
	const log = await open(LOG_FILE, "a", 0o600);
	const startupLogOffset = (await log.stat()).size;
	const script = process.argv[1];
	if (!script)
		throw new Error(
			"cannot autospawn Gigacode: entrypoint path is unavailable",
		);
	const child = spawn(
		process.execPath,
		[...process.execArgv, script, "daemon"],
		{
			detached: true,
			stdio: ["ignore", log.fd, log.fd],
			env: process.env,
		},
	);
	child.unref();
	await log.close();
	const healthReady = waitForDaemonHealth(child.pid);
	if (streamStartup) {
		const runtimeReady = healthReady.then(async (health) => {
			console.error(
				`[gigacode] OpenCode API is ready; ${health.rivetStartupStage ?? "Rivet startup is continuing in the background"}`,
			);
			return await waitForRivetRuntime();
		});
		await printStartupLogUntil(startupLogOffset, runtimeReady);
	} else {
		const health = await healthReady;
		console.error(
			`[gigacode] OpenCode API is ready; ${health.rivetStartupStage ?? "Rivet startup is continuing in the background"}`,
		);
	}
	return startupLogOffset;
}

async function runDaemon() {
	if (await healthy())
		throw new Error(`Gigacode daemon is already running at ${API_ENDPOINT}`);
	await writePid();
	let runtime: RivetRuntime | undefined;
	try {
		const state = new CompatState();
		const activeRuntime = new RivetRuntime();
		runtime = activeRuntime;
		const modelCatalog = new GlobalModelCatalog(activeRuntime);
		const runtimeStartup = activeRuntime.start();
		void runtimeStartup.catch((error) => {
			console.error("Gigacode Rivet runtime failed to initialize", error);
		});
		// A missing or invalid cache is the one automatic discovery path. Keep the
		// OpenCode-compatible API closed until it finishes so the TUI cannot race it.
		// A valid cache makes this return without probing any harness.
		await modelCatalog.start();
		const workspaceStarted = performance.now();
		startupLog("preparing the default workspace actor");
		await activeRuntime.connection(DEFAULT_DIRECTORY);
		startupLog("default workspace actor is ready", workspaceStarted);
		const server = await serveCompat(state, activeRuntime, modelCatalog);
		let requestShutdown: (() => void) | undefined;
		const shutdownRequested = new Promise<void>((resolve) => {
			requestShutdown = resolve;
		});
		const exitCleanly = () => {
			requestShutdown?.();
			requestShutdown = undefined;
		};
		process.once("SIGINT", exitCleanly);
		process.once("SIGTERM", exitCleanly);
		startupLog(`OpenCode API is listening at ${API_ENDPOINT}`);
		startupLog(`Rivet engine will listen at ${RIVET_ENDPOINT}`);
		console.warn(
			`[gigacode] each active host project is mounted read-write at ${WORKSPACE_MOUNT_PATH}; this is not a security boundary`,
		);
		await shutdownRequested;
		for (const meta of state.sessions.values()) {
			if (!state.promptQueues.has(meta.id)) continue;
			await abortPromptQueue(state, activeRuntime, meta).catch((error) =>
				console.error(`failed to cancel active turn for ${meta.id}`, error),
			);
		}
		if (state.messagesLoaded) {
			await saveMessages(state).catch((error) =>
				console.error("failed to persist Gigacode state", error),
			);
		}
		closeAllSessionLogs();
		server.close();
		server.closeAllConnections();
		await activeRuntime
			.shutdown()
			.catch((error) => console.error("failed to stop RivetKit", error));
		runtime = undefined;
		await rm(PID_FILE, { force: true });
		process.exit(0);
	} catch (error) {
		await runtime
			?.shutdown()
			.catch((shutdownError) =>
				console.error("failed to stop RivetKit after daemon startup", shutdownError),
			);
		throw error;
	} finally {
		await rm(PID_FILE, { force: true });
	}
}

async function stopDaemon() {
	const pid = await readPid();
	if (!pid || !processExists(pid)) {
		// A daemon can fail before writing its PID while its independently spawned
		// engine is still alive. The storage path uniquely identifies our engine.
		await stopRivetEngine();
		await rm(PID_FILE, { force: true });
		console.log("Gigacode daemon is not running");
		return;
	}
	process.kill(pid, "SIGTERM");
	const gracefulDeadline = Date.now() + RIVET_SHUTDOWN_TIMEOUT_MS + 6_000;
	while (processExists(pid) && Date.now() < gracefulDeadline) {
		await new Promise((resolve) => setTimeout(resolve, 100));
	}
	// RivetKit can let the daemon exit before its managed engine child is fully
	// reaped. Always verify teardown by this daemon's unique storage path instead
	// of assuming that a vanished parent implies a vanished engine.
	await stopRivetEngine();
	if (processExists(pid)) {
		try {
			process.kill(pid, "SIGKILL");
		} catch (error) {
			if ((error as NodeJS.ErrnoException).code !== "ESRCH") throw error;
		}
	}
	const forcedDeadline = Date.now() + 2_000;
	while (processExists(pid) && Date.now() < forcedDeadline) {
		await new Promise((resolve) => setTimeout(resolve, 50));
	}
	if (processExists(pid)) {
		throw new Error(`Gigacode daemon ${pid} did not stop`);
	}
	console.log(`Stopped Gigacode daemon (${pid})`);
}

async function refreshModels() {
	const wasRunning = Boolean(await daemonHealth());
	await startDaemon(true, true);
	const health = await daemonHealth();
	if (!wasRunning && health?.modelCatalogStage === "model catalog is ready") {
		console.log("Gigacode model catalog discovered during first startup");
		return;
	}

	await mkdir(STATE_DIR, { recursive: true });
	const log = await open(LOG_FILE, "a", 0o600);
	const refreshLogOffset = (await log.stat()).size;
	await log.close();
	console.error("[gigacode] manually refreshing the model catalog");
	await printStartupLogUntil(
		refreshLogOffset,
		fetch(`${API_ENDPOINT}/_gigacode/models/refresh`, {
			method: "POST",
			signal: AbortSignal.timeout(STARTUP_TIMEOUT_MS),
		}),
	);
	console.log("Gigacode model catalog refreshed");
}

async function runClient(args: string[]) {
	await startDaemon(true, true);
	const attachUrl = `${API_ENDPOINT}/opencode`;
	const configured = process.env.GIGACODE_OPENCODE_BIN;
	const command = configured ?? "opencode";
	const opencodeArgs =
		args[0] === "run"
			? ["run", "--attach", attachUrl, ...args.slice(1)]
			: ["attach", attachUrl, ...args];
	const child = spawn(command, opencodeArgs, {
		stdio: "inherit",
	});
	const status = await new Promise<number | null>((resolve, reject) => {
		child.once("error", reject);
		child.once("exit", resolve);
	}).catch(async (error: NodeJS.ErrnoException) => {
		if (!configured && error.code === "ENOENT") {
			const fallback = spawn("npx", ["--yes", "opencode-ai", ...opencodeArgs], {
				stdio: "inherit",
			});
			return await new Promise<number | null>((resolve, reject) => {
				fallback.once("error", reject);
				fallback.once("exit", resolve);
			});
		}
		throw error;
	});
	if (status !== 0) throw new Error(`OpenCode exited with status ${status}`);
}

async function runShell(args: string[]): Promise<number> {
	await startDaemon(true);
	for (let poll = 0; poll < 300; poll++) {
		if ((await daemonHealth())?.rivetReady) break;
		await new Promise((resolve) => setTimeout(resolve, 100));
	}
	if (!(await daemonHealth())?.rivetReady)
		throw new Error("Gigacode Rivet runtime did not become ready");
	const [{ createClient: createRivetClient }, { attachShell }] =
		await Promise.all([
			importModule("@rivet-dev/agentos/client"),
			importModule("@rivet-dev/agentos/node"),
		]);
	const client = createRivetClient({ endpoint: RIVET_ENDPOINT });
	const directory = canonicalDirectory(process.cwd());
	const handle = client.vm.getOrCreate(workspaceKey(directory));
	await handle.resolve();
	await ensureWorkspaceMount(handle, directory);
	const connection = handle.connect();
	const guestArgs = args[0] === "--" ? args.slice(1) : args;
	const command = guestArgs[0] ?? "bash";
	const commandArgs =
		guestArgs.length > 0
			? guestArgs.slice(1)
			: ["--input-backend", "minimal", "-i"];
	let shellError: unknown;
	let exitCode: number | undefined;

	try {
		exitCode = await attachShell(connection, {
			command,
			args: commandArgs,
			cwd: WORKSPACE_MOUNT_PATH,
			env: SESSION_ENV,
		});
	} catch (error) {
		shellError = error;
	}
	const cleanup = await Promise.allSettled([
		Promise.resolve().then(() => connection.dispose()),
	]);
	const failures = cleanup.flatMap((result) =>
		result.status === "rejected" ? [result.reason] : [],
	);
	if (shellError !== undefined) {
		for (const failure of failures) {
			console.error("Gigacode shell cleanup failed", failure);
		}
		throw shellError;
	}
	if (failures.length > 0) throw failures[0];
	if (exitCode === undefined) {
		throw new Error("Gigacode shell exited without an exit code");
	}
	return exitCode;
}

function usage() {
	console.log(
		`Gigacode ${VERSION}\n\nUsage:\n  gigacode [OpenCode args...]\n  gigacode shell [command [args...]]\n  gigacode daemon [start|status|stop]\n  gigacode models refresh\n  gigacode debugger [actorID]\n`,
	);
}

export async function main(argv = process.argv.slice(2)) {
	const [command, subcommand] = argv;
	if (command === "--help" || command === "-h" || command === "help")
		return usage();
	if (command === "--version" || command === "-V") return console.log(VERSION);
	if (command === "daemon") {
		if (subcommand === "start") return startDaemon(true);
		if (subcommand === "status") {
			let pid = await readPid();
			if (pid && !processExists(pid)) {
				await rm(PID_FILE, { force: true });
				pid = undefined;
			}
			const health = await daemonHealth();
			console.log(
				JSON.stringify(
					{
						running: Boolean(health),
						pid,
						apiEndpoint: API_ENDPOINT,
						rivetEndpoint: RIVET_ENDPOINT,
						rivetReady: health?.rivetReady ?? false,
						rivetStartupStage: health?.rivetStartupStage,
						modelCatalogStage: health?.modelCatalogStage,
						startupLog: LOG_FILE,
					},
					null,
					2,
				),
			);
			return;
		}
		if (subcommand === "stop") return stopDaemon();
		if (subcommand) throw new Error(`unknown daemon command: ${subcommand}`);
		return runDaemon();
	}
	if (command === "debugger") {
		await startDaemon(true);
		const url = debuggerUrl(subcommand);
		openUrl(url);
		console.log(url);
		return;
	}
	if (command === "models") {
		if (subcommand === "refresh") return refreshModels();
		throw new Error(
			subcommand
				? `unknown models command: ${subcommand}`
				: "missing models command; expected: refresh",
		);
	}
	if (command === "shell") {
		process.exitCode = await runShell(argv.slice(1));
		return;
	}
	return runClient(argv);
}

if (import.meta.url === pathToFileURL(process.argv[1] as string).href) {
	main().catch((error) => {
		console.error(error instanceof Error ? error.message : error);
		process.exit(1);
	});
}
